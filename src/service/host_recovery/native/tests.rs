//! Deterministic protocol/admission detectors. The private fixture constructor
//! stands in for authenticated transport only; real broker peer/ledger tests
//! live in test_host_restoration.py. No public JSON can construct these types.
use super::*;
use crate::service::host_recovery::blocks_admission;
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
// Share authentic physical-gate setup while letting the producer own its
// first attribution. Native proof fixtures explicitly request a prior hold.
struct HostSeed {
    fixture: storyhook_test_support::ServiceFixture,
    store: SqliteStore,
    candidate: VerificationCandidate,
    record: AttributionRecord,
    gate: GateExecution,
    raw: Vec<u8>,
    root: std::path::PathBuf,
}
impl HostSeed {
    fn new(settled: bool, attributed: bool) -> Self {
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
        attempt.finished_at = settled.then(|| AT.into());
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
                if attributed {
                    tx.insert_attribution(&record)?;
                }
                Ok(())
            })
            .unwrap();
        Self {
            fixture,
            store,
            candidate,
            record,
            gate,
            raw,
            root,
        }
    }
    fn ctx(&self) -> Ctx<'_, SqliteStore> {
        Ctx::new(
            &self.store,
            self.candidate.project,
            self.fixture.cwd(),
            Environment::at(self.fixture.cwd()).with_subprocess_patience(),
        )
        .no_hooks(true)
    }
}
impl Fixture {
    fn new() -> Self {
        Self::with_attempt(true)
    }
    fn with_attempt(settled: bool) -> Self {
        let HostSeed {
            fixture,
            store,
            candidate,
            record,
            gate,
            raw,
            root,
        } = HostSeed::new(settled, true);
        let mut subject = store
            .read(|tx| {
                capture_with_phase(
                    tx,
                    &candidate,
                    &record.id,
                    &gate.id,
                    "pressure",
                    if settled {
                        CapturePhase::NativeAuthority
                    } else {
                        CapturePhase::PendingObservation
                    },
                )
            })
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
            .read(|tx| super::super::owner::blocks_admission(tx))
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
            .read(|tx| super::super::owner::blocks_admission(tx))
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

#[test]
fn native_host_pause_fences_other_project_queue_and_cached_direct_admission() {
    use crate::service::project_recovery::{ProjectRecoveryService, RepairAdmission, RepairInput};
    let f = Fixture::new();
    let fixture_project = f.fixture.add_project("another-project", "OTHER");
    let checkout = f
        .fixture
        .github_checkout_for(fixture_project, "https://github.com/acme/another");
    let other = ProjectId::new(fixture_project.get());
    let other_ctx = Ctx::new(
        &f.store,
        other,
        &checkout,
        Environment::at(f.fixture.cwd()).with_subprocess_patience(),
    )
    .no_hooks(true);
    let stories = StoryService::new(&other_ctx);
    let story = stories
        .create(&NewStoryInput {
            title: "independent cached gate".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    PrLinkService::new(&other_ctx)
        .link(&story, "https://github.com/acme/another/pull/2", true)
        .unwrap();
    stories
        .set_state(&story, "verifying", None, None, None)
        .unwrap();
    let cached = f
        .store
        .read(|tx| crate::service::verification::ordered_candidates_for(tx, other))
        .unwrap()
        .pop()
        .unwrap();
    let input = RepairInput {
        base: "a".repeat(40),
        head: "b".repeat(40),
        head_tree: "c".repeat(40),
        tree: "d".repeat(40),
    };
    assert_eq!(cached.project, other);
    assert!(cached.verifying_generation.is_some());
    assert_eq!(cached.checkout, checkout);
    assert_eq!(
        cached.pull_request.as_ref().unwrap().url,
        "https://github.com/acme/another/pull/2"
    );
    // Establish that the exact cached submission reaches the real repair door
    // before the native fault; missing PR metadata must not satisfy this test.
    assert!(matches!(
        ProjectRecoveryService::new(&other_ctx)
            .admit_repair(&cached, "before-native-host-fault", &input)
            .unwrap(),
        RepairAdmission::Proceed { recovery_id: None }
    ));
    let original = f.ctx();
    let service = HostRecoveryService::new(&original);
    let owner = service
        .enroll(&HostFaultEvidence {
            live: f.proof(false),
        })
        .unwrap();
    assert!(
        f.store
            .read(|tx| crate::service::verification::ordered_candidates_for(tx, other))
            .unwrap()
            .is_empty()
    );
    let refused = ProjectRecoveryService::new(&other_ctx)
        .admit_repair(&cached, "cached-before-host-fault", &input)
        .unwrap_err();
    assert!(
        refused.to_string().contains("active native host fault"),
        "{refused}"
    );
    service
        .restore(
            &owner.id,
            owner.revision,
            &HostRestorationEvidence {
                live: f.proof(true),
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        f.store
            .read(|tx| crate::service::verification::ordered_candidates_for(tx, other))
            .unwrap(),
        vec![cached.clone()]
    );
    assert!(matches!(
        ProjectRecoveryService::new(&other_ctx)
            .admit_repair(&cached, "after-native-restoration", &input)
            .unwrap(),
        RepairAdmission::Proceed { recovery_id: None }
    ));
    let retained = &f.subject.candidate;
    let number = f.subject.attribution.submission.story_number().unwrap();
    assert!(
        f.store
            .read(
                |tx| crate::service::project_recovery::requires_certification(
                    tx,
                    retained.project,
                    number
                )
            )
            .unwrap()
    );
    f.store
        .write(|tx| tx.put_verification_enabled(retained.project, false))
        .unwrap();
    assert!(
        f.store
            .read(|tx| crate::service::verification::ordered_candidates_for(tx, retained.project))
            .unwrap()
            .is_empty(),
        "host readmission entered verification-skipped mode"
    );
}

#[test]
fn pending_native_host_custody_survives_restart_without_granting_owner_authority() {
    let f = HostSeed::new(false, false);
    let ctx = f.ctx();
    assert!(
        f.store
            .read(|tx| tx.attributions(f.candidate.project))
            .unwrap()
            .is_empty(),
        "producer fixture pre-created the attribution it must own"
    );
    assert!(retain_failed_pressure(&ctx, &f.candidate, "host-attempt").unwrap());
    let original_attribution = f
        .store
        .read(|tx| tx.attributions(f.candidate.project))
        .unwrap();
    assert_eq!(original_attribution.len(), 1);
    assert_eq!(original_attribution[0].attempt, "host-attempt");
    assert_eq!(
        original_attribution[0].components[0].id,
        "native-host-pressure"
    );
    let before = f
        .store
        .read(|tx| tx.host_recovery_pending(f.candidate.project))
        .unwrap();
    assert_eq!(before.len(), 1);
    assert!(retain_failed_pressure(&ctx, &f.candidate, "host-attempt").unwrap());
    assert_eq!(
        f.store
            .read(|tx| tx.host_recovery_pending(f.candidate.project))
            .unwrap(),
        before
    );
    assert_eq!(
        f.store
            .read(|tx| tx.attributions(f.candidate.project))
            .unwrap(),
        original_attribution,
        "idempotent retry replaced or duplicated the producer's attribution"
    );
    assert!(
        f.store.read(|tx| tx.host_recoveries()).unwrap().is_empty(),
        "an observation minted global host authority"
    );
    let restarted = SqliteStore::open(f.store.path()).unwrap();
    let waiting = restarted
        .read(|tx| pending_subjects(tx, f.candidate.project))
        .unwrap();
    assert_eq!(waiting.len(), 1);
    let pending = &waiting[0];
    assert_eq!(pending.candidate, f.candidate);
    assert!(
        restarted
            .read(|tx| capture(
                tx,
                &pending.candidate,
                &pending.attribution,
                &pending.execution,
                &pending.component
            ))
            .is_err(),
        "a live gate admission supplied completed native authority"
    );
    restarted
        .write(|tx| {
            let mut attempt = tx.gate_attempts(f.candidate.project)?.pop().unwrap();
            let previous = attempt.revision;
            attempt.revision += 1;
            attempt.finished_at = Some(AT.into());
            assert!(tx.update_gate_attempt(&attempt, previous)?);
            Ok(())
        })
        .unwrap();
    let retained = restarted
        .read(|tx| {
            capture(
                tx,
                &pending.candidate,
                &pending.attribution,
                &pending.execution,
                &pending.component,
            )
        })
        .unwrap();
    assert_eq!(retained.candidate, f.candidate);
    assert_eq!(retained.admitted_at, AT);
    fs::write(
        &retained.archive.path,
        "changed after pending custody was retained",
    )
    .unwrap();
    assert!(
        restarted
            .read(|tx| capture(
                tx,
                &pending.candidate,
                &pending.attribution,
                &pending.execution,
                &pending.component
            ))
            .is_err()
    );
    assert_eq!(
        restarted
            .read(|tx| tx.host_recovery_pending(f.candidate.project))
            .unwrap(),
        before,
        "raw failure erased pending original identity"
    );
}

#[test]
fn native_host_proof_refuses_late_cleanup_custody_replacement() {
    let f = Fixture::new();
    let proof = HostFaultEvidence {
        live: f.proof(false),
    };
    let candidate = &f.subject.candidate;
    assert_eq!(candidate.cleanup_lease, None);
    let ctx = f.ctx();
    let service = HostRecoveryService::new(&ctx);
    let owner = service.enroll(&proof).unwrap();
    // Enrollment appends a comment. This is therefore a late, conflicting
    // lease, not the adjacent lease used to define the original generation.
    f.fixture.append_cleanup_lease(&candidate.story_id, serde_json::from_value(serde_json::json!({
        "version":1,"project_slug":candidate.project_slug,"story_id":candidate.story_id,
        "repository_path":candidate.checkout,"worktree_path":candidate.checkout.join("replacement"),
        "branch":"replacement-work","tmux":{"socket_path":candidate.checkout.join("fixture-socket"),"revivify":null}
    })).unwrap());
    assert!(service.enroll(&proof).is_err());
    assert!(
        service
            .restore(
                &owner.id,
                owner.revision,
                &HostRestorationEvidence {
                    live: f.proof(true)
                }
            )
            .is_err()
    );
    assert_eq!(service.list().unwrap(), vec![owner]);
    assert!(f.store.read(|tx| blocks_admission(tx)).unwrap());
}

#[test]
fn native_host_status_isolates_invalid_owner_and_preserves_original_elapsed_origin() {
    let f = Fixture::new();
    let ctx = f.ctx();
    let owner = HostRecoveryService::new(&ctx)
        .enroll(&HostFaultEvidence {
            live: f.proof(false),
        })
        .unwrap();
    f.store
        .write(|tx| {
            assert!(tx.insert_host_recovery(&crate::store::HostRecovery {
                id: uuid::Uuid::new_v4().to_string(),
                fault_key: "f".repeat(64),
                revision: 0,
                active: true,
                state: json!({"version":1,"started_at":"not-a-timestamp"}),
            })?);
            Ok(())
        })
        .unwrap();
    let statuses = f
        .store
        .read(|tx| crate::service::host_recovery::status_snapshot(tx, f.subject.candidate.project))
        .unwrap();
    assert_eq!(statuses.len(), 2);
    let valid = statuses.iter().find(|s| s.id == owner.id).unwrap();
    assert_eq!(valid.started_at.as_deref(), Some(AT));
    assert!(valid.elapsed_milliseconds.is_some());
    assert!(valid.pauses_admission);
    let invalid = statuses.iter().find(|s| s.phase == "invalid").unwrap();
    assert_eq!(invalid.started_at, None);
    assert_eq!(invalid.elapsed_milliseconds, None);
    assert!(invalid.pauses_admission);
    assert!(
        f.store.read(|tx| blocks_admission(tx)).is_err(),
        "status isolation granted admission through invalid custody"
    );
}

#[test]
fn restored_host_status_keeps_history_without_claiming_completed_story_needs_gate() {
    let f = Fixture::new();
    let ctx = f.ctx();
    let service = HostRecoveryService::new(&ctx);
    let owner = service
        .enroll(&HostFaultEvidence {
            live: f.proof(false),
        })
        .unwrap();
    service
        .restore(
            &owner.id,
            owner.revision,
            &HostRestorationEvidence {
                live: f.proof(true),
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        f.store
            .read(|tx| crate::service::host_recovery::status_snapshot(
                tx,
                f.subject.candidate.project
            ))
            .unwrap()
            .len(),
        1
    );
    StoryService::new(&ctx)
        .set_state(&f.subject.candidate.story_id, "done", None, None, None)
        .unwrap();
    assert!(
        f.store
            .read(|tx| crate::service::host_recovery::status_snapshot(
                tx,
                f.subject.candidate.project
            ))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        f.store.read(|tx| tx.host_recoveries()).unwrap().len(),
        1,
        "status projection erased recovery history"
    );
}

#[test]
fn host_recovery_status_exposes_exact_fault_and_only_selected_project_admissions() {
    let f = Fixture::new();
    let other = ProjectId::new(f.fixture.add_project("other-status-project", "OTHER").get());
    let other_ctx = Ctx::new(
        &f.store,
        other,
        f.fixture.cwd(),
        Environment::at(f.fixture.cwd()).with_subprocess_patience(),
    )
    .no_hooks(true);
    let other_story = StoryService::new(&other_ctx)
        .create(&NewStoryInput {
            title: "private other-project subject".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    StoryService::new(&other_ctx)
        .set_state(&other_story, "verifying", None, None, None)
        .unwrap();
    let other_candidate = f
        .store
        .read(|tx| crate::service::verification::ordered_candidates_for(tx, other))
        .unwrap()
        .pop()
        .unwrap();
    let owner = HostRecoveryService::new(&f.ctx())
        .enroll(&HostFaultEvidence {
            live: f.proof(false),
        })
        .unwrap();
    let original = f.subject.attribution.submission.clone();
    // Add retained observations for a second project to the same native episode.
    // This is status-only data; it does not mint a live fault/restoration proof.
    let mut other_subject = f.subject.clone();
    other_subject.candidate = other_candidate.clone();
    other_subject.attribution.id = "other-status-attribution".into();
    other_subject.attribution.submission = GateSubmission {
        project: other,
        story_id: other_story.clone(),
        generation: other_candidate.verifying_generation,
        submitted_at: other_candidate.verifying_since.clone(),
    };
    other_subject.execution.id = "other-status-execution".into();
    let mut duplicate_subject = f.subject.clone();
    duplicate_subject.attribution.id = "second-observation-same-generation".into();
    duplicate_subject.execution.id = "second-observation-execution".into();
    f.store
        .write(|tx| {
            let mut record = tx
                .host_recoveries()?
                .into_iter()
                .find(|r| r.id == owner.id)
                .unwrap();
            let expected = record.revision;
            let template = record.state["members"][0].clone();
            for subject in [&other_subject, &duplicate_subject] {
                let mut member = template.clone();
                member["subject"] = serde_json::to_value(subject).unwrap();
                record.state["members"].as_array_mut().unwrap().push(member);
            }
            record.revision += 1;
            assert!(tx.update_host_recovery(&record, expected)?);
            tx.insert_gate_attempt(&GateAttempt::new(
                "status-retry".into(),
                original.clone(),
                AT,
            ))?;
            tx.insert_gate_attempt(&GateAttempt::new(
                "other-status-attempt".into(),
                other_subject.attribution.submission.clone(),
                AT,
            ))?;
            assert!(tx.insert_host_recovery(&crate::store::HostRecovery {
                id: uuid::Uuid::new_v4().to_string(),
                fault_key: "f".repeat(64),
                revision: 0,
                active: true,
                state: json!({"version":1,"started_at":"invalid"}),
            })?);
            Ok(())
        })
        .unwrap();
    let reopened = SqliteStore::open(f.store.path()).unwrap();
    let before = reopened.read(|tx| tx.host_recoveries()).unwrap();
    let statuses = reopened
        .read(|tx| crate::service::host_recovery::status_snapshot(tx, original.project))
        .unwrap();
    let status = statuses.iter().find(|s| s.id == owner.id).unwrap();
    let fault = status.fault.as_ref().unwrap();
    assert_eq!(fault.kind, "native-host-pressure");
    assert_eq!(fault.sequence, f.subject.request.fault.sequence);
    assert_eq!(
        fault.key,
        before.iter().find(|r| r.id == owner.id).unwrap().fault_key
    );
    assert_eq!(status.submissions, [original.story_id.clone()]);
    assert_eq!(status.retained_submissions.len(), 1);
    assert_eq!(status.retained_submissions[0].submission, original);
    assert_eq!(status.retained_submissions[0].admission_count, Some(2));
    assert!(!serde_json::to_string(status).unwrap().contains("OTHER-"));
    let foreign = reopened
        .read(|tx| crate::service::host_recovery::status_snapshot(tx, other))
        .unwrap();
    let selected = foreign.iter().find(|s| s.id == owner.id).unwrap();
    assert_eq!(selected.retained_submissions.len(), 1);
    assert_eq!(selected.retained_submissions[0].submission.project, other);
    assert_eq!(selected.retained_submissions[0].admission_count, Some(1));
    let invalid = statuses.iter().find(|s| s.phase == "invalid").unwrap();
    assert!(invalid.fault.is_none());
    assert!(invalid.retained_submissions.is_empty());
    assert_eq!(
        reopened.read(|tx| tx.host_recoveries()).unwrap(),
        before,
        "status mutated retained owners"
    );
}

#[test]
fn host_recovery_status_refuses_mixed_candidate_and_attribution_binding() {
    let f = Fixture::new();
    let owner = HostRecoveryService::new(&f.ctx())
        .enroll(&HostFaultEvidence {
            live: f.proof(false),
        })
        .unwrap();
    let original = f
        .store
        .read(|tx| tx.host_recoveries())
        .unwrap()
        .into_iter()
        .find(|r| r.id == owner.id)
        .unwrap();
    for field in ["project", "story", "generation"] {
        let mut subject = f.subject.clone();
        match field {
            "project" => {
                subject.attribution.submission.project =
                    ProjectId::new(f.subject.candidate.project.get() + 100)
            }
            "story" => subject.attribution.submission.story_id = "PRIVATE-999".into(),
            "generation" => {
                subject.attribution.submission.generation = Some(GlobalSeq::new(
                    f.subject.candidate.verifying_generation.unwrap().get() + 1,
                ))
            }
            _ => unreachable!(),
        }
        f.store
            .write(|tx| {
                let mut record = tx
                    .host_recoveries()?
                    .into_iter()
                    .find(|r| r.id == owner.id)
                    .unwrap();
                let expected = record.revision;
                record.state = original.state.clone();
                record.state["members"][0]["subject"] = serde_json::to_value(&subject).unwrap();
                record.revision += 1;
                assert!(tx.update_host_recovery(&record, expected)?);
                Ok(())
            })
            .unwrap();
        let before = f.store.read(|tx| tx.host_recoveries()).unwrap();
        let statuses = f
            .store
            .read(|tx| {
                crate::service::host_recovery::status_snapshot(tx, f.subject.candidate.project)
            })
            .unwrap();
        let status = statuses.iter().find(|s| s.id == owner.id).unwrap();
        assert_eq!(status.phase, "invalid", "{field}");
        assert!(
            status
                .next_action
                .contains("subject project, story or generation"),
            "{field}: {}",
            status.next_action
        );
        assert!(status.fault.is_none());
        assert!(status.retained_submissions.is_empty());
        assert!(status.submissions.is_empty());
        assert!(
            !serde_json::to_string(status)
                .unwrap()
                .contains("PRIVATE-999")
        );
        assert_eq!(f.store.read(|tx| tx.host_recoveries()).unwrap(), before);
    }
}

#[test]
fn pending_native_host_refuses_existing_attribution_without_adopting_or_replacing_hold() {
    let f = HostSeed::new(false, true);
    let ctx = f.ctx();
    let before = f
        .store
        .read(|tx| tx.attributions(f.candidate.project))
        .unwrap();
    assert_eq!(before, vec![f.record.clone()]);
    let events = f
        .store
        .read(|tx| {
            tx.events_for(
                f.candidate.project,
                f.record.submission.story_number().unwrap(),
            )
        })
        .unwrap();
    let error = retain_failed_pressure(&ctx, &f.candidate, "host-attempt").unwrap_err();
    assert!(
        error
            .to_string()
            .contains("existing attribution lacks exact pending host custody"),
        "{error}"
    );
    assert_eq!(
        f.store
            .read(|tx| tx.attributions(f.candidate.project))
            .unwrap(),
        before
    );
    assert_eq!(
        f.store
            .read(|tx| tx.events_for(
                f.candidate.project,
                f.record.submission.story_number().unwrap()
            ))
            .unwrap(),
        events
    );
    assert!(
        f.store
            .read(|tx| tx.host_recovery_pending(f.candidate.project))
            .unwrap()
            .is_empty()
    );
    assert!(f.store.read(|tx| tx.host_recoveries()).unwrap().is_empty());
}
