//! Deterministic protocol/admission detectors. The private fixture constructor
//! stands in for authenticated transport only; real broker peer/ledger tests
//! live in test_host_restoration.py. No public JSON can construct these types.
use super::*;
use crate::{
    service::attribution::FailureComponent,
    service::{NewStoryInput, PrLinkService, StoryService, host_recovery::HostRecoveryService},
    store::{GateAttempt, GateInputs, GateSubmission, ProjectId, SqliteStore, WriteOps},
};
use serde_json::json;

const AT: &str = "2026-10-08T00:00:00Z";

struct Fixture {
    fixture: storyhook_test_support::ServiceFixture,
    store: SqliteStore,
    subject: Subject,
    raw: Vec<u8>,
}
impl Fixture {
    fn new() -> Self {
        let fixture = storyhook_test_support::ServiceFixture::new();
        let root = fixture.github_checkout("https://github.com/acme/widgets");
        let store = SqliteStore::open(fixture.store().path()).unwrap();
        let project = ProjectId::new(fixture.project().get());
        let ctx = Ctx::new(
            &store,
            project,
            &root,
            Environment::at(fixture.cwd()).with_subprocess_patience(),
        )
        .no_hooks(true);
        let stories = StoryService::new(&ctx);
        let story = stories
            .create(&NewStoryInput {
                title: "native host retained submission".into(),
                ..Default::default()
            })
            .unwrap()
            .id;
        PrLinkService::new(&ctx)
            .link(&story, "https://github.com/acme/widgets/pull/1", true)
            .unwrap();
        stories
            .set_state(&story, "verifying", None, None, None)
            .unwrap();
        // This fixture exercises store authority, not external origin lookup.
        let candidate = store
            .read(|tx| crate::service::verification::ordered_candidates_for(tx, project))
            .unwrap()
            .pop()
            .unwrap();
        let binding = json!({"attempt_id":"host-attempt","execution_id":"host-gate","generation":candidate.verifying_generation.unwrap().get()});
        let event = |kind: &str, sequence: u64| json!({"version":1,"authority":"native-authority","host":"native-host","boot":"native-boot","policy":"a".repeat(64),"sequence":sequence,"at":sequence,"event":kind,"lease":"root","parent":null,"project":root.join(".git"),"work":"test","resources":{"cpu":1,"memory":1},"binding":binding,"reason":"severe pressure"});
        let mut cancel = event("cancel", 12);
        cancel["pressure_fault_sequence"] = json!(11);
        let events = vec![event("request", 10), cancel, event("release", 13)];
        let mut lines = vec![
            json!({"kind":"run","attempt_id":"host-attempt","execution_id":"host-gate","generation":candidate.verifying_generation.unwrap().get()}),
            json!({"kind":"admission","entry":"verifier-gate","cause":"pressure","retryable":true,"reason":"severe pressure"}),
        ];
        lines.extend(events.iter().map(|event|json!({"kind":"resource","attempt_id":"host-attempt","execution_id":"host-gate","generation":candidate.verifying_generation.unwrap().get(),"observation":event})));
        let raw = lines
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            .into_bytes();
        let path = fixture.cwd().join("host-gate.ndjson");
        fs::write(&path, &raw).unwrap();
        let submission = GateSubmission {
            project,
            story_id: story,
            generation: candidate.verifying_generation,
            submitted_at: candidate.verifying_since.clone(),
        };
        let mut gate = GateExecution::new("host-gate".into(), AT, path.display().to_string());
        gate.finished_at = Some(AT.into());
        gate.milliseconds = Some(1);
        gate.verdict = Some("infrastructure-failure".into());
        gate.journal_bound = true;
        gate.submissions = vec![submission.clone()];
        gate.inputs = GateInputs {
            head: Some("a".repeat(40)),
            base: Some("b".repeat(40)),
            tree: Some("c".repeat(40)),
            ..Default::default()
        };
        gate.resource_events = events;
        let mut attempt = GateAttempt::new("host-attempt".into(), submission.clone(), AT);
        attempt.finished_at = Some(AT.into());
        attempt.control_revision = Some(0);
        attempt.verdict = Some("infrastructure-failure".into());
        attempt.executions.push(gate.clone());
        let record = AttributionRecord {
            version: 1,
            id: "host-attribution".into(),
            revision: 0,
            submission,
            attempt: attempt.id.clone(),
            inputs: gate.inputs.clone(),
            created_at: AT.into(),
            components: vec![FailureComponent {
                id: "pressure".into(),
                check: "native-host-pressure".into(),
                signature: "severe pressure".into(),
                requirement: "settled native pressure restoration".into(),
                log: path.display().to_string(),
                observed_cause: FailureCause::HostExternal,
            }],
            preparation: None,
            settlement: None,
            plans: vec![],
            probes: vec![],
            assessments: vec![],
            diagnosis_ms: 0,
            held: true,
            retired: None,
        };
        store
            .write(|tx| {
                let mut live = attempt.clone();
                live.finished_at = None;
                live.verdict = None;
                live.executions.clear();
                tx.insert_gate_attempt(&live)?;
                attempt.revision = 1;
                assert!(tx.update_gate_attempt(&attempt, 0)?);
                tx.insert_attribution(&record)
            })
            .unwrap();
        let mut subject = store
            .read(|tx| capture(tx, &candidate, &record.id, &gate.id, "pressure"))
            .unwrap();
        subject.repository_common = root.join(".git");
        Self {
            fixture,
            store,
            subject,
            raw,
        }
    }
    fn ctx(&self) -> Ctx<'_, SqliteStore> {
        Ctx::new(
            &self.store,
            self.subject.candidate.project,
            self.fixture.cwd(),
            Environment::at(self.fixture.cwd()).with_subprocess_patience(),
        )
        .no_hooks(true)
    }
    fn proof(&self, restored: bool) -> Live {
        let mut subject = self.subject.clone();
        subject.request.operation = if restored {
            "restoration-proof"
        } else {
            "fault-proof"
        }
        .into();
        subject.request.nonce = uuid::Uuid::new_v4().to_string();
        let now = monotonic_ms().unwrap();
        let mut reply = json!({"version":1,"kind":if restored {"host-pressure-restoration"}else{"host-pressure-fault"},"nonce":subject.request.nonce,"fault":subject.request.fault,"window":subject.request.window,"affected":subject.request.affected,"broker":{"pid":1,"start":"native-start","boot":"native-boot"},"checked_at":now,"timing":{"stale_ms":60000},"settled":[{"lease":"root","binding":subject.request.affected[0].binding,"project":subject.repository_common,"work":"test","state":"released","pressure_links":[{"sequence":12,"event":"cancel","reason":"severe pressure","pressure_fault_sequence":11}]}]});
        if restored {
            reply["sample"] = json!({"at":now});
        }
        validate_reply(&subject, &reply, now, restored).unwrap();
        Live {
            subject,
            reply,
            deadline: Instant::now() + Duration::from_secs(60),
            cancellation: Cancellation::default(),
        }
    }
}

