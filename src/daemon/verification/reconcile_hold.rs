//! A conflict-reconcile hold: the project's verifier waits for the story it
//! returned on a merge conflict to resubmit (D-E, SH-650), so `main` cannot
//! move under the reconcile.
//!
//! The hold lasts only while the reconcile can still end in a resubmission
//! (SH-770, council decision D1 on the story). A false release costs one
//! story its reservation: it rejoins the queue in priority order when it
//! resubmits. A false hold blocks the project's whole queue until a person
//! stops the verifier. So every rule here errs toward release.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::domain::StoryCleanupLease;
use crate::error::AppError;
use crate::process::Cancellation;
use crate::service::engine::{STALL_CEILING_SECS, WindowProbe, silent_on_both_channels};
use crate::service::verification::{HeldStory, RETURNED_STATE, VERIFYING_STATE};
use crate::service::{VerificationCandidate, VerificationQueue};
use crate::store::{GlobalSeq, Store};

/// Consecutive `Gone` probes that release a hold (council decision D1 on
/// SH-770). One is not enough: an agent exits right after the `story move`
/// that resubmits, and a re-dispatch that relaunches in the same pane can
/// read dead for an instant.
pub const GONE_CONFIRMATIONS: u32 = 2;

/// How a conflict-reconcile wait ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReconcileWait {
    /// The reserved story resubmitted: a newer generation, validated against
    /// the checkout origin. The reservation transfers to it.
    Resubmitted(Box<VerificationCandidate>),
    /// A daemon stop, `story verifier stop`, or `human-only` ended the wait.
    /// The verifier writes nothing: whoever ended it owns the story.
    Ended,
    /// The reconcile stopped, so the verifier released the queue (SH-770).
    Released(HoldRelease),
}

/// Why a conflict-reconcile hold released the queue before a resubmission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HoldRelease {
    /// The story says it cannot proceed: it carries `awaiting` (the agent's
    /// `story block`, or the Full Auto watchdog's quarantine), an open
    /// `blocked-by`, or the `blocked` state. `reason` is in its own words.
    StoryBlocked { reason: String },
    /// The story left `in-progress` for a state that is not `verifying`, so
    /// nobody is reconciling it.
    StoryLeft { state: String },
    /// [`GONE_CONFIRMATIONS`] probes in a row found no live agent pane named
    /// for the story on the tmux server its lease records. `detail` is the
    /// last probe's own reason.
    AgentGone { detail: String },
    /// The story's change feed and its agent pane's output were both silent
    /// for longer than the stall ceiling (the engine's two-channel rule).
    /// `probe` is the last probe's reason when it did not find the agent.
    AgentSilent {
        silent_secs: u64,
        probe: Option<String>,
    },
}

impl HoldRelease {
    /// What stopped the reconcile, as a sentence fragment for the story's
    /// comment and the activity journal.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::StoryBlocked { reason } => format!("the story is blocked: {reason}"),
            Self::StoryLeft { state } => {
                format!("the story moved to `{state}`, so nobody is reconciling it")
            }
            Self::AgentGone { detail } => format!("its agent pane is gone: {detail}"),
            Self::AgentSilent { silent_secs, probe } => format!(
                "its agent showed no activity for {silent_secs} s: no story event and no pane output{}",
                probe
                    .as_ref()
                    .map(|probe| format!(" ({probe})"))
                    .unwrap_or_default()
            ),
        }
    }

    /// What a person or agent does next, as one sentence for the comment.
    #[must_use]
    pub fn next_step(&self) -> &'static str {
        match self {
            Self::StoryBlocked { .. } => "Clear the block before you resubmit.",
            Self::StoryLeft { .. } => "No action is necessary for the queue.",
            Self::AgentGone { .. } | Self::AgentSilent { .. } => {
                "Resume the agent, or reconcile the branch by hand."
            }
        }
    }
}

/// How a conflict-reconcile hold asks whether the reserved story's agent still
/// runs: the story, the cleanup lease its latest submission recorded (read
/// from the store at each probe), and the attempt's cancellation.
pub type AgentProbe<'a> = dyn Fn(&VerificationCandidate, Option<&StoryCleanupLease>, &Cancellation) -> WindowProbe
    + Sync
    + 'a;

