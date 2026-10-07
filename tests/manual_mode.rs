//! Manual mode is project-scoped, durable, and keeps ordinary validation.
use storyhook::service::{NewStoryInput, SettingsService, StoryService};
use storyhook::store::{ReadOps, Store};
use storyhook_test_support::ServiceFixture;

#[test]
fn manual_mode_defaults_on_and_round_trips_without_losing_settings() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let settings = SettingsService::new(&ctx);
    assert_eq!(
        settings
            .get("automations.enabled")
            .unwrap()
            .value
            .as_deref(),
        Some("true")
    );
    settings.set("cleanup.interval", "2d").unwrap();
    settings.set("automations.enabled", "false").unwrap();
    assert!(
        !fixture
            .store()
            .read(|tx| tx.automations_enabled(fixture.project()))
            .unwrap()
    );
    assert_eq!(
        settings.get("cleanup.interval").unwrap().value.as_deref(),
        Some("2d")
    );
    assert!(settings.set("automations.enabled", "maybe").is_err());
    settings.unset("automations.enabled").unwrap();
    assert!(
        fixture
            .store()
            .read(|tx| tx.automations_enabled(fixture.project()))
            .unwrap()
    );
}

#[test]
fn manual_completion_needs_no_verifier_receipt_but_unknown_states_fail() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = service
        .create(&NewStoryInput {
            title: "Manual task".into(),
            ..Default::default()
        })
        .unwrap();
    service
        .set_state(&story.id, "verifying", None, None, None)
        .unwrap();
    assert!(
        service
            .set_state(&story.id, "done", None, None, None)
            .is_err()
    );
    SettingsService::new(&ctx)
        .set("automations.enabled", "false")
        .unwrap();
    assert!(
        service
            .set_state(&story.id, "unknown", None, None, None)
            .is_err()
    );
    assert_eq!(
        service
            .set_state(&story.id, "done", None, None, None)
            .unwrap()
            .state,
        "done"
    );
}