#[test]
fn native_host_factory_rejects_historical_pressure_wrong_binding_and_missing_raw_cause() {
    let f = Fixture::new();
    let request = derive_request(
        "host-attempt",
        &f.subject.execution,
        f.subject.candidate.verifying_generation.map(GlobalSeq::get),
        &f.raw,
    )
    .unwrap();
    assert_eq!(request.affected.len(), 1);
    for change in ["historical", "raw-cause", "binding", "unfinished-root"] {
        let mut gate = f.subject.execution.clone();
        let mut raw = f.raw.clone();
        match change {
            "historical" => {
                gate.resource_events[1]
                    .as_object_mut()
                    .unwrap()
                    .remove("pressure_fault_sequence");
            }
            "raw-cause" => {
                raw = String::from_utf8(raw)
                    .unwrap()
                    .replace("severe pressure", "unrelated failure")
                    .into_bytes();
            }
            "binding" => {
                raw = String::from_utf8(raw)
                    .unwrap()
                    .replace("host-gate", "other-gate")
                    .into_bytes();
            }
            "unfinished-root" => {
                gate.resource_events.truncate(1);
            }
            _ => unreachable!(),
        }
        assert!(
            derive_request(
                "host-attempt",
                &gate,
                f.subject.candidate.verifying_generation.map(GlobalSeq::get),
                &raw
            )
            .is_err(),
            "accepted {change}"
        );
    }
}

#[test]
fn native_host_reply_requires_exact_project_cause_set_and_consumption_freshness() {
    let f = Fixture::new();
    let live = f.proof(true);
    let now = number(&live.reply, "checked_at").unwrap();
    for change in [
        "project",
        "cause",
        "lease",
        "duplicate",
        "nonce",
        "fault-kind",
        "stale",
        "future",
    ] {
        let mut reply = live.reply.clone();
        let mut at = now;
        match change {
            "project" => reply["settled"][0]["project"] = json!("/another/project/.git"),
            "cause" => reply["settled"][0]["pressure_links"] = json!([]),
            "lease" => reply["settled"][0]["lease"] = json!("unrelated-root"),
            "duplicate" => {
                let row = reply["settled"][0].clone();
                reply["settled"].as_array_mut().unwrap().push(row);
            }
            "nonce" => reply["nonce"] = json!("replayed-query"),
            "fault-kind" => reply["kind"] = json!("host-pressure-fault"),
            "stale" => at += 60001,
            "future" => reply["checked_at"] = json!(now + 1),
            _ => unreachable!(),
        }
        assert!(
            validate_reply(&live.subject, &reply, at, true).is_err(),
            "accepted {change}"
        );
    }
}

