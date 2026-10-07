use super::*;
use crate::service::{NewStoryInput, attribution::RustTarget};
use crate::store::{GateExecutionPurpose, GateInputs, SqliteStore};
use storyhook_test_support::ServiceFixture;
mod native;

struct Board {
    fixture: ServiceFixture,
    store: SqliteStore,
    env: Environment,
    candidate: VerificationCandidate,
    activity: VerificationActivity,
}

impl Board {
    fn new() -> Self {
        let fixture = ServiceFixture::new();
        let store = SqliteStore::open(fixture.store().path()).unwrap();
        let project = ProjectId::new(fixture.project().get());
        let env = Environment::at(fixture.cwd());
        let ctx = Ctx::new(&store, project, fixture.cwd(), env.clone()).no_hooks(true);
        let story = StoryService::new(&ctx)
            .create(&NewStoryInput {
                title: "durable native coordinator".into(),
                ..Default::default()
            })
            .unwrap();
        StoryService::new(&ctx)
            .set_state(&story.id, "verifying", None, None, None)
            .unwrap();
        let candidate = VerificationQueue::new(&store).next().unwrap().unwrap();
        Self {
            fixture,
            store,
            env,
            candidate,
            activity: VerificationActivity::new(),
        }
    }
    fn ctx(&self) -> Ctx<'_, SqliteStore> {
        Ctx::new(
            &self.store,
            self.candidate.project,
            self.fixture.cwd(),
            self.env.clone(),
        )
        .no_hooks(true)
    }
    fn owner(&self) -> VerificationGuard {
        self.activity
            .try_acquire(&self.store, &self.env, &self.candidate, self.env.now())
            .unwrap()
            .unwrap()
    }
    fn fail(&self, owner: &VerificationGuard) -> RustDiagnosisRequest {
        let log = self.fixture.cwd().join("gate.log");
        std::fs::write(&log, "unsupported original output\n").unwrap();
        cost::execute(
            &self.store,
            &self.env,
            owner,
            &self.candidate,
            GateExecutionPurpose::Gate,
            GateInputs::default(),
            vec![cost::submission(&self.candidate)],
            |_| VerificationOutcome::TestsFailed {
                tree: "a".repeat(40),
                log: log.display().to_string(),
                detail: "failed gate".into(),
                gate: "fixture".into(),
            },
            |outcome| Ok(Some(outcome.clone())),
        )
        .unwrap();
        let execution = self
            .store
            .read(|tx| tx.gate_attempts(self.candidate.project))
            .unwrap()[0]
            .executions[0]
            .id
            .clone();
        RustDiagnosisRequest {
            execution,
            log,
            case: RustCase::new(
                "subject",
                RustTarget::Integration("contract".into()),
                "answer",
            )
            .unwrap(),
            intervention: TreeIntervention::Unchanged,
            fixture: None,
        }
    }
}

#[test]
fn unsupported_original_is_held_durably_without_diagnostic_launch() {
    let b = Board::new();
    let owner = b.owner();
    let request = b.fail(&owner);
    let before = b
        .store
        .read(|tx| tx.gate_attempts(b.candidate.project))
        .unwrap();
    let result = owner
        .diagnose_rust_failure(&b.ctx(), &b.candidate, request)
        .unwrap();
    let RustDiagnosisResult::Held { evidence, detail } = result else {
        panic!("unsupported original must remain held")
    };
    assert!(detail.contains("original"), "{detail}");
    let reopened = SqliteStore::open(b.store.path()).unwrap();
    let records = reopened
        .read(|tx| tx.attributions(b.candidate.project))
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, evidence);
    assert!(records[0].held);
    assert!(records[0].preparation.is_none());
    assert!(records[0].probes.is_empty());
    assert_eq!(
        reopened
            .read(|tx| tx.gate_attempts(b.candidate.project))
            .unwrap(),
        before
    );
    assert!(VerificationQueue::new(&reopened).next().unwrap().is_none());
}

