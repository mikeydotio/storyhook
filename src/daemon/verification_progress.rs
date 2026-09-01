//! The SH-524 verification progress publisher.
//!
//! Every story sitting in `verifying` carries one self-updating
//! `CENTRAL VERIFICATION PROGRESS —` comment: the candidate the release gate
//! is presently running for shows a live nested checklist read from its own
//! [`gate_progress`] journal; every other candidate shows its queue position
//! and wait. This publisher needs no shared state with
//! [`super::verification::poll_verification`] — it independently re-derives
//! "which candidate is active" from [`VerificationQueue::ordered`], the same
//! store-backed queue the verifier itself drains, because verification is
//! strictly serial and queue membership is already a story fact rather than
//! a second job record (`crate::service::verification`'s own module doc).

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::env::Environment;
use crate::error::AppError;
use crate::service::engine::elapsed_secs;
use crate::service::gate_progress::{self, GATE_PROGRESS_PREFIX, VerificationProgressView};
use crate::service::{Ctx, StoryService, VerificationCandidate, VerificationQueue};
use crate::store::Store;

use super::verification::journal_path;

/// The base cadence while a publish tick actually changes something — twice
/// the verifier's own recovery-wake cadence
/// ([`super::verification`]'s `RECOVERY_WAKE`, 30s), so a progress rewrite
/// never outruns the loop that could act on the story it describes.
pub const PUBLISH_INTERVAL: Duration = Duration::from_secs(60);

/// The silent-gate backoff ladder, in minutes: 1 → 2 → 5 → 10, an operator
/// determination of 2026-08-31. Comments are append-only events and every
/// write re-folds the story's whole history, so a strict 60s cadence on an
/// 8-hour wedge — the case this story was filed for — would be roughly 480
/// rewrites on one story; this caps it near 15 while a run is genuinely
/// silent, without slowing down a run that is actually progressing.
const SILENT_LADDER_MINUTES: [u64; 4] = [1, 2, 5, 10];

/// How long the next publish tick should wait, backing off through
/// [`SILENT_LADDER_MINUTES`] across consecutive ticks that write nothing and
/// resetting to [`PUBLISH_INTERVAL`] the instant one does.
pub struct PublishBackoff {
    step: usize,
}

impl Default for PublishBackoff {
    fn default() -> Self {
        Self::new()
    }
}

impl PublishBackoff {
    pub fn new() -> Self {
        Self { step: 0 }
    }

    /// Records whether the tick that just ran wrote anything, and returns
    /// how long to wait before the next one.
    pub fn advance(&mut self, moved: bool) -> Duration {
        if moved {
            self.step = 0;
            return PUBLISH_INTERVAL;
        }
        let wait = Duration::from_secs(SILENT_LADDER_MINUTES[self.step] * 60);
        if self.step + 1 < SILENT_LADDER_MINUTES.len() {
            self.step += 1;
        }
        wait
    }
}

/// How many of `ordered[..index]` sort strictly ahead of `ordered[index]` on
/// priority alone, versus on the equal-priority/older-creation tie-break.
///
/// Sound because `ordered` is already sorted the identical way
/// [`VerificationQueue::next`] drains it: nothing ahead of `index` can have a
/// *lower* priority than it, so the prefix splits cleanly into "strictly
/// higher priority" and "equal priority" — the latter necessarily created no
/// later, by the sort's own tie-break.
fn ahead_counts(ordered: &[VerificationCandidate], index: usize) -> (usize, usize) {
    let candidate = &ordered[index];
    let mut higher = 0;
    let mut equal_older = 0;
    for other in &ordered[..index] {
        if other.priority < candidate.priority {
            higher += 1;
        } else {
            equal_older += 1;
        }
    }
    (higher, equal_older)
}

/// The journal's own most recent timestamp, read straight from its raw
/// text without a full fold — the publisher's staleness signal needs only
/// this, not the whole tree.
fn last_event_at(journal_text: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Timestamped {
        at: Option<String>,
    }
    journal_text
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str::<Timestamped>(line.trim()).ok()?.at)
}

