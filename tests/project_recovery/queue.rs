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
    project_fault: bool,
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
        if self.project_fault {
            let storyhook::service::project_recovery::RepairJudgment::ProjectFault { fault } =
                attempts::judgment(
                    &self.input,
                    storyhook::service::project_recovery::RepairCompletion::ProjectFault,
                )
            else {
                panic!("fault")
            };
            return VerificationOutcome::ProjectFault { fault };
        }
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
            project_fault: false,
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
            let status = activity.status(&ctx).unwrap();
            assert_eq!(status.project_recoveries[0].phase, "landed");
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
            project_fault: false,
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

#[test]
fn a_project_fault_releases_the_queue_and_retains_unjudged_submission() {
    let f = fixture();
    submitted(&f, "faulting submission");
    submitted(&f, "unrelated queued work");
    let activity = VerificationActivity::new();
    let mut endpoint = GateEndpoint {
        store: f.store(),
        env: f.env(),
        activity: &activity,
        input: RepairInput {
            base: "a".repeat(40),
            head: "b".repeat(40),
            head_tree: "c".repeat(40),
            tree: "d".repeat(40),
        },
        mismatch: false,
        fail_tests: false,
        project_fault: true,
        executions: AtomicUsize::new(0),
    };
    assert_eq!(
        tick_with_activity(
            f.store(),
            f.env(),
            &endpoint,
            &activity,
            &InFlight::new(f.env().clone()),
            f.project()
        )
        .unwrap(),
        TickResult::Returned
    );
    assert!(activity.active_for(f.project()).is_none());
    assert!(
        f.store()
            .read(|tx| tx.verification_incident(f.project()))
            .unwrap()
            .is_none()
    );
    let records = f
        .store()
        .read(|tx| tx.project_recoveries(f.project()))
        .unwrap();
    assert_eq!(records.len(), 1);
    let ctx = f.ctx();
    let view = ProjectRecoveryService::new(&ctx)
        .show(&records[0].id)
        .unwrap();
    assert_eq!(view.state.assessment.status, AssessmentStatus::Pending);
    assert_eq!(view.state.subjects[0].candidate.story_id, "SH-1");
    endpoint.project_fault = false;
    assert_eq!(
        tick_with_activity(
            f.store(),
            f.env(),
            &endpoint,
            &activity,
            &InFlight::new(f.env().clone()),
            f.project()
        )
        .unwrap(),
        TickResult::Completed
    );
    assert_eq!(
        f.store()
            .read(|tx| tx.story(f.project(), StoryNo::new(2)))
            .unwrap()
            .unwrap()
            .state,
        "done"
    );
    assert_eq!(
        f.store()
            .read(|tx| tx.story(f.project(), StoryNo::new(1)))
            .unwrap()
            .unwrap()
            .state,
        "in-progress"
    );
}