/// How a conflict-reconcile hold watches the reserved story's agent (SH-770).
///
/// The clock is injected so tests can move time past the probe cadence and
/// the stall ceiling without waiting for it.
pub struct HoldWatch<'a> {
    /// Asks whether the agent pane still runs.
    pub probe: &'a AgentProbe<'a>,
    /// The time the cadence and the stall judgment read.
    pub clock: &'a (dyn Fn() -> SystemTime + Sync),
    /// Time between probes. The first probe comes one interval after the
    /// wait starts, because the delivery that started it just found the agent.
    pub probe_every: Duration,
    /// Longest silence on both channels that a live agent can show.
    pub stall_ceiling: Duration,
    /// Told the latest activity the stall rule judges, once per change, so
    /// status can see a hold that stopped releasing (SH-770 decision D1, as
    /// SH-827 applied it to SH-768's reservation bound).
    pub on_activity: Option<&'a (dyn Fn(SystemTime) + Sync)>,
}

impl<'a> HoldWatch<'a> {
    /// The production watch: one probe per recovery wake, the engine's stall
    /// ceiling (derived from one host tool call, SH-394/SH-657), and the
    /// system clock.
    #[must_use]
    pub fn production(probe: &'a AgentProbe<'a>) -> Self {
        Self {
            probe,
            clock: &SystemTime::now,
            probe_every: super::RECOVERY_WAKE,
            stall_ceiling: Duration::from_secs(STALL_CEILING_SECS),
            on_activity: None,
        }
    }
}

/// The agent evidence a hold holds as of one pass.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AgentObservation {
    /// The last probe's reason when the last [`GONE_CONFIRMATIONS`] probes
    /// all found the agent gone.
    pub(crate) gone: Option<String>,
    /// Seconds since the story's change feed last moved; `None` when the
    /// clock went backwards, which states nothing (SH-372).
    pub(crate) store_silent_secs: Option<u64>,
    /// Seconds since the pane last wrote, from the latest probe; `None` when
    /// no probe has read it.
    pub(crate) output_silent_secs: Option<u64>,
    /// The latest probe's reason when it did not find the agent alive.
    pub(crate) probe_detail: Option<String>,
}

/// Decides from the agent evidence whether the reconcile has stopped. Pure,
/// so every rule is table-testable.
///
/// The stall rule is the engine's own predicate, never a copy (council
/// decision D1): with no pane output known — no lease, or a probe that did not
/// answer — the store's silence alone decides at the same ceiling (SH-626).
pub(crate) fn agent_release(
    observation: &AgentObservation,
    stall_ceiling_secs: u64,
) -> Option<HoldRelease> {
    if let Some(detail) = &observation.gone {
        return Some(HoldRelease::AgentGone {
            detail: detail.clone(),
        });
    }
    // Silence is measured from the change feed's last move, so the store is
    // unmoved by construction.
    silent_on_both_channels(
        true,
        observation.store_silent_secs,
        observation.output_silent_secs,
        stall_ceiling_secs,
    )
    .then(|| HoldRelease::AgentSilent {
        silent_secs: observation.store_silent_secs.unwrap_or_default(),
        probe: observation.probe_detail.clone(),
    })
}

/// What one hold has observed of its agent across passes.
struct AgentWatch {
    last_probe_at: SystemTime,
    head_seq: Option<GlobalSeq>,
    moved_at: SystemTime,
    gone_streak: u32,
    window: Option<WindowProbe>,
}

impl AgentWatch {
    fn new(now: SystemTime) -> Self {
        Self {
            last_probe_at: now,
            head_seq: None,
            moved_at: now,
            gone_streak: 0,
            window: None,
        }
    }

    /// Records the story's change-feed position. The first position read is
    /// the one the hold started at, so silence counts from the start.
    fn observe_story(&mut self, head_seq: GlobalSeq, now: SystemTime) {
        if self.head_seq.is_some_and(|seen| seen != head_seq) {
            self.moved_at = now;
        }
        self.head_seq = Some(head_seq);
    }

    /// A clock that went backwards makes the probe due rather than silence it.
    fn probe_due(&self, now: SystemTime, every: Duration) -> bool {
        match now.duration_since(self.last_probe_at) {
            Ok(elapsed) => elapsed >= every,
            Err(_) => true,
        }
    }