/// Runs one publish attempt across every story presently in `verifying`.
///
/// Returns whether anything was actually written — [`PublishBackoff`]'s own
/// signal for whether to keep the base cadence or start stretching it out.
pub fn publish_once(store: &impl Store, env: &Environment, now: &str) -> Result<bool, AppError> {
    let ordered = VerificationQueue::new(store).ordered()?;
    let mut moved = false;
    for (index, candidate) in ordered.iter().enumerate() {
        let ctx = Ctx::new(
            store,
            candidate.project,
            candidate.checkout.clone(),
            env.clone(),
        )
        .no_hooks(true);
        let body = if index == 0 {
            let journal = journal_path(env, candidate);
            let text = std::fs::read_to_string(&journal).unwrap_or_default();
            let progress = gate_progress::fold(&text);
            let elapsed_seconds = candidate
                .verifying_since
                .as_deref()
                .and_then(|since| elapsed_secs(since, now));
            let seconds_since_last_event = last_event_at(&text)
                .as_deref()
                .and_then(|at| elapsed_secs(at, now));
            gate_progress::render(
                &VerificationProgressView::Running {
                    progress: &progress,
                    elapsed_seconds,
                    seconds_since_last_event,
                },
                now,
            )
        } else {
            let (ahead_higher_priority, ahead_equal_priority_older) = ahead_counts(&ordered, index);
            gate_progress::render(
                &VerificationProgressView::Queued {
                    position: index + 1,
                    ahead_higher_priority,
                    ahead_equal_priority_older,
                },
                now,
            )
        };
        let (_, wrote) = StoryService::new(&ctx).upsert_marked_comment(
            &candidate.story_id,
            GATE_PROGRESS_PREFIX,
            &body,
        )?;
        moved |= wrote;
    }
    Ok(moved)
}

/// Runs the publisher until daemon shutdown, sleeping in short increments so
/// shutdown is prompt regardless of the current backoff step.
pub(crate) fn poll_verification_progress(store: &impl Store, env: &Environment, stop: &AtomicBool) {
    const SHUTDOWN_CHECK: Duration = Duration::from_millis(250);
    let mut backoff = PublishBackoff::new();
    while !stop.load(Ordering::Relaxed) {
        let moved = match publish_once(store, env, &env.now()) {
            Ok(moved) => moved,
            Err(error) => {
                eprintln!("storyhook: verification progress publish failed: {error}");
                false
            }
        };
        let wait_until = Instant::now() + backoff.advance(moved);
        while !stop.load(Ordering::Relaxed) {
            let remaining = wait_until.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            std::thread::sleep(remaining.min(SHUTDOWN_CHECK));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_backoff_stretches_through_the_ladder_while_silent_and_resets_on_any_move() {
        let mut backoff = PublishBackoff::new();
        assert_eq!(backoff.advance(false), Duration::from_secs(60));
        assert_eq!(backoff.advance(false), Duration::from_secs(120));
        assert_eq!(backoff.advance(false), Duration::from_secs(300));
        assert_eq!(backoff.advance(false), Duration::from_secs(600));
        assert_eq!(
            backoff.advance(false),
            Duration::from_secs(600),
            "the ladder caps at its last rung rather than continuing to grow"
        );
        assert_eq!(
            backoff.advance(true),
            Duration::from_secs(60),
            "any movement resets to the base interval immediately, the same tick it happens"
        );
        assert_eq!(
            backoff.advance(false),
            Duration::from_secs(60),
            "the ladder restarts from its first rung after a reset"
        );
    }

    #[test]
    fn the_ladder_cap_is_close_to_one_nominal_gate_run() {
        // scripts/gate-receipt.sh's own header calls the gate "nine minutes
        // nominal" -- the cap should sit in that neighborhood, not far below
        // it (which would spam rewrites on an ordinary run) or far above it
        // (which would leave a wedge invisible for most of a nominal run).
        let nominal_gate_minutes = 9;
        let cap_minutes = *SILENT_LADDER_MINUTES.last().unwrap();
        assert!(
            cap_minutes >= nominal_gate_minutes,
            "the cap ({cap_minutes}m) must be at least one nominal gate run ({nominal_gate_minutes}m), \
             or a run that is merely slow -- not stuck -- would already be publishing at the cap's cadence"
        );
    }
}
