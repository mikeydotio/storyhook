//! A verifier that still owns a story its own write took out of the queue
//! (SH-768).
//!
//! Only the owner knows why its generation left the verifying queue, so it
//! declares that on its slot and status reads the declaration instead of
//! inferring a fault from the absence. Like the ownership it qualifies, the
//! declaration is process-local: it cannot outlive the daemon that holds it.

use std::sync::PoisonError;

use serde::{Deserialize, Serialize};

use super::{ActiveVerification, VerificationGuard};
use crate::daemon::verification_progress::{StoryVerificationStatus, VerificationStatus};
use crate::service::VerificationCandidate;
use crate::store::GlobalSeq;

/// Why a project's verifier keeps its slot after its own write took the owned
/// generation out of the verifying queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReservationReason {
    /// Returned on a merge conflict; held until the agent resubmits.
    Reconcile,
}

impl ReservationReason {
    /// What the verifier is doing, as an operator reads it.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::Reconcile => "merge-conflict reconcile until it resubmits",
        }
    }
}

/// The owner's declaration, carried by its registry slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Reservation {
    pub(crate) reason: ReservationReason,
    pub(crate) reserved_at: String,
    /// The owner's write committed, so the generation has left the queue. A
    /// pending declaration precedes that write: it may explain an absence,
    /// never a presence.
    pub(crate) retired: bool,
}

/// One registry read of a project's slot, taken under the registry lock.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SlotView<'a> {
    /// Identity of the owned attempt.
    pub(crate) active: &'a ActiveVerification,
    /// Why the owner holds a story its own write took out of the queue.
    pub(crate) reservation: Option<&'a Reservation>,
}

/// A reservation declared before the write that retires the owned generation.
///
/// Dropping it without [`PendingReservation::retire`] withdraws it, so a
/// write that was superseded or failed leaves no declaration behind.
#[must_use = "dropping a pending reservation withdraws it; retire() it once the write applied"]
pub(crate) struct PendingReservation<'a> {
    owner: &'a VerificationGuard,
    armed: bool,
}

impl PendingReservation<'_> {
    /// Keeps the declaration: the owner's write committed.
    pub(crate) fn retire(mut self) {
        self.owner.update_reservation(|reservation| {
            if let Some(reservation) = reservation {
                reservation.retired = true;
            }
        });
        self.armed = false;
    }
}

impl Drop for PendingReservation<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.owner
                .update_reservation(|reservation| *reservation = None);
        }
    }
}

impl VerificationGuard {
    /// Declares, before the write that takes the owned generation out of the
    /// queue, why this owner will keep its slot afterwards.
    ///
    /// Declaring first leaves no instant in which status sees the generation
    /// gone without the reason. This takes the registry lock alone; callers
    /// hold neither it nor a store transaction.
    pub(crate) fn reserve(
        &self,
        reason: ReservationReason,
        reserved_at: String,
    ) -> PendingReservation<'_> {
        self.update_reservation(|reservation| {
            *reservation = Some(Reservation {
                reason,
                reserved_at,
                retired: false,
            });
        });
        PendingReservation {
            owner: self,
            armed: true,
        }
    }

    /// Whether this owner's slot carries a reservation.
    pub(crate) fn is_reserved(&self) -> bool {
        self.registry
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&self.active.project)
            .filter(|slot| slot.active == self.active)
            .is_some_and(|slot| slot.reservation.is_some())
    }

    fn update_reservation(&self, update: impl FnOnce(&mut Option<Reservation>)) {
        let mut slots = self
            .registry
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(slot) = slots
            .get_mut(&self.active.project)
            .filter(|slot| slot.active == self.active)
        {
            update(&mut slot.reservation);
        }
    }
}

/// A verifier held for one story that has left its queue: ordinary activity,
/// not missing evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifierReservation {
    /// Display id of the reserved story.
    pub story_id: String,
    /// The generation the owner took out of the queue.
    pub generation: Option<GlobalSeq>,
    /// Why the verifier keeps the story.
    pub reason: ReservationReason,
    /// When the owner declared the reservation (UTC).
    pub reserved_at: String,
    /// Seconds since `reserved_at`; absent when the clock moved backwards.
    pub age_seconds: Option<u64>,
    /// Stories waiting for this verifier; held and landing-pending ones are not.
    pub queued_behind: usize,
}

