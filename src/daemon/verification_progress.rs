//! The SH-524 verification progress publisher.
//!
//! Every story sitting in `verifying` carries one self-updating
//! `CENTRAL VERIFICATION PROGRESS —` comment: the candidate the release gate
//! is presently running for shows a live nested checklist read from its own
//! [`gate_progress`] journal; every other candidate shows its queue position
//! and wait. SH-549 replaced the unsound queue-head inference with
//! [`VerificationActivity`]: priority may reorder the queue while a lower-
//! priority attempt remains active, so ownership and waiting order are two
//! different facts.

use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::env::Environment;
use crate::error::AppError;
use crate::service::engine::elapsed_secs;
use crate::service::gate_progress::{self, GATE_PROGRESS_PREFIX, VerificationProgressView};
use crate::service::{Ctx, StoryService, VerificationCandidate, VerificationQueue};
use crate::store::Store;

use super::verification::{ActiveVerification, VerificationActivity, journal_path};

/// Dashboard wire shape for a story waiting in the verification queue or
/// actively owned by the daemon verifier.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum VerificationStatus {
    /// Waiting work. Position counts waiting stories only; the active attempt
    /// is not a member of this queue.
    Queued {
        #[serde(skip_serializing_if = "Option::is_none")]
        wait_seconds: Option<u64>,
        position: usize,
    },
    /// Work inside the verifier actuator now.
    Running {
        elapsed_seconds: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        current_step: Option<VerificationStep>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tests: Option<VerificationTests>,
    },
}

/// Current explicit journal item for an active attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VerificationStep {
    /// Human-readable producer label for the active gate step.
    pub label: String,
    /// Seconds since this step emitted its latest `running` event.
    pub elapsed_seconds: u64,
}

/// Exact completed/planned test counts for an active attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VerificationTests {
    /// Exact terminal case count observed in the current attempt.
    pub completed: u32,
    /// Exact planned case count declared by the current attempt.
    pub total: u32,
}

/// One project/story association used while adding statuses to `/data`.
pub type StoryVerificationStatus = (crate::store::ProjectId, String, VerificationStatus);

fn owns(candidate: &VerificationCandidate, active: &ActiveVerification) -> bool {
    candidate.project == active.project
        && candidate.story_id == active.story_id
        && candidate.verifying_generation.is_some()
        && candidate.verifying_generation == active.generation
}

fn matching_progress(
    env: &Environment,
    candidate: &VerificationCandidate,
) -> Option<gate_progress::GateProgress> {
    let generation = candidate.verifying_generation?.get();
    let text = std::fs::read_to_string(journal_path(env, candidate)).ok()?;
    let progress = gate_progress::fold(&text);
    (progress.run.as_ref()?.generation == generation).then_some(progress)
}

/// Builds the exact dashboard status for one consistent ordered queue
/// snapshot plus one process-local ownership snapshot.
#[must_use]
pub fn status_snapshot(
    ordered: &[VerificationCandidate],
    active: Option<&ActiveVerification>,
    env: &Environment,
    now: &str,
) -> Vec<StoryVerificationStatus> {
    let waiting: Vec<&VerificationCandidate> = ordered
        .iter()
        .filter(|candidate| !active.is_some_and(|held| owns(candidate, held)))
        .collect();

    ordered
        .iter()
        .map(|candidate| {
            let status = if let Some(held) = active.filter(|held| owns(candidate, held)) {
                let progress = matching_progress(env, candidate);
                let current_step = progress.as_ref().and_then(|progress| {
                    let step = progress.current_step()?;
                    Some(VerificationStep {
                        label: step.label,
                        elapsed_seconds: elapsed_secs(&step.started_at, now)?,
                    })
                });
                let tests = progress
                    .as_ref()
                    .and_then(gate_progress::GateProgress::exact_test_counts)
                    .map(|(completed, total)| VerificationTests { completed, total });
                VerificationStatus::Running {
                    elapsed_seconds: elapsed_secs(&held.started_at, now).unwrap_or(0),
                    current_step,
                    tests,
                }
            } else {
                let position = waiting
                    .iter()
                    .position(|waiting| {
                        waiting.project == candidate.project
                            && waiting.story_id == candidate.story_id
                            && waiting.verifying_generation == candidate.verifying_generation
                    })
                    .expect("every non-active candidate remains in the waiting queue")
                    + 1;
                VerificationStatus::Queued {
                    wait_seconds: candidate
                        .verifying_since
                        .as_deref()
                        .and_then(|since| elapsed_secs(since, now)),
                    position,
                }
            };
            (candidate.project, candidate.story_id.clone(), status)
        })
        .collect()
}

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