#[test]
fn host_owner_restarts_once_and_readmits_only_original_head_with_fresh_restoration() {
    let f = Fixture::new();
    let ctx = f.ctx();
    let service = HostRecoveryService::new(&ctx);
    let fault = HostFaultEvidence {
        live: f.proof(false),
    };
    let owner = service.enroll(&fault).unwrap();
    assert!(owner.active);
    assert_eq!(owner.submissions, 1);
    assert_eq!(owner.readmitted, 0);
    assert_eq!(service.enroll(&fault).unwrap(), owner);
    let reopened = SqliteStore::open(f.store.path()).unwrap();
    assert!(
        reopened
            .read(super::super::owner::blocks_admission)
            .unwrap()
    );
    let restored = HostRestorationEvidence {
        live: f.proof(true),
    };
    let view = service
        .restore(&owner.id, owner.revision, &restored)
        .unwrap()
        .unwrap();
    assert!(!view.active);
    assert_eq!(view.started_at, owner.started_at);
    assert_eq!(view.readmitted, 1);
    assert!(
        !reopened
            .read(super::super::owner::blocks_admission)
            .unwrap()
    );
    // The next central gate records its own admission before pinned-input
    // validation. It must not be mistaken for replacement author evidence.
    let mut next = GateAttempt::new(
        "fresh-after-host-restoration".into(),
        f.subject.attribution.submission.clone(),
        AT,
    );
    next.control_revision = Some(f.subject.control);
    reopened.write(|tx| tx.insert_gate_attempt(&next)).unwrap();
    reopened
        .read(|tx| super::super::owner::check_input(tx, &f.subject.candidate, &"a".repeat(40)))
        .unwrap();
    assert!(
        reopened
            .read(|tx| super::super::owner::check_input(tx, &f.subject.candidate, &"d".repeat(40)))
            .is_err()
    );
    let row = reopened
        .read(|tx| {
            tx.story(
                f.subject.candidate.project,
                f.subject.attribution.submission.story_number().unwrap(),
            )
        })
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "verifying");
    assert!(row.awaiting.is_none());
    assert_eq!(
        reopened
            .read(|tx| crate::service::verification::verifying_entry(
                tx,
                f.subject.candidate.project,
                f.subject.attribution.submission.story_number().unwrap()
            ))
            .unwrap()
            .map(|(_, generation)| generation),
        f.subject.candidate.verifying_generation
    );
}

#[test]
fn native_host_archive_refuses_fifo_without_blocking_store_admission() {
    use std::{ffi::CString, os::unix::ffi::OsStrExt, sync::mpsc};
    let root = storyhook_test_support::scratch_dir();
    let path = root.path().join("substituted-journal");
    let name = CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: the NUL-terminated path belongs to this isolated scratch fixture.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let (send, receive) = mpsc::channel();
    let target = path.clone();
    let worker = std::thread::spawn(move || {
        send.send(Archive::capture(&target).map(|_| ())).unwrap();
    });
    let patience = storyhook_test_support::load_grace::graced_now(Duration::from_secs(10));
    let answer = receive.recv_timeout(patience);
    if answer.is_err() {
        // A negative control that removes O_NONBLOCK waits for a FIFO writer.
        // Release that fixture-owned open before failing, without reading it.
        let writer = fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path);
        if writer.is_ok() && receive.recv_timeout(patience).is_ok() {
            worker.join().unwrap();
        }
        panic!("special-file replacement blocked native evidence capture");
    }
    worker.join().unwrap();
    assert!(
        answer.unwrap().is_err(),
        "FIFO was accepted as a retained native journal"
    );
}

#[test]
fn host_restoration_preserves_manual_control_and_tampered_raw_holds() {
    for change in ["cancel", "raw", "stop", "stale"] {
        let f = Fixture::new();
        let ctx = f.ctx();
        let service = HostRecoveryService::new(&ctx);
        let owner = service
            .enroll(&HostFaultEvidence {
                live: f.proof(false),
            })
            .unwrap();
        let mut proof = HostRestorationEvidence {
            live: f.proof(true),
        };
        match change {
            "cancel" => proof.live.cancellation.cancel(),
            "raw" => fs::write(&f.subject.archive.path, "changed retained journal").unwrap(),
            "stop" => f
                .store
                .write(|tx| tx.put_verification_enabled(f.subject.candidate.project, false))
                .unwrap(),
            "stale" => proof.live.deadline = Instant::now(),
            _ => unreachable!(),
        }
        assert!(
            service.restore(&owner.id, owner.revision, &proof).is_err(),
            "accepted {change}"
        );
        assert_eq!(service.list().unwrap(), vec![owner]);
        assert!(
            f.store
                .read(|tx| tx.attributions(f.subject.candidate.project))
                .unwrap()[0]
                .held
        );
    }
}