/// Decides what a slot's declaration means for one snapshot of the queue.
///
/// `Ok(None)`: no declaration applies, so journal evidence is read as usual.
/// That includes a pending declaration whose write has not committed yet.
/// `Ok(Some(_))`: the declaration explains why the owned generation is absent.
/// `Err(_)`: a retired declaration whose generation is still queued, which
/// only a defect can produce.
pub(crate) fn held<'a>(
    owner: Option<SlotView<'a>>,
    ordered: &[VerificationCandidate],
) -> Result<Option<&'a Reservation>, String> {
    let Some((active, reservation)) =
        owner.and_then(|owner| Some((owner.active, owner.reservation?)))
    else {
        return Ok(None);
    };
    match super::evidence::owned_candidate(ordered, active) {
        None => Ok(Some(reservation)),
        Some(_) if !reservation.retired => Ok(None),
        Some(_) => Err(format!(
            "reservation for project {} story {} ({:?}) says generation {:?} left the verifying queue, but it is still verifying",
            active.project, active.story_id, reservation.reason, active.generation
        )),
    }
}

impl VerifierReservation {
    /// Projects an applicable declaration next to the statuses of the stories
    /// that wait for the same verifier.
    pub(crate) fn project(
        active: &ActiveVerification,
        reservation: &Reservation,
        statuses: &[StoryVerificationStatus],
        now: &str,
    ) -> Self {
        Self {
            story_id: active.story_id.clone(),
            generation: active.generation,
            reason: reservation.reason,
            reserved_at: reservation.reserved_at.clone(),
            age_seconds: crate::service::engine::elapsed_secs(&reservation.reserved_at, now),
            queued_behind: statuses
                .iter()
                .filter(|(_, _, status)| matches!(status, VerificationStatus::Queued { .. }))
                .count(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use super::{ReservationReason, VerifierReservation};
    use crate::service::{Clock, NewStoryInput, RelationService};
    use storyhook_test_support::ServiceFixture;

    const T0: &str = "2026-01-01T00:00:00Z";

    struct Board {
        // Owns the scratch directory the store and environment live in.
        _fixture: ServiceFixture,
        store: crate::store::SqliteStore,
        project: ProjectId,
        env: Environment,
    }

    impl Board {
        fn new() -> Self {
            let fixture = ServiceFixture::new();
            // Unit tests link a second crate instance through test-support. Reopen
            // its seeded database with this crate's types instead of duplicating the seed.
            let store = crate::store::SqliteStore::open(fixture.store().path()).unwrap();
            let project = ProjectId::new(fixture.project().get());
            let env = Environment::at(fixture.cwd());
            Self {
                _fixture: fixture,
                store,
                project,
                env,
            }
        }

        fn ctx_at(&self, now: &str) -> Ctx<'_, crate::store::SqliteStore> {
            Ctx::new(
                &self.store,
                self.project,
                self.env.home().to_path_buf(),
                self.env.clone(),
            )
            .no_hooks(true)
            .clock(Clock::Fixed(now.into()))
        }

        fn story(&self, title: &str, state: Option<&str>) -> String {
            let ctx = self.ctx_at(T0);
            let id = StoryService::new(&ctx)
                .create(&NewStoryInput {
                    title: title.into(),
                    ..NewStoryInput::default()
                })
                .unwrap()
                .id;
            if let Some(state) = state {
                self.move_to(&id, state);
            }
            id
        }

        fn candidate(&self, id: &str) -> VerificationCandidate {
            VerificationQueue::new(&self.store)
                .ordered_for(self.project)
                .unwrap()
                .into_iter()
                .find(|candidate| candidate.story_id == id)
                .unwrap()
        }

        fn move_to(&self, id: &str, state: &str) {
            StoryService::new(&self.ctx_at(T0))
                .set_state(id, state, None, None, None)
                .unwrap();
        }

        fn status_at(&self, activity: &VerificationActivity, now: &str) -> status::VerifierStatus {
            activity.status(&self.ctx_at(now)).unwrap()
        }
    }

    fn after(seconds: i64) -> String {
        (chrono::DateTime::parse_from_rfc3339(T0).unwrap() + chrono::Duration::seconds(seconds))
            .to_rfc3339()
    }

    #[test]
    fn an_undeclared_absence_fails_loud_and_a_declared_one_reads_as_activity() {
        let board = Board::new();
        let held = board.story("Reconciling", Some("verifying"));
        let candidate = board.candidate(&held);
        let activity = VerificationActivity::new();
        let guard = activity.acquire(&candidate, T0.into());
        board.move_to(&held, "in-progress");

        let undeclared = board.status_at(&activity, &after(5));
        assert!(undeclared.reservation.is_none());
        assert!(
            undeclared
                .evidence_error
                .as_deref()
                .is_some_and(|error| error.contains("no longer in the verifying queue")),
            "{undeclared:?}"
        );
        assert!(undeclared.warning.is_some());

        guard
            .reserve(ReservationReason::Reconcile, T0.into())
            .retire();
        for (elapsed, age) in [(5, 5), (121, 121)] {
            let status = board.status_at(&activity, &after(elapsed));
            assert_eq!(
                status.reservation,
                Some(VerifierReservation {
                    story_id: held.clone(),
                    generation: candidate.verifying_generation,
                    reason: ReservationReason::Reconcile,
                    reserved_at: T0.into(),
                    age_seconds: Some(age),
                    queued_behind: 0,
                })
            );
            assert_eq!(status.evidence_error, None, "{status:?}");
            assert_eq!(status.warning, None, "{status:?}");
            assert_eq!(status.silence_seconds, None);
            assert_eq!(status.last_evidence_at, None);
            let text = status.render_human();
            assert!(
                text.contains(&format!(
                    "{held} reserved for merge-conflict reconcile until it resubmits since"
                )),
                "{text}"
            );
            assert!(
                text.contains(&format!("({age}s); 0 queued behind")),
                "{text}"
            );
            assert!(!text.contains("gate on"), "{text}");
        }
    }

    #[test]
    fn a_pending_reservation_explains_an_absence_and_is_withdrawn_when_dropped() {
        let board = Board::new();
        let held = board.story("Returning", Some("verifying"));
        let activity = VerificationActivity::new();
        let guard = activity.acquire(&board.candidate(&held), T0.into());

        let pending = guard.reserve(ReservationReason::Reconcile, T0.into());
        let before_write = board.status_at(&activity, &after(1));
        assert!(before_write.reservation.is_none(), "{before_write:?}");
        assert_eq!(before_write.evidence_error, None);
        assert!(before_write.render_human().contains("gate on"));

        board.move_to(&held, "in-progress");
        let after_write = board.status_at(&activity, &after(2));
        assert_eq!(
            after_write
                .reservation
                .map(|reservation| reservation.reason),
            Some(ReservationReason::Reconcile)
        );
        assert_eq!(after_write.evidence_error, None);

        drop(pending);
        assert!(!guard.is_reserved());
        assert!(
            board
                .status_at(&activity, &after(3))
                .evidence_error
                .is_some_and(|error| error.contains("no longer in the verifying queue"))
        );
    }

    #[test]
    fn a_retired_reservation_whose_story_is_still_queued_is_a_contradiction() {
        let board = Board::new();
        let held = board.story("Never returned", Some("verifying"));
        let activity = VerificationActivity::new();
        let guard = activity.acquire(&board.candidate(&held), T0.into());
        guard
            .reserve(ReservationReason::Reconcile, T0.into())
            .retire();

        let status = board.status_at(&activity, &after(1));
        assert!(status.reservation.is_none());
        assert!(
            status
                .evidence_error
                .as_deref()
                .is_some_and(|error| error.contains("but it is still verifying")),
            "{status:?}"
        );
        assert!(
            status
                .warning
                .is_some_and(|warning| warning.contains("still verifying"))
        );
    }

    #[test]
    fn a_reservation_ends_with_its_generation_or_its_owner() {
        let board = Board::new();
        let held = board.story("Resubmits", Some("verifying"));
        let mut candidate = board.candidate(&held);
        let activity = VerificationActivity::new();
        let mut guard = activity.acquire(&candidate, T0.into());
        guard
            .reserve(ReservationReason::Reconcile, T0.into())
            .retire();
        assert!(guard.is_reserved());

        candidate.verifying_generation = Some(GlobalSeq::new(
            candidate.verifying_generation.unwrap().get() + 1,
        ));
        guard.replace(&candidate, T0.into());
        assert!(!guard.is_reserved(), "a new generation starts unreserved");

        guard
            .reserve(ReservationReason::Reconcile, T0.into())
            .retire();
        drop(guard);
        assert!(activity.active_for(board.project).is_none());
    }

    #[test]
    fn only_stories_waiting_for_the_verifier_count_as_queued_behind() {
        let board = Board::new();
        let held = board.story("Reconciling", Some("verifying"));
        let activity = VerificationActivity::new();
        let guard = activity.acquire(&board.candidate(&held), T0.into());
        board.move_to(&held, "in-progress");
        let waiting = board.story("Waiting", Some("verifying"));
        let blocker = board.story("Open blocker", None);
        let blocked = board.story("Blocked", Some("verifying"));
        RelationService::new(&board.ctx_at(T0))
            .relate(&blocked, "blocked-by", &blocker, false)
            .unwrap();
        guard
            .reserve(ReservationReason::Reconcile, T0.into())
            .retire();

        let status = board.status_at(&activity, &after(1));
        assert_eq!(status.verifying, [waiting, blocked]);
        assert_eq!(status.reservation.map(|r| r.queued_behind), Some(1));
    }
}