    /// The latest activity the stall rule credits: the change feed's last
    /// move (the hold's start until then) or the latest probe's pane output,
    /// whichever is later. A pane stamp in the future is no evidence (SH-372),
    /// as it is to [`Self::observation`]. When the pane channel is unknown the
    /// store alone decides, and no later than this instant plus the ceiling.
    fn last_activity(&self, now: SystemTime) -> SystemTime {
        let output = match &self.window {
            Some(WindowProbe::Alive {
                last_output_at: Some(stamp),
            }) => u64::try_from(*stamp)
                .ok()
                .map(|stamp| UNIX_EPOCH + Duration::from_secs(stamp))
                .filter(|stamp| *stamp <= now),
            _ => None,
        };
        output.map_or(self.moved_at, |output| output.max(self.moved_at))
    }

    fn record_probe(&mut self, window: WindowProbe, now: SystemTime) {
        self.last_probe_at = now;
        self.gone_streak = match window {
            WindowProbe::Gone { .. } => self.gone_streak.saturating_add(1),
            WindowProbe::Alive { .. } | WindowProbe::Unanswered { .. } => 0,
        };
        self.window = Some(window);
    }

    fn observation(&self, now: SystemTime) -> AgentObservation {
        let output_silent_secs = match &self.window {
            Some(WindowProbe::Alive {
                last_output_at: Some(stamp),
            }) => now.duration_since(UNIX_EPOCH).ok().and_then(|now| {
                u64::try_from(*stamp)
                    .ok()
                    .and_then(|stamp| now.as_secs().checked_sub(stamp))
            }),
            _ => None,
        };
        AgentObservation {
            gone: (self.gone_streak >= GONE_CONFIRMATIONS)
                .then(|| self.window.as_ref().and_then(WindowProbe::detail))
                .flatten()
                .map(str::to_string),
            store_silent_secs: now
                .duration_since(self.moved_at)
                .ok()
                .map(|silent| silent.as_secs()),
            output_silent_secs,
            probe_detail: self
                .window
                .as_ref()
                .and_then(WindowProbe::detail)
                .map(str::to_string),
        }
    }
}

/// Decides from the reserved story's own facts whether its reconcile has
/// stopped. Pure, so every rule is table-testable.
///
/// A blocked story is tested first: a story can be blocked in any state, and
/// its block is the more specific reason.
pub(crate) fn story_release(story: &HeldStory) -> Option<HoldRelease> {
    if let Some(reason) = &story.blocked {
        return Some(HoldRelease::StoryBlocked {
            reason: reason.clone(),
        });
    }
    // `verifying` without a newer generation is the instant between a
    // resubmission's write and its queue membership; the next pass sees it.
    (story.state != RETURNED_STATE && story.state != VERIFYING_STATE).then(|| {
        HoldRelease::StoryLeft {
            state: story.state.clone(),
        }
    })
}

/// Waits until the reserved story creates a newer verification generation,
/// or until its reconcile stops.
///
/// Other queue arrivals and coarse bus wakes only cause a fresh observation;
/// they cannot transfer the reservation. An observation reads the store alone
/// and starts no process; the checkout origin is validated once, for the
/// resubmission this returns (SH-769). The only process a hold starts is one
/// agent probe per [`HoldWatch::probe_every`]. A daemon stop ends the wait
/// without manufacturing a candidate. Public for shutdown and event-order
/// integration tests.
pub fn wait_for_reconciled_candidate(
    store: &impl Store,
    subscription: &crate::daemon::bus::Subscription,
    stop: &AtomicBool,
    reserved: &VerificationCandidate,
    watch: &HoldWatch<'_>,
) -> Result<ReconcileWait, AppError> {
    wait_for_reconciled_candidate_cancellable(
        store,
        subscription,
        stop,
        reserved,
        &Cancellation::default(),
        watch,
    )
}

