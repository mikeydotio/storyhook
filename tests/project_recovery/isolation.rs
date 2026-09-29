//! One invalid recovery stops only the stories it names (SH-848).
//!
//! The fixture strands a decided recovery exactly as `story delete` did
//! before SH-848: the claim into SH-1 is retracted and SH-1 is purged under
//! the recovery, so its exact event references no longer resolve. Readers
//! that ask about another story must not fail; readers for a story the
//! record names, and the diagnostics that read every record, stay loud.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use storyhook::daemon::{lifecycle::InFlight, verification::*};
use storyhook::error::AppError;
use storyhook::service::RelationService;
use storyhook::service::engine::{EngineService, StartRequest};
use storyhook::service::project_recovery::{RecoveryView, RepairInput};
use storyhook::store::{
    BlockAction, EngineAgent, EngineLaneState, EngineScope, VerificationFailureDisposition,
    VerificationIncident,
};
use storyhook_test_support::{DispatcherStep, FakeDispatcher};

/// A decided recovery whose subject SH-1 was purged; SH-2 is its repair.
fn stranded(f: &ServiceFixture) -> RecoveryView {
    let view = resume::decided(f);
    RelationService::new(&f.ctx())
        .relate("SH-2", "blocks", "SH-1", true)
        .unwrap();
    f.store()
        .write(|tx| tx.purge_story(f.project(), StoryNo::new(1)).map(|_| ()))
        .unwrap();
    assert!(
        ProjectRecoveryService::new(&f.ctx())
            .show(&view.record.id)
            .is_err(),
        "the fixture must leave the recovery invalid"
    );
    view
}

#[test]
fn an_unrelated_story_still_enters_the_verification_queue() {
    let f = fixture();
    let view = stranded(&f);
    let candidate = submitted(&f, "unrelated submission");
    assert_eq!(candidate.story_id, "SH-3");
    assert!(
        VerificationQueue::new(f.store())
            .ordered_for(f.project())
            .unwrap()
            .iter()
            .any(|c| c.story_id == "SH-3")
    );
    // Repair show still reads every reference and still says so.
    assert!(
        ProjectRecoveryService::new(&f.ctx())
            .show(&view.record.id)
            .is_err()
    );
}

fn created(f: &ServiceFixture, title: &str) -> String {
    StoryService::new(&f.ctx())
        .create(&NewStoryInput {
            title: title.into(),
            ..Default::default()
        })
        .unwrap()
        .id
}

fn input() -> RepairInput {
    RepairInput {
        base: "a".repeat(40),
        head: "b".repeat(40),
        head_tree: "e".repeat(40),
        tree: "f".repeat(40),
    }
}

fn gate<'a>(f: &'a ServiceFixture, activity: &'a VerificationActivity) -> queue::GateEndpoint<'a> {
    queue::GateEndpoint {
        store: f.store(),
        env: f.env(),
        activity,
        input: input(),
        mismatch: false,
        fail_tests: false,
        project_fault: false,
        executions: AtomicUsize::new(0),
    }
}

fn tick(
    f: &ServiceFixture,
    gate: &queue::GateEndpoint<'_>,
    activity: &VerificationActivity,
) -> Result<TickResult, AppError> {
    tick_with_activity(
        f.store(),
        f.env(),
        gate,
        activity,
        &InFlight::new(f.env().clone()),
        f.project(),
    )
}

/// Repair admission, repair completion and landing each ask which active
/// recovery owns the candidate. None names SH-3, so one full tick verifies
/// and lands it.
#[test]
fn one_verifier_tick_verifies_and_lands_an_unrelated_story() {
    let f = fixture();
    stranded(&f);
    submitted(&f, "unrelated submission");
    let activity = VerificationActivity::new();
    let gate = gate(&f, &activity);
    assert_eq!(tick(&f, &gate, &activity).unwrap(), TickResult::Completed);
    assert_eq!(gate.executions.load(Ordering::SeqCst), 1);
    let row = f
        .store()
        .read(|tx| tx.story(f.project(), StoryNo::new(3)))
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "done");
}