#[test]
fn cancelled_or_changed_authority_does_not_start_or_write_diagnosis() {
    for change in ["cancel", "stop", "stop-start", "resubmit", "human-only"] {
        let b = Board::new();
        let owner = b.owner();
        let request = b.fail(&owner);
        match change {
            "cancel" => owner.cancellation.cancel(),
            "stop" | "stop-start" => b
                .store
                .write(|tx| {
                    tx.put_verification_enabled(b.candidate.project, false)?;
                    if change == "stop-start" {
                        tx.put_verification_enabled(b.candidate.project, true)?;
                    }
                    Ok(())
                })
                .unwrap(),
            "resubmit" => {
                let ctx = b.ctx();
                let s = StoryService::new(&ctx);
                s.set_state(&b.candidate.story_id, "in-progress", None, None, None)
                    .unwrap();
                s.set_state(&b.candidate.story_id, "verifying", None, None, None)
                    .unwrap();
            }
            "human-only" => {
                StoryService::new(&b.ctx())
                    .set_labels(&b.candidate.story_id, &["human-only".into()], &[])
                    .unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            matches!(
                owner
                    .diagnose_rust_failure(&b.ctx(), &b.candidate, request)
                    .unwrap(),
                RustDiagnosisResult::Superseded
            ),
            "{change}"
        );
        assert!(
            b.store
                .read(|tx| tx.attributions(b.candidate.project))
                .unwrap()
                .is_empty(),
            "{change}"
        );
    }
}

#[test]
fn a_revision_zero_record_survives_reentry_without_replay() {
    let b = Board::new();
    let owner = b.owner();
    let request = b.fail(&owner);
    let (original, control) = record::original(&b.store, &b.candidate, &owner)
        .unwrap()
        .unwrap();
    let selection = record::select(&original, &request);
    record::begin(
        &b.ctx(),
        &b.candidate,
        &owner,
        control,
        &original,
        &request,
        &selection,
    )
    .unwrap()
    .unwrap();
    let before = b
        .store
        .read(|tx| tx.attributions(b.candidate.project))
        .unwrap();
    assert_eq!(before[0].revision, 0);
    assert!(matches!(
        owner
            .diagnose_rust_failure(&b.ctx(), &b.candidate, request)
            .unwrap(),
        RustDiagnosisResult::Held { .. }
    ));
    assert_eq!(
        b.store
            .read(|tx| tx.attributions(b.candidate.project))
            .unwrap(),
        before
    );
}

#[test]
fn unnamed_failed_legs_are_preserved_beside_named_cases() {
    let b = Board::new();
    let owner = b.owner();
    let request = b.fail(&owner);
    let (mut original, control) = record::original(&b.store, &b.candidate, &owner)
        .unwrap()
        .unwrap();
    original.failed_cases.push(crate::store::GateFailedCase {
        path: "rust-suite".into(),
        name: Some("answer".into()),
        target: Some("contract".into()),
        identity: None,
        title_path: None,
    });
    original.legs.push(crate::store::GateLeg {
        path: "lint".into(),
        status: "failed".into(),
        milliseconds: Some(1),
        receipt: None,
    });
    record::begin(
        &b.ctx(),
        &b.candidate,
        &owner,
        control,
        &original,
        &request,
        &Err("unsupported output".into()),
    )
    .unwrap()
    .unwrap();
    let retained = b
        .store
        .read(|tx| tx.attributions(b.candidate.project))
        .unwrap();
    assert_eq!(retained[0].components.len(), 2);
    assert!(
        retained[0]
            .components
            .iter()
            .any(|c| c.check == "original-leg:lint")
    );
}

#[test]
fn nonregular_original_output_cannot_wait_for_a_fifo_writer() {
    use std::os::unix::fs::OpenOptionsExt;
    let b = Board::new();
    let owner = b.owner();
    let mut request = b.fail(&owner);
    let (mut original, _) = record::original(&b.store, &b.candidate, &owner)
        .unwrap()
        .unwrap();
    original.failed_cases.push(crate::store::GateFailedCase {
        path: "rust-suite".into(),
        name: Some("answer".into()),
        target: Some("contract".into()),
        identity: None,
        title_path: None,
    });
    request.log = b.fixture.cwd().join("original-fifo");
    original.logs.push(request.log.display().to_string());
    let path = std::ffi::CString::new(request.log.to_str().unwrap()).unwrap();
    // Fixture FIFO has no writer. Cleanup below also releases a regressed blocking reader.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    std::thread::scope(|scope| {
        let (sent, received) = std::sync::mpsc::channel();
        let request_ref = &request;
        scope.spawn(move || {
            let result = record::select(&original, request_ref);
            sent.send(result).unwrap();
        });
        let result = received.recv_timeout(storyhook_test_support::load_grace::graced_now(
            crate::daemon::lifecycle::CONTROL_DEADLINE,
        ));
        let _release = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&request.log)
            .unwrap();
        let error = result
            .expect("original-file inspection blocked on a FIFO writer")
            .unwrap_err();
        assert!(error.contains("regular file"), "{error}");
    });
}
