//! Durable context transfers preserve story eligibility and require receiver evidence.
use serde_json::{Value, json};
use storyhook::error::AppError;
use storyhook::service::continuation::{ContinuationRuntime, ContinuationService};
use storyhook::service::{NewStoryInput, StoryService};
use storyhook_test_support::ServiceFixture;

struct Runtime;
impl ContinuationRuntime for Runtime {
    fn call(&self, operation: &str, input: &Value) -> Result<Value, AppError> {
        assert_eq!(operation, "capture");
        Ok(json!({"ok":true,"capture":{
            "lease":{"version":1,"project_slug":"fixture","story_id":input["handoff"]["story_id"],"repository_path":"/tmp/repo","worktree_path":"/tmp/repo/lane","branch":"work","tmux":{"socket_path":"/tmp/tmux"}},
            "head":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","fingerprint":"dirty-1","provider":"codex","session_id":"session-1","turn_id":input["origin"]["turn_id"],"mode":"default","socket":"/tmp/tmux","pane":"%1","pid":123,"started":"start","model":"gpt","effort":"high","speed":"standard","autonomy":true
        }}))
    }
}
fn setup() -> (ServiceFixture, String) {
    let mut f = ServiceFixture::new();
    f.set_clock(storyhook::service::Clock::System);
    let id = StoryService::new(&f.ctx())
        .create(&NewStoryInput {
            title: "Continue safely".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    StoryService::new(&f.ctx())
        .set_state(&id, "in-progress", None, None, None)
        .unwrap();
    (f, id)
}
fn input(id: &str) -> Value {
    json!({"provider":"codex","origin":{"session_id":"session-1","turn_id":"turn-1"},"handoff":{"type":"storyhook.session-handoff","version":1,"story_id":id,"kind":"context","evidence":{"context":"context is exhausted","outstanding_work":"finish regression tests"}}})
}
#[test]
fn context_request_is_durable_idempotent_and_does_not_block_story() {
    let (f, id) = setup();
    let ctx = f.ctx();
    let service = ContinuationService::new(&ctx, &Runtime);
    let first = service.request(&id, input(&id)).unwrap();
    let again = service.request(&id, input(&id)).unwrap();
    assert_eq!(first.id, again.id);
    assert_eq!(
        service.status(&id).unwrap()["requests"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let story = StoryService::new(&ctx)
        .set_state(&id, "in-progress", None, None, None)
        .unwrap();
    assert!(story.awaiting.is_none());
    assert_eq!(
        story
            .comments
            .iter()
            .filter(|c| c.text.contains("CONTEXT HANDOFF"))
            .count(),
        1
    );
}
#[test]
fn unresolved_handoff_refuses_verification_and_conflicting_retries() {
    let (f, id) = setup();
    let ctx = f.ctx();
    let service = ContinuationService::new(&ctx, &Runtime);
    service.request(&id, input(&id)).unwrap();
    let mut changed = input(&id);
    changed["handoff"]["evidence"]["outstanding_work"] = json!("different work");
    assert!(service.request(&id, changed).is_err());
    assert!(
        StoryService::new(&ctx)
            .set_state(&id, "verifying", None, None, None)
            .unwrap_err()
            .to_string()
            .contains("continuation")
    );
}

fn requests(f: &ServiceFixture) -> Vec<storyhook::store::Continuation> {
    use storyhook::store::{ReadOps, Store};
    f.store().read(|tx| tx.continuations(f.project())).unwrap()
}
fn sequence(service: &ContinuationService<'_, storyhook::store::SqliteStore>, id: &str) -> i64 {
    service.status(id).unwrap()["snapshot_seq"]
        .as_i64()
        .unwrap()
}
const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
struct Observer {
    phase: &'static str,
    calls: std::cell::RefCell<Vec<String>>,
}
impl Observer {
    fn new(phase: &'static str) -> Self {
        Self {
            phase,
            calls: Default::default(),
        }
    }
}
impl ContinuationRuntime for Observer {
    fn call(&self, operation: &str, input: &Value) -> Result<Value, AppError> {
        self.calls.borrow_mut().push(operation.into());
        if operation == "capture" {
            return Runtime.call(operation, input);
        }
        if operation == "resume" {
            let mut capture = input["capture"].clone();
            capture["session_id"] = json!("resumed-session");
            capture["pid"] = json!(456);
            return Ok(json!({"ok":true,"phase":"submitted","capture":capture}));
        }
        assert_eq!(operation, "observe");
        Ok(
            json!({"ok":true,"phase":self.phase,"capture":input["capture"],"detail":"isolated observation"}),
        )
    }
}
#[test]
fn native_continuation_never_injects_live_terminal_input() {
    use storyhook::store::ContinuationStatus;
    for phase in ["idle", "busy"] {
        let (f, id) = setup();
        let ctx = f.ctx();
        let runtime = Observer::new(phase);
        let service = ContinuationService::new(&ctx, &runtime);
        service.request(&id, input(&id)).unwrap();
        storyhook::daemon::continuation::process_one(f.store(), f.env(), &runtime).unwrap();
        assert_eq!(runtime.calls.borrow().as_slice(), ["capture", "observe"]);
        assert_eq!(requests(&f)[0].status, ContinuationStatus::AwaitingAck);
    }
}
#[test]
fn absent_provider_resumes_once_and_requires_receiving_identity() {
    use storyhook::store::ContinuationStatus;
    let (f, id) = setup();
    let ctx = f.ctx();
    let runtime = Observer::new("absent");
    let service = ContinuationService::new(&ctx, &runtime);
    let request = service.request(&id, input(&id)).unwrap();
    storyhook::daemon::continuation::process_one(f.store(), f.env(), &runtime).unwrap();
    assert_eq!(
        runtime.calls.borrow().as_slice(),
        ["capture", "observe", "resume"]
    );
    assert_eq!(requests(&f)[0].status, ContinuationStatus::AwaitingAck);
    let receiver = Observer::new("busy");
    let receiving = ContinuationService::new(&ctx, &receiver);
    assert!(
        receiving
            .ack(
                &id,
                &request.id,
                sequence(&receiving, &id),
                HEAD,
                "codex",
                "session-1"
            )
            .is_err()
    );
    receiving
        .ack(
            &id,
            &request.id,
            sequence(&receiving, &id),
            HEAD,
            "codex",
            "resumed-session",
        )
        .unwrap();
    assert_eq!(requests(&f)[0].status, ContinuationStatus::Acknowledged);
}
#[test]
fn uncertain_observation_and_restart_effect_gap_do_not_replay() {
    use storyhook::store::{ContinuationStatus, ReadOps, Store, WriteOps};
    let (f, id) = setup();
    let ctx = f.ctx();
    let runtime = Observer::new("uncertain");
    let service = ContinuationService::new(&ctx, &runtime);
    service.request(&id, input(&id)).unwrap();
    storyhook::daemon::continuation::process_one(f.store(), f.env(), &runtime).unwrap();
    assert_eq!(requests(&f)[0].status, ContinuationStatus::NeedsAttention);
    assert_eq!(runtime.calls.borrow().as_slice(), ["capture", "observe"]);
    f.store()
        .write(|tx| {
            let mut r = tx.continuations(f.project())?.remove(0);
            let old = r.revision;
            r.revision += 1;
            r.status = ContinuationStatus::Attempting;
            r.phase = storyhook::store::ContinuationPhase::Resume;
            tx.update_continuation(&r, old)?;
            Ok(())
        })
        .unwrap();
    storyhook::daemon::continuation::recover(f.store(), f.env()).unwrap();
    assert_eq!(requests(&f)[0].status, ContinuationStatus::NeedsAttention);
    assert!(requests(&f)[0].detail.contains("no automatic replay"));
    storyhook::daemon::continuation::process_one(f.store(), f.env(), &runtime).unwrap();
    assert_eq!(runtime.calls.borrow().as_slice(), ["capture", "observe"]);
}
#[test]
fn native_compaction_receipt_is_informational_and_session_bound() {
    let (f, id) = setup();
    let ctx = f.ctx();
    let runtime = Observer::new("compacted");
    let service = ContinuationService::new(&ctx, &runtime);
    let r = service.request(&id, input(&id)).unwrap();
    let receipt = json!({"event":"post-compact","provider":"codex","session_id":"session-1","origin":{"session_id":"session-1"}});
    let recorded = service.receipt(&id, &r.id, &receipt).unwrap();
    assert_eq!(
        recorded.phase,
        storyhook::store::ContinuationPhase::NativeContinuation
    );
    let duplicate = service.receipt(&id, &r.id, &receipt).unwrap();
    assert_eq!(duplicate.revision, recorded.revision);
    let wrong = json!({"event":"post-compact","provider":"claude","session_id":"session-1","origin":{"session_id":"session-1"}});
    assert!(service.receipt(&id, &r.id, &wrong).is_err());
}
#[test]
fn acknowledgement_requires_current_sequence_and_head() {
    let (f, id) = setup();
    let ctx = f.ctx();
    let runtime = Observer::new("busy");
    let service = ContinuationService::new(&ctx, &runtime);
    let r = service.request(&id, input(&id)).unwrap();
    let before = sequence(&service, &id);
    StoryService::new(&ctx)
        .comment(&id, "A correction arrived while review was running")
        .unwrap();
    assert!(
        service
            .ack(&id, &r.id, before, HEAD, "codex", "session-1")
            .unwrap_err()
            .to_string()
            .contains("changed after review")
    );
    let now = sequence(&service, &id);
    assert!(
        service
            .ack(
                &id,
                &r.id,
                now,
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "codex",
                "session-1"
            )
            .is_err()
    );
    service
        .ack(&id, &r.id, now, HEAD, "codex", "session-1")
        .unwrap();
    StoryService::new(&ctx)
        .comment(&id, "A second correction arrived before submission")
        .unwrap();
    assert!(
        StoryService::new(&ctx)
            .set_state(&id, "verifying", None, None, None)
            .unwrap_err()
            .to_string()
            .contains("review is stale")
    );
}
#[test]
fn context_request_cannot_clear_real_holds_and_status_survives_reopen() {
    use storyhook::store::{ReadOps, SqliteStore, Store};
    let (f, id) = setup();
    let ctx = f.ctx();
    let service = ContinuationService::new(&ctx, &Runtime);
    let r = service.request(&id, input(&id)).unwrap();
    let reopened = SqliteStore::open(f.store().path()).unwrap();
    assert_eq!(
        reopened.read(|tx| tx.continuations(f.project())).unwrap()[0].id,
        r.id
    );
    StoryService::new(&ctx)
        .set_awaiting(&id, "operator trust review")
        .unwrap();
    let observer = Observer::new("absent");
    storyhook::daemon::continuation::process_one(f.store(), f.env(), &observer).unwrap();
    assert!(observer.calls.borrow().is_empty());
    let row = f
        .store()
        .read(|tx| tx.story(f.project(), r.story_no))
        .unwrap()
        .unwrap();
    assert_eq!(row.awaiting.as_deref(), Some("operator trust review"));
}

#[test]
fn cli_requires_explicit_receiving_review_evidence() {
    use storyhook::cli::{ContinuationAction, Invocation, parse_invocation};
    let parse =
        |args: &[&str]| parse_invocation(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    assert!(matches!(
        parse(&["continuation", "capabilities"]).unwrap(),
        Invocation::Continuation {
            action: ContinuationAction::Capabilities,
            ..
        }
    ));
    assert!(parse(&["continuation", "request", "SH-1", "--stdin"]).is_ok());
    assert!(parse(&["continuation", "ack", "SH-1", "request"]).is_err());
    assert!(
        parse(&[
            "continuation",
            "ack",
            "SH-1",
            "request",
            "--reviewed-seq",
            "3",
            "--head",
            HEAD,
            "--provider",
            "codex",
            "--session-id",
            "sid"
        ])
        .is_ok()
    );
}
#[test]
fn session_start_retains_exact_native_transcript_path_additively() {
    let f = ServiceFixture::new();
    let ctx=f.ctx().with_stdin(Some(r#"{"session_id":"session-a","transcript_path":"/tmp/native/exact-session.jsonl","storyhook_plugin_root":"/tmp/plugin"}"#.into()));
    storyhook::service::SessionService::new(&ctx).publish_sentinel();
    let sentinel: Value = serde_json::from_slice(
        &std::fs::read(f.cwd().join(".claude/dispatch-sentinel.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(sentinel["protocol_version"], 2);
    assert_eq!(
        sentinel["transcript_path"],
        "/tmp/native/exact-session.jsonl"
    );
    assert_eq!(sentinel["session_id"], "session-a");
}
#[test]
fn duplicate_json_keys_are_refused_at_every_evidence_depth() {
    use storyhook::service::continuation::parse_document;
    assert!(parse_document(r#"{"a":1,"a":2}"#).is_err());
    assert!(parse_document(r#"{"evidence":{"context":"first","context":"second"}}"#).is_err());
    assert!(parse_document(r#"{"a":[{"b":true,"b":false}]}"#).is_err());
    assert_eq!(
        parse_document(r#"{"valid":[null,true,1,1.5,"text"]}"#).unwrap(),
        json!({"valid":[null,true,1,1.5,"text"]})
    );
}
#[test]
fn concurrent_identical_requests_share_one_durable_generation() {
    let (f, id) = setup();
    let ids = std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            let ctx = f.ctx();
            ContinuationService::new(&ctx, &Runtime)
                .request(&id, input(&id))
                .unwrap()
                .id
        });
        let second = scope.spawn(|| {
            let ctx = f.ctx();
            ContinuationService::new(&ctx, &Runtime)
                .request(&id, input(&id))
                .unwrap()
                .id
        });
        (first.join().unwrap(), second.join().unwrap())
    });
    assert_eq!(ids.0, ids.1);
    assert_eq!(requests(&f).len(), 1);
}
#[test]
fn three_unchanged_generations_stop_automatic_continuation() {
    use storyhook::store::ContinuationStatus;
    let (f, id) = setup();
    let ctx = f.ctx();
    let runtime = Observer::new("busy");
    let service = ContinuationService::new(&ctx, &runtime);
    for turn in 1..=3 {
        let mut request = input(&id);
        request["origin"]["turn_id"] = json!(format!("turn-{turn}"));
        request["handoff"]["evidence"]["context"] = json!(format!("reworded handoff {turn}"));
        let record = service.request(&id, request).unwrap();
        if turn < 3 {
            assert_eq!(record.status, ContinuationStatus::AwaitingAck);
            service
                .ack(
                    &id,
                    &record.id,
                    sequence(&service, &id),
                    HEAD,
                    "codex",
                    "session-1",
                )
                .unwrap();
        } else {
            assert_eq!(record.status, ContinuationStatus::NeedsAttention);
            assert!(record.detail.contains("three consecutive"));
        }
    }
}
#[test]
fn late_acknowledgement_can_resolve_timeout_without_replaying_input() {
    use storyhook::store::{ContinuationStatus, ReadOps, Store, WriteOps};
    let (f, id) = setup();
    let ctx = f.ctx();
    let runtime = Observer::new("busy");
    let service = ContinuationService::new(&ctx, &runtime);
    let request = service.request(&id, input(&id)).unwrap();
    f.store()
        .write(|tx| {
            let mut r = tx.continuations(f.project())?.remove(0);
            let old = r.revision;
            r.revision += 1;
            r.updated_at = "2020-01-01T00:00:00Z".into();
            tx.update_continuation(&r, old)?;
            Ok(())
        })
        .unwrap();
    storyhook::daemon::continuation::process_one(f.store(), f.env(), &runtime).unwrap();
    assert_eq!(requests(&f)[0].status, ContinuationStatus::NeedsAttention);
    assert!(service.retry(&id, &request.id).is_err());
    service
        .ack(
            &id,
            &request.id,
            sequence(&service, &id),
            HEAD,
            "codex",
            "session-1",
        )
        .unwrap();
    assert_eq!(requests(&f)[0].status, ContinuationStatus::Acknowledged);
}
#[test]
fn compare_and_swap_refuses_stale_delivery_writes() {
    use storyhook::store::{Store, WriteOps};
    let (f, id) = setup();
    let ctx = f.ctx();
    let service = ContinuationService::new(&ctx, &Runtime);
    let mut request = service.request(&id, input(&id)).unwrap();
    let old = request.revision;
    request.revision += 1;
    assert!(
        f.store()
            .write(|tx| tx.update_continuation(&request, old))
            .unwrap()
    );
    assert!(
        !f.store()
            .write(|tx| tx.update_continuation(&request, old))
            .unwrap()
    );
}
#[test]
fn continuation_migration_preserves_legacy_operational_receipts() {
    use storyhook::store::{MIGRATIONS, SqliteStore, Store};
    let dir = storyhook_test_support::scratch_dir();
    let path = dir.path().join("before.sqlite");
    let store = SqliteStore::open(&path).unwrap();
    store.migrate_with(&MIGRATIONS[..39]).unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute(
            "CREATE TABLE independent_legacy_receipt (value TEXT NOT NULL)",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO independent_legacy_receipt VALUES ('retained')",
            [],
        )
        .unwrap();
    drop(connection);
    store.migrate().unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT value FROM independent_legacy_receipt", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
        "retained"
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM continuations", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
#[test]
fn engine_restart_preserves_continuation_lane_before_missing_pane_classification() {
    use storyhook::service::engine::{EngineService, StartRequest};
    use storyhook::store::{EngineAgent, EngineLaneState, EngineScope, ReadOps, Store, WriteOps};
    use storyhook_test_support::FakeDispatcher;
    let (f, id) = setup();
    let ctx = f.ctx();
    let dispatcher = FakeDispatcher::new([storyhook_test_support::DispatcherStep::WindowAlive {
        window: "=fixture:=absent-pane".into(),
        alive: false,
    }]);
    let engine = EngineService::new(&ctx, &dispatcher);
    let run = engine
        .start(StartRequest {
            scope: EngineScope::Project,
            lanes: 1,
            agent: EngineAgent::Codex,
            model: None,
            effort: None,
            speed: None,
        })
        .unwrap();
    f.store()
        .write(|tx| {
            let mut lane = tx.engine_lanes(&run.id)?.remove(0);
            lane.state = EngineLaneState::Working;
            lane.story_id = Some(id.clone());
            lane.window_name = Some("absent-pane".into());
            lane.worktree_path = Some("/tmp/repo/lane".into());
            tx.put_engine_lane(&lane)
        })
        .unwrap();
    ContinuationService::new(&ctx, &Runtime)
        .request(&id, input(&id))
        .unwrap();
    assert_eq!(requests(&f)[0].capture["engine_lane"]["run_id"], run.id);
    assert_eq!(requests(&f)[0].capture["engine_lane"]["lane_index"], 0);
    engine.reconcile_after_restart(&run.id).unwrap();
    let lane = f
        .store()
        .read(|tx| tx.engine_lanes(&run.id))
        .unwrap()
        .remove(0);
    assert_eq!(lane.state, EngineLaneState::Working);
    assert_eq!(lane.story_id.as_deref(), Some(id.as_str()));
    assert!(
        engine.status(Some(&run.id)).unwrap()[0].lanes[0]
            .probe_detail
            .as_ref()
            .unwrap()
            .contains("continuation")
    );
}
#[test]
fn generic_unblock_delivery_yields_to_outstanding_continuation_owner() {
    use storyhook::store::{DeliveryStatus, ReadOps, Store, WriteOps};
    let (f, id) = setup();
    let ctx = f.ctx();
    ContinuationService::new(&ctx, &Runtime)
        .request(&id, input(&id))
        .unwrap();
    StoryService::new(&ctx)
        .set_awaiting(&id, "temporary dependency")
        .unwrap();
    f.store()
        .write(|tx| {
            let mut delivery = tx.block_deliveries(f.project())?.remove(0);
            delivery.status = DeliveryStatus::Delivered;
            delivery.target = Some("verified-target".into());
            tx.update_block_delivery(&delivery, DeliveryStatus::Pending)?;
            Ok(())
        })
        .unwrap();
    StoryService::new(&ctx).clear_awaiting(&id).unwrap();
    storyhook::daemon::block_delivery::process_one(
        f.store(),
        f.env(),
        Some(std::path::Path::new("/nonexistent-continuation-helper")),
    )
    .unwrap();
    let deliveries = f
        .store()
        .read(|tx| tx.block_deliveries(f.project()))
        .unwrap();
    assert_eq!(deliveries[1].status, DeliveryStatus::Superseded);
    assert!(deliveries[1].detail.contains("continuation"));
}

#[test]
fn native_feedback_is_claimed_once_by_the_atomic_request_creator() {
    let (f, id) = setup();
    let ctx = f.ctx();
    let service = ContinuationService::new(&ctx, &Runtime);
    let (first, feedback) = service.request_with_receipt(&id, input(&id)).unwrap();
    assert!(feedback);
    let (duplicate, feedback) = service.request_with_receipt(&id, input(&id)).unwrap();
    assert_eq!(first.id, duplicate.id);
    assert!(!feedback);
}
fn fixture_git(f: &ServiceFixture, args: &[&str]) -> String {
    let result = std::process::Command::new("git")
        .arg("-C")
        .arg(f.cwd())
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8_lossy(&result.stdout).trim().into()
}
struct GitRuntime<'a>(&'a ServiceFixture);
impl ContinuationRuntime for GitRuntime<'_> {
    fn call(&self, operation: &str, input: &Value) -> Result<Value, AppError> {
        let mut capture = if operation == "capture" {
            Runtime.call(operation, input)?["capture"].clone()
        } else {
            input["capture"].clone()
        };
        capture["head"] = json!(fixture_git(self.0, &["rev-parse", "HEAD"]));
        capture["lease"]["worktree_path"] = json!(self.0.cwd().canonicalize().unwrap());
        capture["lease"]["branch"] = json!("work");
        Ok(json!({"ok":true,"phase":"busy","capture":capture}))
    }
}
#[test]
fn managed_submission_requires_clean_reviewed_worktree() {
    let (f, id) = setup();
    fixture_git(&f, &["init", "-b", "work"]);
    fixture_git(&f, &["commit", "--allow-empty", "-m", "initial"]);
    let ctx = f.ctx();
    let runtime = GitRuntime(&f);
    let service = ContinuationService::new(&ctx, &runtime);
    let request = service.request(&id, input(&id)).unwrap();
    let head = fixture_git(&f, &["rev-parse", "HEAD"]);
    service
        .ack(
            &id,
            &request.id,
            sequence(&service, &id),
            &head,
            "codex",
            "session-1",
        )
        .unwrap();
    std::fs::write(f.cwd().join("unfinished.rs"), "preserve uncommitted work").unwrap();
    let error = StoryService::new(&ctx)
        .set_state(&id, "verifying", None, None, None)
        .unwrap_err();
    assert!(error.to_string().contains("clean"), "{error}");
    std::fs::remove_file(f.cwd().join("unfinished.rs")).unwrap();
    fixture_git(&f, &["switch", "-c", "foreign-branch"]);
    let error = StoryService::new(&ctx)
        .set_state(&id, "verifying", None, None, None)
        .unwrap_err();
    assert!(error.to_string().contains("retained worktree and branch"));
    fixture_git(&f, &["switch", "work"]);

    StoryService::new(&ctx)
        .set_state(&id, "verifying", None, None, None)
        .unwrap();
}

#[test]
fn provider_transport_passes_literal_json_on_stdin_and_reports_invalid_replies() {
    use storyhook::service::continuation::PythonRuntime;
    let f = ServiceFixture::new();
    let helper = f.cwd().join("runtime.py");
    std::fs::write(&helper,"import json,sys\nv=json.load(sys.stdin)\nprint(json.dumps({'ok':True,'phase':'busy','echo':v,'operation':sys.argv[1]}))\n").unwrap();
    let runtime = PythonRuntime::at(&helper, f.env().clone());
    let document = json!({"capture":{"provider":"codex"},"evidence":"literal\n$(never execute) `never execute` ' \""});
    let answer = runtime.call("observe", &document).unwrap();
    assert_eq!(answer["echo"], document);
    assert_eq!(answer["operation"], "observe");
    let broken = f.cwd().join("invalid-runtime.py");
    std::fs::write(&broken, "print('not JSON')\n").unwrap();
    let error = PythonRuntime::at(&broken, f.env().clone())
        .call("observe", &document)
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("invalid continuation observe response")
    );
}