pub(super) fn wait_for_reconciled_candidate_cancellable(
    store: &impl Store,
    subscription: &crate::daemon::bus::Subscription,
    stop: &AtomicBool,
    reserved: &VerificationCandidate,
    cancellation: &Cancellation,
    watch: &HoldWatch<'_>,
) -> Result<ReconcileWait, AppError> {
    let queue = VerificationQueue::new(store);
    let newer = |generation: Option<GlobalSeq>| {
        generation.is_some() && generation != reserved.verifying_generation
    };
    let mut agent = AgentWatch::new((watch.clock)());
    let mut published = None;
    loop {
        if stop.load(Ordering::Relaxed) || cancellation.is_cancelled() {
            return Ok(ReconcileWait::Ended);
        }
        // A pass runs every 100 ms and on every bus wake, so it reads the store
        // alone: validating origins starts `git` (SH-769). Only a resubmission
        // is validated, and the second read may find it gone again.
        let view = queue.hold_view(reserved).map_err(|error| {
            error.with_context(&format!(
                "reading the reconcile hold for project={} story={}",
                reserved.project_slug, reserved.story_id,
            ))
        })?;
        let Some(story) = view.story.filter(|story| story.permitted) else {
            return Ok(ReconcileWait::Ended);
        };
        if newer(view.generation)
            && let Some(candidate) = queue
                .current_for(reserved)?
                .filter(|candidate| newer(candidate.verifying_generation))
        {
            return Ok(ReconcileWait::Resubmitted(Box::new(candidate)));
        }
        if let Some(release) = story_release(&story) {
            return Ok(ReconcileWait::Released(release));
        }
        let now = (watch.clock)();
        agent.observe_story(story.head_seq, now);
        // Once per change, not per pass: the hook takes the registry lock.
        let activity = agent.last_activity(now);
        if published != Some(activity) {
            published = Some(activity);
            if let Some(publish) = watch.on_activity {
                publish(activity);
            }
        }
        if let Some(release) = agent_release(&agent.observation(now), watch.stall_ceiling.as_secs())
        {
            return Ok(ReconcileWait::Released(release));
        }
        if agent.probe_due(now, watch.probe_every) {
            let lease = queue.cleanup_lease_for(reserved)?;
            agent.record_probe((watch.probe)(reserved, lease.as_ref(), cancellation), now);
            // The probe's evidence is judged on a fresh pass, so a resubmission
            // or a store fact that landed while tmux answered wins.
            continue;
        }
        let _ = subscription.recv(Duration::from_millis(100));
    }
}

