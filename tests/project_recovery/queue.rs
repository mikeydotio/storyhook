//! Real callback and queue transactions; only the gate/remote-merge endpoint is a double.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use storyhook::cli::{Invocation, VerifierAction};
use storyhook::daemon::{lifecycle::InFlight, verification::*};
use storyhook::domain::SubmittedPullRequest;
use storyhook::error::AppError;
use storyhook::service::project_recovery::{RepairAdmission, RepairInput, RepairScope};
use storyhook::store::{LandingIntent, PrLink, SqliteStore};

struct GateEndpoint<'a> {
    store: &'a SqliteStore,
    env: &'a storyhook::env::Environment,
    activity: &'a VerificationActivity,
    input: RepairInput,
    mismatch: bool,
    fail_tests: bool,
    executions: AtomicUsize,
}
impl VerificationActuator for GateEndpoint<'_> {
    fn submit(&self, _: &VerificationCandidate) -> Result<SubmittedPullRequest, SubmissionFailure> {
        panic!("linked fixture")
    }
    fn verify(&self, candidate: &VerificationCandidate, _: &PrLink) -> VerificationOutcome {
        let active = self.activity.active_for(candidate.project).unwrap();
        let ctx = storyhook::service::Ctx::new(
            self.store,
            candidate.project,
            candidate.checkout.clone(),
            self.env.clone(),
        )
        .with_verification_activity(Some(self.activity));
        let response = storyhook::invoke::dispatch(
            &ctx,
            Invocation::Verifier {
                action: VerifierAction::RepairAdmit {
                    story_id: candidate.story_id.clone(),
                    attempt_id: active.attempt_id,
                    generation: active.generation.unwrap().get(),
                    input: self.input.clone(),
                },
            },
        )
        .unwrap();
        let answer: RepairAdmission =
            serde_json::from_str(&storyhook::output::render_response(&response, true, false))
                .unwrap();
        if let RepairAdmission::Deferred {
            recovery_id,
            reason,
        } = answer
        {
            return VerificationOutcome::RepairDeferred {
                recovery_id,
                reason,
            };
        }
        self.executions.fetch_add(1, Ordering::SeqCst);
        if self.fail_tests {
            return VerificationOutcome::TestsFailed {
                tree: self.input.tree.clone(),
                log: "/tmp/gate.log".into(),
                detail: "regression failed".into(),
                gate: "gate".into(),
            };
        }
        VerificationOutcome::Certified {
            head: self.input.head.clone(),
            tree: if self.mismatch {
                "2".repeat(40)
            } else {
                self.input.tree.clone()
            },
            gate: "make test".into(),
            detail: "external gate certified input".into(),
        }
    }
    fn land(&self, _: &VerificationCandidate, _: &LandingIntent) -> LandingOutcome {
        LandingOutcome::Merged {
            detail: "remote confirms exact certified merge".into(),
        }
    }
    fn recover_landing(&self, _: &VerificationCandidate, _: &LandingIntent) -> LandingOutcome {
        panic!("no previous landing")
    }
    fn notify(&self, _: &VerificationCandidate, _: &str) -> Result<NotifyDelivery, AppError> {
        panic!("no repair delivery while verifier owns the attempt")
    }
    fn redispatch(&self, _: &VerificationCandidate, _: &ResumePlan) -> Result<(), AppError> {
        panic!("no dispatch while verifier owns the attempt")
    }
    fn reap(&self, _: &VerificationCandidate) -> Result<(), AppError> {
        Ok(())
    }
}

#[test]
fn queue_disposes_refusal_or_lands_only_the_admitted_repair_input() {
    for (unchanged, mismatch, expected) in [
        (true, false, TickResult::Returned),
        (false, true, TickResult::Halted),
        (false, false, TickResult::Completed),
    ] {
        let f = fixture();
        let view = decision::ready(&f);
        let ctx = f.ctx();
        ProjectRecoveryService::new(&ctx)
            .decide(
                &view.record.id,
                &decision::input(&view, RepairScope::SameStory),
            )
            .unwrap();
        StoryService::new(&ctx)
            .set_state("SH-1", "verifying", None, None, None)
            .unwrap();
        let activity = VerificationActivity::new();
        let gate = GateEndpoint {
            store: f.store(),
            env: f.env(),
            activity: &activity,
            input: RepairInput {
                base: "a".repeat(40),
                head: "b".repeat(40),
                head_tree: if unchanged {
                    "d".repeat(40)
                } else {
                    "e".repeat(40)
                },
                tree: "f".repeat(40),
            },
            mismatch,
            fail_tests: false,
            executions: AtomicUsize::new(0),
        };
        let result = tick_with_activity(
            f.store(),
            f.env(),
            &gate,
            &activity,
            &InFlight::new(f.env().clone()),
            f.project(),
        )
        .unwrap();
        assert_eq!(result, expected);
        assert!(
            activity.active_for(f.project()).is_none(),
            "repair disposition cannot retain verifier ownership"
        );
        assert_eq!(
            gate.executions.load(Ordering::SeqCst),
            usize::from(!unchanged)
        );
        let current = ProjectRecoveryService::new(&ctx)
            .show(&view.record.id)
            .unwrap();
        if unchanged {
            assert!(current.state.refusals[0].disposition.is_some());
            assert!(current.state.attempts.is_empty());
            assert!(
                f.store()
                    .read(|tx| tx.verification_incident(f.project()))
                    .unwrap()
                    .is_none()
            );
        } else if mismatch {
            assert!(current.state.attempts[0].completion.is_none());
            assert!(current.state.landing.is_none());
        } else {
            assert!(current.state.landing.is_some());
            assert!(current.state.attempts[0].judgment.is_some());
        }
    }
}

#[test]
fn queue_returns_failed_repair_without_synchronous_agent_delivery() {
    let f = fixture();
    let view = decision::ready(&f);
    let ctx = f.ctx();
    let recovery = ProjectRecoveryService::new(&ctx);
    recovery
        .decide(
            &view.record.id,
            &decision::input(&view, RepairScope::SameStory),
        )
        .unwrap();
    let activity = VerificationActivity::new();
    for n in 1..=3 {
        StoryService::new(&ctx)
            .set_state("SH-1", "verifying", None, None, None)
            .unwrap();
        let gate = GateEndpoint {
            store: f.store(),
            env: f.env(),
            activity: &activity,
            input: RepairInput {
                base: "a".repeat(40),
                head: format!("{n:040x}"),
                head_tree: format!("{:040x}", n + 10),
                tree: format!("{:040x}", n + 20),
            },
            mismatch: false,
            fail_tests: true,
            executions: AtomicUsize::new(0),
        };
        assert_eq!(
            tick_with_activity(
                f.store(),
                f.env(),
                &gate,
                &activity,
                &InFlight::new(f.env().clone()),
                f.project()
            )
            .unwrap(),
            TickResult::Returned
        );
        assert!(activity.active_for(f.project()).is_none());
        let current = recovery.show(&view.record.id).unwrap();
        assert_eq!(current.state.attempts.len(), n as usize);
        assert!(current.state.landing.is_none());
        assert!(
            f.store()
                .read(|tx| tx.verification_incident(f.project()))
                .unwrap()
                .is_none()
        );
        let row = f
            .store()
            .read(|tx| tx.story(f.project(), StoryNo::new(1)))
            .unwrap()
            .unwrap();
        assert_eq!(row.state, "in-progress");
        assert_eq!(row.awaiting.is_some(), n == 3);
    }
}
