//! The verifier's own submission is not a supersession (SH-776, adopted).
//!
//! Every leased generation is submitted, and the submission links its pull
//! request again: a new link, or the same one with a new `linked_at`. The
//! refresh after it compared the full link, took the verifier's own write
//! for a supersession of the same generation, and replaced the admitted
//! attempt before its gate: a new attempt id, no retry origin, and a
//! "superseded" record in the daemon journal for every verification.

use super::*;
use storyhook::daemon::verification::{ActiveVerification, LandingOutcome};

/// Records the owning attempt at each blocking call the tick makes.
struct AttemptWitness<'a> {
    activity: &'a VerificationActivity,
    submission: storyhook::domain::landing::SubmissionOutcome,
    outcomes: Mutex<VecDeque<VerificationOutcome>>,
    seen: Mutex<Vec<(&'static str, ActiveVerification)>>,
}

impl<'a> AttemptWitness<'a> {
    fn new(
        activity: &'a VerificationActivity,
        submission: storyhook::domain::landing::SubmissionOutcome,
        outcomes: impl IntoIterator<Item = VerificationOutcome>,
    ) -> Self {
        Self {
            activity,
            submission,
            outcomes: Mutex::new(outcomes.into_iter().collect()),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn witness(&self, call: &'static str, candidate: &VerificationCandidate) {
        let owner = self
            .activity
            .active_for(candidate.project)
            .expect("the tick owns the project during its blocking calls");
        self.seen.lock().unwrap().push((call, owner));
    }

    fn take_seen(&self) -> Vec<(&'static str, ActiveVerification)> {
        std::mem::take(&mut *self.seen.lock().unwrap())
    }
}

impl VerificationActuator for AttemptWitness<'_> {
    fn submit(
        &self,
        candidate: &VerificationCandidate,
    ) -> Result<storyhook::domain::landing::SubmissionOutcome, SubmissionFailure> {
        self.witness("submit", candidate);
        Ok(self.submission.clone())
    }

    fn verify(
        &self,
        candidate: &VerificationCandidate,
        _pull_request: &PrLink,
    ) -> VerificationOutcome {
        self.witness("verify", candidate);
        self.outcomes
            .lock()
            .unwrap()
            .pop_front()
            .expect("every verification attempt must have a fixture outcome")
    }

    fn land(
        &self,
        _candidate: &VerificationCandidate,
        _intent: &storyhook::store::LandingIntent,
    ) -> LandingOutcome {
        LandingOutcome::Merged {
            detail: "test merge confirmed".into(),
        }
    }

    fn recover_landing(
        &self,
        _candidate: &VerificationCandidate,
        _intent: &storyhook::store::LandingIntent,
    ) -> LandingOutcome {
        panic!("this test does not leave unresolved landing authority")
    }

    fn notify(
        &self,
        _candidate: &VerificationCandidate,
        _message: &str,
    ) -> Result<NotifyDelivery, AppError> {
        Ok(NotifyDelivery::Delivered)
    }

    fn redispatch(
        &self,
        _candidate: &VerificationCandidate,
        _plan: &ResumePlan,
    ) -> Result<(), AppError> {
        panic!("a delivered notification never re-dispatches")
    }

    fn reap(&self, _candidate: &VerificationCandidate) -> Result<(), AppError> {
        Ok(())
    }
}

/// One production tick whose clock reads `at`, so every link the tick
/// records carries a `linked_at` distinct from any earlier one.
fn tick_at(
    fixture: &ServiceFixture,
    root: &Path,
    witness: &AttemptWitness<'_>,
    at: &str,
) -> TickResult {
    let env = Environment::at(root).clock(Clock::Fixed(at.into()));
    tick_with_activity(
        fixture.store(),
        &env,
        witness,
        witness.activity,
        &InFlight::new(env.clone()),
        fixture.project(),
    )
    .unwrap()
}

fn certified() -> VerificationOutcome {
    VerificationOutcome::Certified {
        head: "a".repeat(40),
        tree: "b".repeat(40),
        detail: "landed".into(),
        gate: GateCommand::DEFAULT.into(),
    }
}

#[test]
fn the_verifiers_own_submission_keeps_the_admitted_attempt() {
    // Opened: the story linked nothing. Adopted: the story linked this pull
    // request earlier, at the fixture's time, and the submission links it again.
    for (case, linked, adopted) in [("opened", None, false), ("adopted", Some(PR_ONE), true)] {
        let fixture = ServiceFixture::new();
        fixture.github_checkout("https://github.com/acme/widgets");
        let root = scratch_dir();
        leased_submission(&fixture, root.path(), case, linked);
        let activity = VerificationActivity::new();
        let witness =
            AttemptWitness::new(&activity, submitted_pr(PR_ONE, 1, adopted), [certified()]);

        assert_eq!(
            tick_at(&fixture, root.path(), &witness, "2026-09-29T00:00:00Z"),
            TickResult::Completed,
            "{case}"
        );

        let seen = witness.take_seen();
        let calls: Vec<_> = seen.iter().map(|(call, _)| *call).collect();
        assert_eq!(calls, ["submit", "verify"], "{case}");
        assert_eq!(
            seen[1].1, seen[0].1,
            "{case}: the gate ran under another attempt than the one admitted"
        );
    }
}

#[test]
fn a_retry_keeps_its_origin_through_its_own_submission() {
    let fixture = ServiceFixture::new();
    fixture.github_checkout("https://github.com/acme/widgets");
    let root = scratch_dir();
    leased_submission(&fixture, root.path(), "retried", Some(PR_ONE));
    let activity = VerificationActivity::new();
    let witness = AttemptWitness::new(
        &activity,
        submitted_pr(PR_ONE, 1, true),
        [
            VerificationOutcome::InfrastructureFailure {
                detail: "head ref did not converge".into(),
                disposition: VerificationFailureDisposition::Retryable,
            },
            certified(),
        ],
    );
    assert_eq!(
        tick_at(&fixture, root.path(), &witness, "2026-09-29T00:00:00Z"),
        TickResult::RetryLater
    );
    witness.take_seen();

    assert_eq!(
        tick_at(&fixture, root.path(), &witness, "2026-09-29T00:01:00Z"),
        TickResult::Completed
    );

    let seen = witness.take_seen();
    let calls: Vec<_> = seen.iter().map(|(call, _)| *call).collect();
    assert_eq!(calls, ["submit", "verify"]);
    let (submitted, verified) = (&seen[0].1, &seen[1].1);
    assert!(
        submitted.retry_origin.is_some(),
        "the retry was admitted as one: {submitted:?}"
    );
    assert_eq!(
        verified, submitted,
        "the retry's gate ran under another attempt, without its origin"
    );
}