#[test]
fn no_auto_fault_is_not_reexecuted_or_changed_and_other_work_can_advance() {
    let f = fixture();
    submitted(&f, "reserved assessment");
    let ctx = f.ctx();
    StoryService::new(&ctx)
        .set_labels("SH-1", &["no-auto".into()], &[])
        .unwrap();
    let activity = VerificationActivity::new();
    let mut endpoint = GateEndpoint {
        store: f.store(),
        env: f.env(),
        activity: &activity,
        input: RepairInput {
            base: "a".repeat(40),
            head: "b".repeat(40),
            head_tree: "c".repeat(40),
            tree: "d".repeat(40),
        },
        mismatch: false,
        fail_tests: false,
        project_fault: true,
        executions: AtomicUsize::new(0),
    };
    for _ in 0..2 {
        tick_with_activity(
            f.store(),
            f.env(),
            &endpoint,
            &activity,
            &InFlight::new(f.env().clone()),
            f.project(),
        )
        .unwrap();
    }
    assert_eq!(endpoint.executions.load(Ordering::SeqCst), 1);
    let row = f
        .store()
        .read(|tx| tx.story(f.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "verifying");
    assert!(row.awaiting.is_none());
    assert!(row.snapshot.labels.iter().any(|l| l == "no-auto"));
    submitted(&f, "unrelated submission");
    endpoint.project_fault = false;
    assert_eq!(
        tick_with_activity(
            f.store(),
            f.env(),
            &endpoint,
            &activity,
            &InFlight::new(f.env().clone()),
            f.project()
        )
        .unwrap(),
        TickResult::Completed
    );
    assert_eq!(endpoint.executions.load(Ordering::SeqCst), 2);
    assert!(
        f.store()
            .read(|tx| tx.verification_incident(f.project()))
            .unwrap()
            .is_none()
    );
}

#[test]
fn legacy_halts_need_typed_corroboration_and_keep_the_original_incident() {
    for proven in [false, true] {
        let f = fixture();
        let candidate = submitted(&f, "legacy incident");
        let ctx = f.ctx();
        let old = storyhook::store::VerificationIncident {
            incident_id: "old-halt".into(),
            project: f.project(),
            story: StoryNo::new(1),
            generation: candidate.verifying_generation.unwrap(),
            disposition: storyhook::store::VerificationFailureDisposition::Permanent,
            halted: true,
            attempts: 1,
            detail: serde_json::to_string(&fault()).unwrap(),
            first_failed_at: ctx.now(),
            last_failed_at: ctx.now(),
        };
        f.store()
            .write(|tx| tx.put_verification_incident(&old))
            .unwrap();
        let service = ProjectRecoveryService::new(&ctx);
        let view = proven.then(|| {
            service
                .observe(&candidate, &fault(), "retained-typed-attempt")
                .unwrap()
                .unwrap()
        });
        let activity = VerificationActivity::new();
        let gate = GateEndpoint {
            store: f.store(),
            env: f.env(),
            activity: &activity,
            input: RepairInput {
                base: "a".repeat(40),
                head: "b".repeat(40),
                head_tree: "e".repeat(40),
                tree: "f".repeat(40),
            },
            mismatch: false,
            fail_tests: false,
            project_fault: false,
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
        assert_eq!(gate.executions.load(Ordering::SeqCst), 0);
        if let Some(view) = view {
            assert_ne!(result, TickResult::Halted);
            let current = service.show(&view.record.id).unwrap();
            let json = serde_json::to_value(current).unwrap();
            assert_eq!(json["state"]["legacy_incidents"], serde_json::json!([old]));
            assert!(
                f.store()
                    .read(|tx| tx.verification_incident(f.project()))
                    .unwrap()
                    .is_none()
            );
        } else {
            assert_eq!(result, TickResult::Halted);
            assert_eq!(
                f.store()
                    .read(|tx| tx.verification_incident(f.project()))
                    .unwrap(),
                Some(old)
            );
            assert!(
                f.store()
                    .read(|tx| tx.project_recoveries(f.project()))
                    .unwrap()
                    .is_empty()
            );
        }
    }
}

#[test]
fn project_fault_to_managed_repair_landing_and_fresh_verification() {
    use std::sync::atomic::AtomicBool;
    use storyhook::daemon::project_recovery::process_one;
    for scope in [RepairScope::SameStory, RepairScope::SeparateStory] {
        let f = fixture();
        let original = submitted(&f, "complete fault recovery flow");
        let activity = VerificationActivity::new();
        let inflight = InFlight::new(f.env().clone());
        let mut gate = GateEndpoint {
            store: f.store(),
            env: f.env(),
            activity: &activity,
            input: RepairInput {
                base: "a".repeat(40),
                head: "b".repeat(40),
                head_tree: "d".repeat(40),
                tree: "c".repeat(40),
            },
            mismatch: false,
            fail_tests: false,
            project_fault: true,
            executions: AtomicUsize::new(0),
        };
        assert_eq!(
            tick_with_activity(f.store(), f.env(), &gate, &activity, &inflight, f.project())
                .unwrap(),
            TickResult::Returned
        );
        assert!(activity.active_for(f.project()).is_none());
        let ctx = f.ctx();
        let service = ProjectRecoveryService::new(&ctx);
        let id = f
            .store()
            .read(|tx| tx.project_recoveries(f.project()))
            .unwrap()[0]
            .id
            .clone();
        let delivery = worker::helper(&f, r#"{"ok":true}"#);
        let stop = AtomicBool::new(false);
        assert!(process_one(f.store(), f.env(), &delivery, &activity, &stop).unwrap());
        let assessed = service.show(&id).unwrap();
        assert_eq!(
            assessed.state.assessment.status,
            AssessmentStatus::Delivered
        );
        let mut input = decision::input(&assessed, scope);
        input.evidence[0] = format!("attempt:{}", assessed.observations[0].attempt_id);
        let decided = service.decide(&id, &input).unwrap();
        assert!(process_one(f.store(), f.env(), &delivery, &activity, &stop).unwrap());
        let repair = decided
            .state
            .decision
            .unwrap()
            .repair_story
            .unwrap()
            .to_id("SH");
        if scope == RepairScope::SeparateStory {
            assert!(
                f.store()
                    .read(|tx| tx.story(f.project(), StoryNo::new(1)))
                    .unwrap()
                    .unwrap()
                    .awaiting
                    .is_some()
            );
            PrLinkService::new(&ctx)
                .link(&repair, "https://github.com/acme/widgets/pull/2", true)
                .unwrap();
        }
        StoryService::new(&ctx)
            .set_state(&repair, "verifying", None, None, None)
            .unwrap();
        gate.input.head_tree = "e".repeat(40);
        gate.input.head = "f".repeat(40);
        gate.input.tree = "1".repeat(40);
        gate.project_fault = false;
        assert_eq!(
            tick_with_activity(f.store(), f.env(), &gate, &activity, &inflight, f.project())
                .unwrap(),
            TickResult::Completed
        );
        let landed = service.show(&id).unwrap();
        assert!(landed.state.landing.is_some());
        assert!(!landed.record.active);
        assert_eq!(
            landed
                .state
                .attempts
                .iter()
                .filter(|a| a.completion.is_some())
                .count(),
            1
        );
        if scope == RepairScope::SeparateStory {
            // First wake releases only its owned dependency; next wake delivers resume.
            assert!(process_one(f.store(), f.env(), &delivery, &activity, &stop).unwrap());
            assert!(process_one(f.store(), f.env(), &delivery, &activity, &stop).unwrap());
            let row = f
                .store()
                .read(|tx| tx.story(f.project(), StoryNo::new(1)))
                .unwrap()
                .unwrap();
            assert!(row.awaiting.is_none());
            assert_eq!(row.state, "in-progress");
            StoryService::new(&ctx)
                .set_state("SH-1", "verifying", None, None, None)
                .unwrap();
            let refreshed = VerificationQueue::new(f.store()).next().unwrap().unwrap();
            assert_ne!(
                refreshed.verifying_generation,
                original.verifying_generation
            );
            gate.input.tree = "2".repeat(40);
            assert_eq!(
                tick_with_activity(f.store(), f.env(), &gate, &activity, &inflight, f.project())
                    .unwrap(),
                TickResult::Completed
            );
        }
        assert_eq!(
            f.store()
                .read(|tx| tx.story(f.project(), StoryNo::new(1)))
                .unwrap()
                .unwrap()
                .state,
            "done"
        );
        assert!(
            f.store()
                .read(|tx| tx.verification_incident(f.project()))
                .unwrap()
                .is_none()
        );
        assert!(!process_one(f.store(), f.env(), &delivery, &activity, &stop).unwrap());
    }
}