/// Seconds since any producer last changed the journal. File modification is
/// the activity signal because every appended event changes it, including
/// `case` lines that intentionally carry no embedded timestamp (SH-549).
fn seconds_since_journal_activity(path: &std::path::Path, now: &str) -> Option<u64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let modified = chrono::DateTime::<chrono::Utc>::from(modified);
    let now = chrono::DateTime::parse_from_rfc3339(now)
        .ok()?
        .with_timezone(&chrono::Utc);
    u64::try_from((now - modified).num_seconds()).ok()
}

/// Runs one publish attempt across every story presently in `verifying`.
///
/// Returns whether anything was actually written — [`PublishBackoff`]'s own
/// signal for whether to keep the base cadence or start stretching it out.
pub fn publish_once(
    store: &impl Store,
    env: &Environment,
    now: &str,
    activity: &VerificationActivity,
) -> Result<bool, AppError> {
    let ordered = VerificationQueue::new(store).ordered()?;
    let active = activity.active();
    let statuses = status_snapshot(&ordered, active.as_ref(), env, now);
    let mut moved = false;
    for (candidate, (_, _, status)) in ordered.iter().zip(statuses) {
        let ctx = Ctx::new(
            store,
            candidate.project,
            candidate.checkout.clone(),
            env.clone(),
        )
        .no_hooks(true);
        let body = if matches!(&status, VerificationStatus::Running { .. }) {
            let journal = journal_path(env, candidate);
            let progress = matching_progress(env, candidate).unwrap_or_default();
            let elapsed_seconds = active
                .as_ref()
                .filter(|held| owns(candidate, held))
                .and_then(|held| elapsed_secs(&held.started_at, now));
            let seconds_since_last_event = seconds_since_journal_activity(&journal, now);
            gate_progress::render(
                &VerificationProgressView::Running {
                    progress: &progress,
                    elapsed_seconds,
                    seconds_since_last_event,
                },
                now,
            )
        } else {
            let VerificationStatus::Queued { position, .. } = status else {
                unreachable!("running handled above")
            };
            let waiting: Vec<VerificationCandidate> = ordered
                .iter()
                .filter(|candidate| !active.as_ref().is_some_and(|held| owns(candidate, held)))
                .cloned()
                .collect();
            let (ahead_higher_priority, ahead_equal_priority_older) =
                ahead_counts(&waiting, position - 1);
            gate_progress::render(
                &VerificationProgressView::Queued {
                    position,
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
pub(crate) fn poll_verification_progress(
    store: &impl Store,
    env: &Environment,
    stop: &AtomicBool,
    activity: &VerificationActivity,
) {
    const SHUTDOWN_CHECK: Duration = Duration::from_millis(250);
    let mut backoff = PublishBackoff::new();
    while !stop.load(Ordering::Relaxed) {
        let moved = match publish_once(store, env, &env.now(), activity) {
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
    fn an_untimestamped_case_uses_journal_file_activity_for_staleness() {
        let root = tempfile::Builder::new()
            .prefix("storyhook-journal-activity-")
            .tempdir()
            .unwrap();
        let journal = root.path().join("attempt.ndjson");
        std::fs::write(
            &journal,
            "{\"kind\":\"item\",\"path\":\"suite\",\"status\":\"running\",\"at\":\"2020-01-01T00:00:00Z\"}\n\
             {\"kind\":\"case\",\"path\":\"suite\",\"outcome\":\"pass\"}\n",
        )
        .unwrap();
        let modified = std::fs::metadata(&journal).unwrap().modified().unwrap();
        let now = (chrono::DateTime::<chrono::Utc>::from(modified)
            + chrono::Duration::seconds(240))
        .to_rfc3339();

        assert_eq!(
            seconds_since_journal_activity(&journal, &now),
            Some(240),
            "the old embedded item timestamp must not make a fresh case append look stale"
        );
    }

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