/// Incident reconcile runs before the queue on every tick. The incident is
/// not corroborated by any recovery, so it keeps its halt instead of
/// failing the tick.
#[test]
fn a_halted_incident_for_an_unrelated_story_still_halts_cleanly() {
    let f = fixture();
    stranded(&f);
    let candidate = submitted(&f, "unrelated halt");
    let now = f.ctx().now();
    let incident = VerificationIncident {
        incident_id: "unrelated-halt".into(),
        project: f.project(),
        story: StoryNo::new(3),
        generation: candidate.verifying_generation.unwrap(),
        disposition: VerificationFailureDisposition::Permanent,
        halted: true,
        attempts: 1,
        detail: serde_json::to_string(&fault()).unwrap(),
        first_failed_at: now.clone(),
        last_failed_at: now,
    };
    f.store()
        .write(|tx| tx.put_verification_incident(&incident))
        .unwrap();
    let activity = VerificationActivity::new();
    let gate = gate(&f, &activity);
    assert_eq!(tick(&f, &gate, &activity).unwrap(), TickResult::Halted);
    assert_eq!(gate.executions.load(Ordering::SeqCst), 0);
    assert_eq!(
        f.store()
            .read(|tx| tx.verification_incident(f.project()))
            .unwrap(),
        Some(incident)
    );
}

/// Every story write derives block edges, and unblocking an active story
/// asks whether a recovery owns its resume.
#[test]
fn unblocking_an_unrelated_active_story_still_writes_its_resume() {
    let f = fixture();
    stranded(&f);
    let ctx = f.ctx();
    let stories = StoryService::new(&ctx);
    let blocker = created(&f, "blocker");
    let active = created(&f, "active work");
    stories
        .set_state(&active, "in-progress", None, None, None)
        .unwrap();
    RelationService::new(&ctx)
        .relate(&blocker, "blocks", &active, false)
        .unwrap();
    stories
        .set_state(&blocker, "done", None, None, None)
        .unwrap();
    assert!(
        f.store()
            .read(|tx| tx.block_deliveries(f.project()))
            .unwrap()
            .iter()
            .any(|d| d.story == StoryNo::new(4) && d.action == BlockAction::Resume),
        "the ordinary resume must be enqueued"
    );
}

/// Full Auto asks, lane by lane, whether a recovery owns the lane's story.
#[test]
fn full_auto_restart_reconciles_an_unrelated_lane() {
    let f = fixture();
    stranded(&f);
    let ctx = f.ctx();
    let id = created(&f, "unrelated lane");
    StoryService::new(&ctx)
        .set_state(&id, "in-progress", None, None, None)
        .unwrap();
    let endpoint = FakeDispatcher::new([DispatcherStep::WindowAlive {
        window: format!("=fixture:={id}"),
        alive: false,
    }]);
    let engine = EngineService::new(&ctx, &endpoint);
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
    let mut lane = f
        .store()
        .read(|tx| tx.engine_lanes(&run.id))
        .unwrap()
        .remove(0);
    lane.state = EngineLaneState::Working;
    lane.story_id = Some(id.clone());
    lane.window_name = Some(id);
    f.store().write(|tx| tx.put_engine_lane(&lane)).unwrap();
    engine.reconcile_after_restart(&run.id).unwrap();
}

/// A reader for SH-2, which the invalid record names, must not guess.
fn assert_repair_admission_fails_closed(f: &ServiceFixture) {
    let ctx = f.ctx();
    PrLinkService::new(&ctx)
        .link("SH-2", "https://github.com/acme/widgets/pull/2", true)
        .unwrap();
    StoryService::new(&ctx)
        .set_state("SH-2", "verifying", None, None, None)
        .unwrap();
    let candidate = VerificationQueue::new(f.store())
        .ordered_for(f.project())
        .unwrap()
        .into_iter()
        .find(|c| c.story_id == "SH-2")
        .expect("the repair is queued");
    let error = ProjectRecoveryService::new(&ctx)
        .admit_repair(&candidate, "named-repair", &input())
        .unwrap_err();
    assert!(matches!(error, AppError::Integrity(_)), "{error:?}");
}

#[test]
fn readers_for_a_story_the_invalid_record_names_still_fail_closed() {
    let f = fixture();
    stranded(&f);
    assert_repair_admission_fails_closed(&f);
}

/// Corrupting the one field that names the repair still leaves SH-2 named
/// through its work target, so the record keeps failing closed for SH-2
/// while unrelated stories proceed.
#[test]
fn a_record_with_one_corrupted_story_field_still_names_its_other_stories() {
    let f = fixture();
    let view = resume::decided(&f);
    let mut record = view.record.clone();
    record.state["decision"]["repair_story"] = serde_json::json!(99);
    let expected = record.revision;
    record.revision += 1;
    assert!(
        f.store()
            .write(|tx| tx.update_project_recovery(&record, expected))
            .unwrap()
    );
    assert!(
        ProjectRecoveryService::new(&f.ctx())
            .show(&view.record.id)
            .is_err()
    );
    submitted(&f, "unrelated submission");
    assert_repair_admission_fails_closed(&f);
}