/// A probe that knows no agent panes, for tests about something else.
#[cfg(test)]
pub(super) fn unwatched(
    _candidate: &VerificationCandidate,
    _lease: Option<&StoryCleanupLease>,
    _cancellation: &Cancellation,
) -> WindowProbe {
    WindowProbe::Unanswered {
        detail: "this test watches no agent pane".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn story(state: &str, blocked: Option<&str>) -> HeldStory {
        HeldStory {
            permitted: true,
            blocked: blocked.map(str::to_string),
            state: state.into(),
            head_seq: GlobalSeq::ZERO,
        }
    }

    #[test]
    fn a_story_still_reconciling_or_resubmitting_holds() {
        assert_eq!(story_release(&story(RETURNED_STATE, None)), None);
        assert_eq!(story_release(&story(VERIFYING_STATE, None)), None);
    }

    #[test]
    fn a_blocked_story_releases_in_any_state_with_its_own_reason() {
        for state in [RETURNED_STATE, VERIFYING_STATE, "blocked", "todo"] {
            assert_eq!(
                story_release(&story(state, Some("blocked by SH-2"))),
                Some(HoldRelease::StoryBlocked {
                    reason: "blocked by SH-2".into()
                }),
                "{state}"
            );
        }
    }

    #[test]
    fn a_story_that_left_the_reconcile_releases() {
        for state in ["todo", "done", "dropped", "backlog"] {
            assert_eq!(
                story_release(&story(state, None)),
                Some(HoldRelease::StoryLeft {
                    state: state.into()
                }),
                "{state}"
            );
        }
    }

    const CEILING: u64 = STALL_CEILING_SECS;

    fn observed(store: Option<u64>, output: Option<u64>) -> AgentObservation {
        AgentObservation {
            store_silent_secs: store,
            output_silent_secs: output,
            ..AgentObservation::default()
        }
    }

    #[test]
    fn two_gone_probes_release_whatever_the_clocks_say() {
        let observation = AgentObservation {
            gone: Some("tmux finds no pane".into()),
            ..observed(Some(0), Some(0))
        };
        assert_eq!(
            agent_release(&observation, CEILING),
            Some(HoldRelease::AgentGone {
                detail: "tmux finds no pane".into()
            })
        );
    }

    #[test]
    fn the_stall_rule_is_the_engines_two_channel_rule() {
        let silent = HoldRelease::AgentSilent {
            silent_secs: CEILING + 1,
            probe: None,
        };
        for (store, output, expected, why) in [
            (
                Some(CEILING + 1),
                Some(CEILING + 1),
                Some(silent.clone()),
                "both silent",
            ),
            (
                Some(CEILING + 1),
                None,
                Some(silent.clone()),
                "no pane evidence: the store decides (SH-626)",
            ),
            (Some(CEILING + 1), Some(5), None, "a pane that wrote holds"),
            (Some(CEILING), None, None, "at the ceiling is not past it"),
            (Some(5), None, None, "a story event holds"),
            (
                None,
                None,
                None,
                "a clock that went backwards states nothing (SH-372)",
            ),
        ] {
            assert_eq!(
                agent_release(&observed(store, output), CEILING),
                expected,
                "{why}"
            );
        }
    }

    #[test]
    fn a_gone_probe_counts_only_in_an_unbroken_run() {
        let start = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let gone = || WindowProbe::Gone {
            detail: "gone".into(),
        };
        let mut agent = AgentWatch::new(start);
        agent.record_probe(gone(), start);
        assert_eq!(
            agent.observation(start).gone,
            None,
            "one probe is not enough"
        );
        agent.record_probe(
            WindowProbe::Unanswered {
                detail: "tmux timed out".into(),
            },
            start,
        );
        agent.record_probe(gone(), start);
        assert_eq!(
            agent.observation(start).gone,
            None,
            "an unanswered probe breaks the run"
        );
        agent.record_probe(gone(), start);
        assert_eq!(agent.observation(start).gone, Some("gone".into()));
    }

    #[test]
    fn pane_output_and_story_events_reset_their_own_channels() {
        let start = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let later = start + Duration::from_secs(700);
        let mut agent = AgentWatch::new(start);
        agent.observe_story(GlobalSeq::ZERO, start + Duration::from_secs(1));
        agent.record_probe(
            WindowProbe::Alive {
                last_output_at: Some(1_000_600),
            },
            later,
        );
        let observation = agent.observation(later);
        assert_eq!(
            observation.store_silent_secs,
            Some(700),
            "silence counts from the hold's start, not from its first read"
        );
        assert_eq!(observation.output_silent_secs, Some(100));
        agent.observe_story(GlobalSeq::ZERO, later);
        assert_eq!(
            agent.observation(later).store_silent_secs,
            Some(700),
            "the same change-feed position is not progress"
        );
        agent.observe_story(GlobalSeq::new(7), later);
        assert_eq!(agent.observation(later).store_silent_secs, Some(0));
    }

    /// The published activity is the instant the stall rule counts from, so
    /// the status bound (that instant plus ceiling and one wake) can only be
    /// passed by a hold that failed to release.
    #[test]
    fn the_published_activity_is_the_later_channel_the_stall_rule_credits() {
        let start = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let at = |secs: u64| UNIX_EPOCH + Duration::from_secs(secs);
        let mut agent = AgentWatch::new(start);
        agent.observe_story(GlobalSeq::ZERO, start);
        assert_eq!(agent.last_activity(start), start, "the hold's start");

        let now = at(1_000_900);
        agent.record_probe(
            WindowProbe::Alive {
                last_output_at: Some(1_000_600),
            },
            now,
        );
        assert_eq!(agent.last_activity(now), at(1_000_600), "pane output");
        agent.observe_story(GlobalSeq::new(3), at(1_000_700));
        assert_eq!(agent.last_activity(now), at(1_000_700), "a story event");

        for (window, why) in [
            (
                WindowProbe::Alive {
                    last_output_at: Some(1_000_950),
                },
                "a stamp in the future is no evidence",
            ),
            (
                WindowProbe::Alive {
                    last_output_at: None,
                },
                "a live pane with no stamp states nothing",
            ),
            (
                WindowProbe::Unanswered {
                    detail: "tmux timed out".into(),
                },
                "an unanswered probe leaves the store to decide",
            ),
            (
                WindowProbe::Gone {
                    detail: "gone".into(),
                },
                "a gone pane leaves the store to decide",
            ),
        ] {
            agent.record_probe(window, now);
            assert_eq!(agent.last_activity(now), at(1_000_700), "{why}");
            assert_eq!(
                agent.observation(now).store_silent_secs,
                Some(200),
                "{why}: the stall rule counts from the same instant"
            );
        }
    }

    #[test]
    fn a_probe_is_due_one_interval_after_the_last_and_after_a_clock_jump_back() {
        let start = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let every = Duration::from_secs(30);
        let agent = AgentWatch::new(start);
        assert!(
            !agent.probe_due(start, every),
            "the delivery just found the agent"
        );
        assert!(!agent.probe_due(start + Duration::from_secs(29), every));
        assert!(agent.probe_due(start + every, every));
        assert!(agent.probe_due(start - every, every));
    }
}