#[test]
fn manual_project_isolated_and_persistent_and_never_replays_old_submissions() {
    use storyhook::service::{Ctx, VerificationQueue};
    use storyhook::store::SqliteStore;
    let fixture = ServiceFixture::new();
    let other = fixture.add_project("other", "OT");
    let ctx = fixture.ctx();
    let other_ctx = Ctx::new(fixture.store(), other, ctx.cwd(), ctx.env().clone());
    let story = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "Before toggle".into(),
            ..Default::default()
        })
        .unwrap();
    StoryService::new(&ctx)
        .set_state(&story.id, "verifying", None, None, None)
        .unwrap();
    let other_story = StoryService::new(&other_ctx)
        .create(&NewStoryInput {
            title: "Other project".into(),
            ..Default::default()
        })
        .unwrap();
    StoryService::new(&other_ctx)
        .set_state(&other_story.id, "verifying", None, None, None)
        .unwrap();
    SettingsService::new(&ctx)
        .set("automations.enabled", "false")
        .unwrap();
    assert!(
        VerificationQueue::new(fixture.store())
            .ordered_for(ctx.project())
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        VerificationQueue::new(fixture.store())
            .ordered_for(other)
            .unwrap()
            .len(),
        1
    );
    let reopened = SqliteStore::open(ctx.env().store_path()).unwrap();
    assert!(
        !reopened
            .read(|tx| tx.automations_enabled(ctx.project()))
            .unwrap()
    );
    SettingsService::new(&ctx)
        .set("automations.enabled", "true")
        .unwrap();
    assert!(
        VerificationQueue::new(fixture.store())
            .ordered_for(ctx.project())
            .unwrap()
            .is_empty()
    );
    StoryService::new(&ctx)
        .set_state(&story.id, "todo", None, None, None)
        .unwrap();
    StoryService::new(&ctx)
        .set_state(&story.id, "verifying", None, None, None)
        .unwrap();
    assert_eq!(
        VerificationQueue::new(fixture.store())
            .ordered_for(ctx.project())
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn manual_blocked_story_can_advance_without_delivery_intent() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let blocker = service
        .create(&NewStoryInput {
            title: "Blocker".into(),
            ..Default::default()
        })
        .unwrap();
    let input = NewStoryInput {
        title: "Blocked task".into(),
        blocked_by: vec![blocker.id],
        state: Some("in-progress".into()),
        ..Default::default()
    };
    assert!(service.create(&input).is_err());
    SettingsService::new(&ctx)
        .set("automations.enabled", "false")
        .unwrap();
    let story = service.create(&input).unwrap();
    service
        .set_state(&story.id, "verifying", None, None, None)
        .unwrap();
    service
        .set_fields(
            &story.id,
            &storyhook::service::FieldEdits {
                state: Some("done".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(
        fixture
            .store()
            .read(|tx| tx.block_deliveries(ctx.project()))
            .unwrap()
            .is_empty()
    );
    assert!(
        fixture
            .store()
            .read(|tx| tx.closure_cleanups(ctx.project()))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn already_started_legacy_merge_hook_cannot_close_a_manual_story() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = service
        .create(&NewStoryInput {
            title: "Legacy hook".into(),
            ..Default::default()
        })
        .unwrap();
    SettingsService::new(&ctx)
        .set("automations.enabled", "false")
        .unwrap();
    let unchanged = service
        .set_state(&story.id, "done", Some("auto-closed by merge"), None, None)
        .unwrap();
    assert_eq!(unchanged.state, story.state);
    assert_eq!(
        service
            .set_state(&story.id, "done", None, None, None)
            .unwrap()
            .state,
        "done"
    );
}

#[test]
fn manual_board_displays_the_stored_state_despite_blockers() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let blocker = service
        .create(&NewStoryInput {
            title: "Blocker".into(),
            ..Default::default()
        })
        .unwrap();
    let story = service
        .create(&NewStoryInput {
            title: "Keep my state".into(),
            blocked_by: vec![blocker.id],
            ..Default::default()
        })
        .unwrap();
    SettingsService::new(&ctx)
        .set("automations.enabled", "false")
        .unwrap();
    let view = fixture
        .store()
        .read(|tx| {
            Ok(
                storyhook::service::QueryService::new(tx, ctx.project(), &ctx.now())
                    .show(&story.id)?,
            )
        })
        .unwrap();
    assert_eq!(view.story.state, "todo");
    assert_eq!(view.display_state, None);
}

#[test]
fn failed_stale_intent_retirement_never_persists_reenabled_automations() {
    use storyhook::store::{
        Continuation, ContinuationPhase, ContinuationStatus, SqliteStore, StoryNo, WriteOps,
    };
    for unset in [false, true] {
        let fixture = ServiceFixture::new();
        let ctx = fixture.ctx();
        let settings = SettingsService::new(&ctx);
        let story = StoryService::new(&ctx)
            .create(&NewStoryInput {
                title: "Interrupted disable".into(),
                ..Default::default()
            })
            .unwrap();
        settings.set("automations.enabled", "false").unwrap();
        // Durable state left if disabling crashed before retiring this intent.
        fixture
            .store()
            .write(|tx| {
                tx.insert_continuation(&Continuation {
                    id: "old-handoff".into(),
                    project_id: ctx.project(),
                    story_no: StoryNo::new(1),
                    story_id: story.id,
                    handoff: serde_json::json!({}),
                    generation: serde_json::json!({}),
                    capture: serde_json::json!({}),
                    status: ContinuationStatus::Pending,
                    phase: ContinuationPhase::Observe,
                    revision: 0,
                    attempts: 0,
                    created_at: ctx.now(),
                    updated_at: ctx.now(),
                    detail: String::new(),
                    reviewed_seq: None,
                    reviewed_head: None,
                })
            })
            .unwrap();
        let connection = rusqlite::Connection::open(fixture.store().path()).unwrap();
        connection
            .execute_batch(
                "CREATE TRIGGER fail_retirement BEFORE UPDATE ON continuations
                 BEGIN SELECT RAISE(ABORT, 'retirement interrupted'); END;",
            )
            .unwrap();
        let enable = || {
            if unset {
                settings.unset("automations.enabled")
            } else {
                settings.set("automations.enabled", "true")
            }
        };
        assert!(enable().is_err());
        let reopened = SqliteStore::open(fixture.store().path()).unwrap();
        assert!(
            !reopened
                .read(|tx| tx.automations_enabled(ctx.project()))
                .unwrap()
        );
        assert_eq!(
            reopened.read(|tx| tx.continuations(ctx.project())).unwrap()[0].status,
            ContinuationStatus::Pending
        );
        connection
            .execute_batch("DROP TRIGGER fail_retirement")
            .unwrap();
        enable().unwrap();
        reopened
            .read(|tx| {
                assert!(tx.automations_enabled(ctx.project())?);
                assert_eq!(
                    tx.continuations(ctx.project())?[0].status,
                    ContinuationStatus::Superseded
                );
                Ok(())
            })
            .unwrap();
    }
}
