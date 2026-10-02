//! A manual resume supersedes the context-handoff chain it replaces (SH-850).
//!
//! After a reboot a story's handoff stays `needs-attention`, which is still
//! outstanding: the resumed session's first handoff would be refused, and its
//! submission fenced. An acknowledged handoff fences submission on a review
//! only the lost session could refresh. `story internal
//! supersede-continuations` retires that chain in one transaction, and refuses
//! (changing nothing) while a delivery may be in flight.
use serde_json::{Value, json};
use storyhook::cli::{parse_invocation, split_global_flags};
use storyhook::error::AppError;
use storyhook::invoke::dispatch;
use storyhook::output::Response;
use storyhook::service::continuation::{ContinuationRuntime, ContinuationService};
use storyhook::service::{NewStoryInput, StoryService};
use storyhook::store::{Continuation, ContinuationStatus, ReadOps, Store, WriteOps};
use storyhook_test_support::ServiceFixture;

/// Answers only `capture`, with the ownership evidence a real runtime binds.
struct Runtime;
impl ContinuationRuntime for Runtime {
    fn call(&self, operation: &str, input: &Value) -> Result<Value, AppError> {
        assert_eq!(operation, "capture");
        Ok(json!({"ok":true,"capture":{
            "lease":{"version":1,"project_slug":"fixture","story_id":input["handoff"]["story_id"],"repository_path":"/tmp/repo","worktree_path":"/tmp/repo/lane","branch":"work","tmux":{"socket_path":"/tmp/tmux"}},
            "head":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","fingerprint":"dirty-1","provider":"codex","session_id":"session-1","turn_id":input["origin"]["turn_id"],"message_id":input["origin"]["message_id"],"mode":"default","socket":"/tmp/tmux","pane":"%1","pid":123,"started":"start","model":"gpt","effort":"high","speed":"standard","autonomy":true
        }}))
    }
}

