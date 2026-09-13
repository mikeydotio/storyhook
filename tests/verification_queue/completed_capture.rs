//! Completion emitted during supervisor termination must survive capture.

use super::*;
use storyhook::daemon::verification::VerificationCancellation;

#[test]
fn interrupted_capture_preserves_completed_cleanup_results() {
    for with_cleanup in [false, true] {
        for result in ["tests-failed", "gate-passed", "merged", "incomplete"] {
            let (checkout, tools) = recording_checkout();
            let (candidate, pr) = shell_actuator_candidate(checkout.path());
            let mut payload = serde_json::json!({
                "result": result, "tree": "judged-tree", "log": "/tmp/attempt.log",
                "detail": "named_failure FAILED", "cleanup_failure": {
                    "phase": "restoration", "detail": "retained writers",
                    "owner": "/tmp/owner", "worktree": "/tmp/verifier",
                    "disposition": "permanent"
                }
            });
            if !with_cleanup {
                payload.as_object_mut().unwrap().remove("cleanup_failure");
            }
            std::fs::write(checkout.path().join("payload"), payload.to_string()).unwrap();
            std::fs::write(tools.path().join("verify-pr.sh"),
                "#!/bin/bash\ntrap 'cat payload; exit 143' TERM\nprintf ready > ready\nwhile :; do sleep 30 & wait; done\n").unwrap();
            let root = scratch_dir();
            let env = Environment::at(root.path());
            let idle = Duration::from_secs(1);
            let actuator = ShellVerificationActuator::with_paths_and_timing(
                env.clone(),
                checkout.path().join("unused-helper"),
                PathBuf::from("/usr/bin/true"),
                idle,
                idle,
                idle,
            )
            .with_verifier_script(tools.path().join("verify-pr.sh"));
            let cancellation = VerificationCancellation::default();
            let outcome = actuator.verify_cancellable(&candidate, &pr, &cancellation);
            assert!(lifecycle::read_owned_processes(&env).is_empty());
            if result == "incomplete" {
                assert!(
                    matches!(
                        outcome,
                        VerificationOutcome::Cancelled
                            | VerificationOutcome::InfrastructureFailure { .. }
                    ),
                    "{outcome:?}"
                );
            } else {
                let VerificationOutcome::CleanupFailed { cleanup, .. } = outcome else {
                    panic!("result={result}: {outcome:?}");
                };
                assert!(cleanup.detail.contains("capture failed"));
                assert_eq!(cleanup.owner.is_some(), with_cleanup);
                assert_eq!(cleanup.worktree.is_some(), with_cleanup);
            }
        }
    }
}

struct StopAfterCompletion<'a> {
    fixture: &'a ServiceFixture,
    activity: &'a VerificationActivity,
    inner: FakeActuator,
}

impl VerificationActuator for StopAfterCompletion<'_> {
    fn submit(
        &self,
        candidate: &VerificationCandidate,
    ) -> Result<SubmittedPullRequest, SubmissionFailure> {
        adopt_linked(candidate)
    }
    fn verify(&self, candidate: &VerificationCandidate, _pr: &PrLink) -> VerificationOutcome {
        self.activity
            .control(
                self.fixture.store(),
                candidate.project,
                VerificationAction::Stop,
            )
            .unwrap();
        self.inner.outcome.clone()
    }
    fn notify(
        &self,
        _candidate: &VerificationCandidate,
        _message: &str,
    ) -> Result<NotifyDelivery, AppError> {
        panic!("cleanup uncertainty prohibits notification")
    }
    fn redispatch(
        &self,
        _candidate: &VerificationCandidate,
        _plan: &ResumePlan,
    ) -> Result<(), AppError> {
        panic!("cleanup uncertainty prohibits dispatch")
    }
    fn reap(&self, _candidate: &VerificationCandidate) -> Result<(), AppError> {
        panic!("cleanup uncertainty prohibits reaping")
    }
}

#[test]
fn late_manual_stop_keeps_the_completed_verdict_and_halt() {
    use storyhook::daemon::verification::{CompletedVerification, VerificationCleanupFailure};
    for passed in [false, true] {
        let fixture = ServiceFixture::new();
        fixture.link_origin("https://github.com/acme/widgets");
        let id = submitted(&fixture, "completed before stop", Priority::High, PR_ONE);
        let activity = VerificationActivity::default();
        let verdict = if passed {
            CompletedVerification::GatePassed {
                tree: "t".into(),
                log: "/tmp/log".into(),
                detail: "passed".into(),
                gate: "gate".into(),
            }
        } else {
            CompletedVerification::TestsFailed {
                tree: "t".into(),
                log: "/tmp/log".into(),
                detail: "named_failure FAILED".into(),
                gate: "gate".into(),
            }
        };
        let actuator = StopAfterCompletion {
            fixture: &fixture,
            activity: &activity,
            inner: FakeActuator::new(VerificationOutcome::CleanupFailed {
                verdict,
                cleanup: VerificationCleanupFailure {
                    phase: "restoration".into(),
                    detail: "retained writers".into(),
                    owner: Some("/tmp/owner".into()),
                    worktree: Some("/tmp/verifier".into()),
                    disposition: VerificationFailureDisposition::Permanent,
                },
            }),
        };
        assert_eq!(
            tick_with_activity(
                fixture.store(),
                fixture.env(),
                &actuator,
                &activity,
                &InFlight::new(fixture.env().clone()),
                fixture.project()
            )
            .unwrap(),
            TickResult::Halted
        );
        let row = story_row(&fixture, &id);
        assert_eq!(row.state, "verifying");
        let prefix = if passed {
            "CENTRAL VERIFICATION GATE PASSED"
        } else {
            "CENTRAL VERIFICATION RED"
        };
        assert!(
            row.snapshot
                .comments
                .iter()
                .any(|c| c.text.starts_with(prefix))
        );
        assert!(
            !row.snapshot
                .comments
                .iter()
                .any(|c| c.text.contains("judged nothing"))
        );
        assert!(
            fixture
                .store()
                .read(|tx| tx.verification_incident(fixture.project()))
                .unwrap()
                .unwrap()
                .halted
        );
    }
}
