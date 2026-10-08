//! A cleanup owner must not stall unrelated lanes in the same run.
use super::*;
use storyhook::service::reset::ResetReservation;
use storyhook::service::resources::ResourceService;
use storyhook::service::story_reset::StoryResetService;
use storyhook::store::{DroppedCleanup, DroppedCleanupPhase, StoryNo};

#[derive(Clone, Copy, Debug)]
enum Owner {
    Card,
    FailedCard,
    Native,
    Dropped,
}

fn reserve(fixture: &ServiceFixture, owner: Owner) {
    let ctx = fixture.ctx_with_subprocess_patience();
    match owner {
        Owner::Card | Owner::FailedCard => {
            let service = StoryResetService::new(&ctx);
            let receipt = service.reserve("SH-1", "SH-1").unwrap();
            if matches!(owner, Owner::FailedCard) {
                // An interrupted attempt leaves its last obstacle on the
                // unfinished receipt; a reset never records a terminal failure.
                let mut interrupted = receipt;
                interrupted.failure = Some("controlled cleanup failure".into());
                fixture
                    .store()
                    .write(|tx| tx.put_story_reset(&interrupted))
                    .unwrap();
            }
        }
        Owner::Native => {
            let reservation = serde_json::to_string(&ResetReservation {
                operation: "native-owner".into(),
                lease: None,
                force: false,
                previous_awaiting: None,
                detail: "reset owns this lane".into(),
            })
            .unwrap();
            fixture
                .store()
                .write(|tx| {
                    tx.put_legacy_story_reset(
                        fixture.project(),
                        StoryNo::new(1),
                        Some(&reservation),
                    )
                })
                .unwrap();
        }
        Owner::Dropped => {
            StoryService::new(&ctx)
                .set_state("SH-1", "dropped", None, None, None)
                .unwrap();
            let row = fixture
                .store()
                .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
                .unwrap()
                .unwrap();
            let resources = ResourceService::new(&ctx)
                .resolve("SH-1", &Default::default())
                .unwrap();
            let record = DroppedCleanup {
                project: fixture.project(),
                story: StoryNo::new(1),
                token: "drop-owner".into(),
                generation: row.head_global_seq,
                lease: cleanup_lease("SH-1", "/owned/SH-1"),
                resources,
                paths: vec![],
                process_start: None,
                phase: DroppedCleanupPhase::Prepared,
                released: false,
                failure: Some("cleanup must be retried".into()),
            };
            fixture
                .store()
                .write(|tx| tx.put_dropped_cleanup(&record))
                .unwrap();
        }
    }
}

fn release(fixture: &ServiceFixture, owner: Owner) {
    fixture
        .store()
        .write(|tx| {
            let project = fixture.project();
            let story = StoryNo::new(1);
            match owner {
                Owner::Card | Owner::FailedCard => {
                    let mut receipt = tx.story_reset(project, story)?.unwrap();
                    receipt.completed = true;
                    receipt.failure = None;
                    tx.put_story_reset(&receipt)
                }
                Owner::Native => tx.put_legacy_story_reset(project, story, None),
                Owner::Dropped => {
                    let mut cleanup = tx.dropped_cleanup(project, story)?.unwrap();
                    cleanup.released = true;
                    tx.put_dropped_cleanup(&cleanup)
                }
            }
        })
        .unwrap();
}

fn setup(fixture: &ServiceFixture, owner: Owner, lanes: u32) -> (String, EngineLaneRecord) {
    let ctx = fixture.ctx_with_subprocess_patience();
    StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "Owned quarantine".into(),
            state: Some("in-progress".into()),
            ..Default::default()
        })
        .unwrap();
    let run = started_run(fixture, &FakeDispatcher::default(), lanes);
    occupy(fixture, &run, 0, "SH-1");
    let mut lane = lane_at(fixture, &run, 0);
    lane.state = EngineLaneState::Quarantined;
    lane.outcome = Some("window-gone".into());
    lane.outcome_detail = Some("retained diagnosis".into());
    fixture
        .store()
        .write(|tx| tx.put_engine_lane(&lane))
        .unwrap();
    fixture
        .store()
        .write(|tx| tx.set_checkout_path(fixture.project(), None))
        .unwrap();
    reserve(fixture, owner);
    (run, lane)
}

#[test]
fn cleanup_owned_quarantine_does_not_stall_other_lanes() {
    for owner in [
        Owner::Card,
        Owner::FailedCard,
        Owner::Native,
        Owner::Dropped,
    ] {
        let fixture = ServiceFixture::new();
        let (run, held) = setup(&fixture, owner, 2);
        let ctx = fixture.ctx_with_subprocess_patience();
        let before = fixture
            .store()
            .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
            .unwrap();
        let fake = FakeDispatcher::default();
        let engine = EngineService::new(&ctx, &fake);
        for _ in 0..2 {
            engine
                .reconcile(&run)
                .unwrap_or_else(|error| panic!("{owner:?}: {error}"));
            assert_eq!(lane_at(&fixture, &run, 0), held, "{owner:?}");
            assert_eq!(run_state(&fixture, &run), EngineRunState::Running);
        }
        let ready = new_story(&fixture, "Unrelated ready work", &[]);
        let dispatch = FakeDispatcher::new([DispatcherStep::Dispatch(
            DispatchOutcome::from_payload(serde_json::json!({"ok": true, "window": "SH-2"})),
        )]);
        let report = EngineService::new(&ctx, &dispatch).reconcile(&run).unwrap();
        assert_eq!(report.filled, [(1, ready)]);
        assert_eq!(lane_at(&fixture, &run, 0), held);
        assert_eq!(
            fixture
                .store()
                .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
                .unwrap(),
            before
        );
        if matches!(owner, Owner::FailedCard) {
            assert_eq!(
                fixture
                    .store()
                    .read(|tx| tx.story_reset(fixture.project(), StoryNo::new(1)))
                    .unwrap()
                    .unwrap()
                    .failure
                    .as_deref(),
                Some("controlled cleanup failure")
            );
        }
    }
}

#[test]
fn draining_waits_for_cleanup_owned_quarantine_then_finishes() {
    for owner in [
        Owner::Card,
        Owner::FailedCard,
        Owner::Native,
        Owner::Dropped,
    ] {
        let fixture = ServiceFixture::new();
        let (run, held) = setup(&fixture, owner, 1);
        let ctx = fixture.ctx_with_subprocess_patience();
        let fake = FakeDispatcher::default();
        let engine = EngineService::new(&ctx, &fake);
        engine.stop(&run, false).unwrap();
        for _ in 0..2 {
            engine
                .reconcile(&run)
                .unwrap_or_else(|error| panic!("{owner:?}: {error}"));
            assert_eq!(lane_at(&fixture, &run, 0), held);
            assert_eq!(run_state(&fixture, &run), EngineRunState::Draining);
        }
        release(&fixture, owner);
        engine.reconcile(&run).unwrap();
        assert_eq!(run_state(&fixture, &run), EngineRunState::Finished);
        assert_eq!(lane_at(&fixture, &run, 0).state, EngineLaneState::Idle);
        assert!(fake.calls().is_empty());
    }
}