fn story(f: &ServiceFixture, title: &str) -> String {
    let id = StoryService::new(&f.ctx())
        .create(&NewStoryInput {
            title: title.into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    StoryService::new(&f.ctx())
        .set_state(&id, "in-progress", None, None, None)
        .unwrap();
    id
}

fn fixture() -> ServiceFixture {
    let mut f = ServiceFixture::new();
    f.set_clock(storyhook::service::Clock::System);
    f
}

fn records(f: &ServiceFixture) -> Vec<Continuation> {
    f.store().read(|tx| tx.continuations(f.project())).unwrap()
}

fn record(f: &ServiceFixture, request: &str) -> Continuation {
    records(f).into_iter().find(|r| r.id == request).unwrap()
}

/// Rewrites one stored request, as the monitor and the review do.
fn set(f: &ServiceFixture, request: &str, change: impl FnOnce(&mut Continuation)) {
    let mut row = record(f, request);
    let expected = row.revision;
    change(&mut row);
    row.revision += 1;
    f.store()
        .write(|tx| {
            assert!(tx.update_continuation(&row, expected)?);
            Ok(())
        })
        .unwrap();
}

/// Files one context handoff for `id` and leaves it in `status`. A story
/// admits one unresolved handoff at a time, so each is filed acknowledged
/// first and moved to its final status by [`finalize`].
fn handoff(f: &ServiceFixture, id: &str, message: &str) -> String {
    let input = json!({"provider":"codex","origin":{"session_id":"session-1","turn_id":format!("turn-{message}"),"message_id":message},"handoff":{"type":"storyhook.session-handoff","version":1,"story_id":id,"kind":"context","evidence":{"context":"context is exhausted","outstanding_work":"finish regression tests"}}});
    let request = ContinuationService::new(&f.ctx(), &Runtime)
        .request(id, input)
        .unwrap()
        .id;
    set(f, &request, |row| {
        row.status = ContinuationStatus::Acknowledged
    });
    request
}

fn finalize(f: &ServiceFixture, request: &str, status: ContinuationStatus) {
    set(f, request, |row| {
        row.status = status;
        row.detail = "state before the resume".into();
    });
}

fn supersede(f: &ServiceFixture, id: &str) -> Result<Value, String> {
    let args = [
        "--project",
        "fixture",
        "internal",
        "supersede-continuations",
        id,
        "--json",
    ]
    .map(str::to_owned);
    let (_, args) = split_global_flags(&args).map_err(|error| error.to_string())?;
    let invocation = parse_invocation(&args).map_err(|error| error.to_string())?;
    match dispatch(&f.ctx(), invocation).map_err(|error| error.to_string())? {
        Response::RawJson(json) => serde_json::from_str(&json).map_err(|error| error.to_string()),
        other => Err(format!("expected protocol receipt, got {other:?}")),
    }
}

fn ids(value: &Value) -> Vec<String> {
    let mut ids: Vec<String> = value
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap().to_owned())
        .collect();
    ids.sort();
    ids
}

fn sorted(mut ids: Vec<String>) -> Vec<String> {
    ids.sort();
    ids
}

#[test]
fn a_resume_supersedes_every_live_link_of_the_chain_and_nothing_else() {
    // The store admits one unresolved handoff per story, so each outstanding
    // status gets its own story, each behind an acknowledged earlier handoff
    // and a handoff that is already retired.
    let f = fixture();
    let neighbor = story(&f, "Another claimed story");
    let adjacent = handoff(&f, &neighbor, "n1");
    finalize(&f, &adjacent, ContinuationStatus::NeedsAttention);
    for (n, status) in [
        ContinuationStatus::Pending,
        ContinuationStatus::AwaitingAck,
        ContinuationStatus::NeedsAttention,
    ]
    .into_iter()
    .enumerate()
    {
        let id = story(&f, &format!("Resumed after a reboot {n}"));
        let retired = handoff(&f, &id, &format!("r{n}"));
        finalize(&f, &retired, ContinuationStatus::Superseded);
        let acknowledged = handoff(&f, &id, &format!("a{n}"));
        let outstanding = handoff(&f, &id, &format!("o{n}"));
        finalize(&f, &outstanding, status);

        let receipt = supersede(&f, &id).unwrap();
        assert_eq!(receipt["protocol_version"], 1);
        assert_eq!(receipt["project"], "fixture");
        assert_eq!(receipt["story_id"], id.as_str());
        assert_eq!(
            ids(&receipt["superseded"]),
            sorted(vec![acknowledged.clone(), outstanding.clone()]),
            "{status:?}"
        );
        assert_eq!(receipt["attempting"], json!([]));
        for request in [&acknowledged, &outstanding] {
            let row = record(&f, request);
            assert_eq!(row.status, ContinuationStatus::Superseded, "{status:?}");
            assert!(row.detail.contains("manual resume"), "{}", row.detail);
        }
        assert_eq!(record(&f, &retired).detail, "state before the resume");
        let again = supersede(&f, &id).unwrap();
        assert_eq!(
            again["superseded"],
            json!([]),
            "a repeat supersedes nothing"
        );
    }
    assert_eq!(
        record(&f, &adjacent).status,
        ContinuationStatus::NeedsAttention,
        "another story's handoff is untouched"
    );
}

#[test]
fn an_obviation_hold_is_left_for_its_person() {
    let f = fixture();
    let id = story(&f, "Held for an obviation review");
    let acknowledged = handoff(&f, &id, "a");
    let obviation = handoff(&f, &id, "o");
    set(&f, &obviation, |row| {
        row.handoff["kind"] = json!("obviation-review");
        row.status = ContinuationStatus::NeedsAttention;
        row.detail = "held for a person".into();
    });

    let receipt = supersede(&f, &id).unwrap();
    assert_eq!(ids(&receipt["superseded"]), vec![acknowledged]);
    let held = record(&f, &obviation);
    assert_eq!(held.status, ContinuationStatus::NeedsAttention);
    assert_eq!(held.detail, "held for a person");
}

#[test]
fn a_delivery_in_flight_refuses_the_whole_replacement() {
    let f = fixture();
    let id = story(&f, "Its monitor is mid-delivery");
    handoff(&f, &id, "m1");
    let attempting = handoff(&f, &id, "m2");
    finalize(&f, &attempting, ContinuationStatus::Attempting);
    let before = records(&f);

    let receipt = supersede(&f, &id).unwrap();
    assert_eq!(receipt["superseded"], json!([]));
    assert_eq!(ids(&receipt["attempting"]), vec![attempting]);
    assert_eq!(records(&f), before, "a refused replacement changes nothing");
}

#[test]
fn after_the_replacement_the_resumed_session_can_submit() {
    let f = fixture();
    let id = story(&f, "Its review was acknowledged by the lost session");
    handoff(&f, &id, "m1");
    // Any later story event makes the lost session's review stale, and only
    // that session could refresh it.
    StoryService::new(&f.ctx())
        .comment(&id, "the resumed session reports what it found")
        .unwrap();
    let fenced = StoryService::new(&f.ctx())
        .set_state(&id, "verifying", None, None, None)
        .unwrap_err()
        .to_string();
    assert!(fenced.contains("continuation"), "{fenced}");

    supersede(&f, &id).unwrap();
    StoryService::new(&f.ctx())
        .set_state(&id, "verifying", None, None, None)
        .expect("no superseded handoff fences the resumed session's submission");
}

#[test]
fn missing_foreign_or_malformed_requests_refuse_and_change_nothing() {
    let f = fixture();
    let id = story(&f, "Owned story");
    let attention = handoff(&f, &id, "m1");
    finalize(&f, &attention, ContinuationStatus::NeedsAttention);
    for other in ["SH-99999", "OTHER-1", "0", "SH-01"] {
        assert!(supersede(&f, other).is_err(), "refuse {other}");
    }
    for request in [
        "internal supersede-continuations",
        "internal supersede-continuations SH-1 SH-2",
        "internal supersede-continuations --force",
    ] {
        let args: Vec<_> = request.split_whitespace().map(str::to_owned).collect();
        assert!(parse_invocation(&args).is_err(), "refuse {request}");
    }
    assert_eq!(
        record(&f, &attention).status,
        ContinuationStatus::NeedsAttention
    );
}
