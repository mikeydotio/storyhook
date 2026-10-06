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
