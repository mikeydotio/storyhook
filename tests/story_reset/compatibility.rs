//! The native and card contracts must never own the same cleanup resources.
use fs4::FileExt;
use storyhook::service::reset::{ResetCaller, ResetReservation, reset_story};
use storyhook::service::story_reset::StoryResetService;
use storyhook::service::{NewStoryInput, StoryService};
use storyhook::store::{BlockAction, ReadOps, Store, StoryNo, WriteOps};
use storyhook_test_support::ServiceFixture;

fn reservation() -> String {
    serde_json::to_string(&ResetReservation {
        operation: "native-reset-owner".into(),
        lease: None,
        force: false,
        previous_awaiting: None,
        detail: "Reserved by native reset".into(),
    })
    .unwrap()
}

#[test]
fn native_and_card_reservations_exclude_each_other_without_losing_the_owner() {
    for native_first in [true, false] {
        let fixture = ServiceFixture::new();
        let ctx = fixture.ctx();
        let story = StoryService::new(&ctx)
            .create(&NewStoryInput {
                title: "One reset contract at a time".into(),
                ..Default::default()
            })
            .unwrap();
        let service = StoryResetService::new(&ctx);
        let native = reservation();
        if native_first {
            fixture
                .store()
                .write(|tx| {
                    tx.put_legacy_story_reset(fixture.project(), StoryNo::new(1), Some(&native))
                })
                .unwrap();
            assert!(service.reserve(&story.id, &story.id).is_err());
            fixture
                .store()
                .read(|tx| {
                    assert_eq!(
                        tx.story_resets(fixture.project())?.get(&StoryNo::new(1)),
                        Some(&native)
                    );
                    assert!(
                        tx.story_reset(fixture.project(), StoryNo::new(1))?
                            .is_none()
                    );
                    Ok(())
                })
                .unwrap();
        } else {
            let card = service.reserve(&story.id, &story.id).unwrap();
            assert!(
                fixture
                    .store()
                    .write(|tx| {
                        tx.put_legacy_story_reset(fixture.project(), StoryNo::new(1), Some(&native))
                    })
                    .is_err()
            );
            fixture
                .store()
                .read(|tx| {
                    assert!(tx.story_resets(fixture.project())?.is_empty());
                    assert_eq!(
                        tx.story_reset(fixture.project(), StoryNo::new(1))?
                            .unwrap()
                            .token,
                        card.token
                    );
                    Ok(())
                })
                .unwrap();
        }
    }
}

#[test]
fn card_reset_waits_for_quiescence_then_acquires_the_shared_workspace_lock() {
    let fixture = ServiceFixture::new();
    let root = storyhook_test_support::scratch_dir();
    let repo = root.path().canonicalize().unwrap();
    let git = storyhook::env::git_env::command(&repo)
        .args(["init", "--initial-branch=main"])
        .output()
        .unwrap();
    assert!(git.status.success());
    fixture
        .store()
        .write(|tx| tx.set_checkout_path(fixture.project(), Some(&repo)))
        .unwrap();
    let ctx = fixture.ctx();
    let story = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "Wait for the actual workspace owner".into(),
            state: Some("in-progress".into()),
            ..Default::default()
        })
        .unwrap();
    let directory = repo.join(".git/storyhook/workspace-locks");
    std::fs::create_dir_all(&directory).unwrap();
    let owner = std::fs::File::create(directory.join(format!("{}.lock", story.id))).unwrap();
    owner.lock_exclusive().unwrap();
    let service = StoryResetService::new(&ctx);
    let reset = service.reserve(&story.id, &story.id).unwrap();
    let mut quiesced = false;
    let error = service
        .execute(&story.id, &reset.token, || {
            quiesced = true;
            Ok(())
        })
        .unwrap_err();
    assert!(
        quiesced,
        "workspace admission must follow dispatch/verifier quiescence"
    );
    assert!(error.to_string().contains("workspace is busy"), "{error}");
    assert!(!service.get(&story.id, &reset.token).unwrap().completed);
    assert_eq!(
        fixture
            .store()
            .read(|tx| Ok(tx.story(fixture.project(), StoryNo::new(1))?.unwrap().state))
            .unwrap(),
        "in-progress"
    );
    assert_eq!(
        service.reserve(&story.id, &story.id).unwrap().token,
        reset.token
    );
    let done = service
        .execute(&story.id, &reset.token, || {
            drop(owner);
            Ok(())
        })
        .unwrap();
    assert!(done.completed);
}

#[test]
fn card_receipts_refuse_payload_identity_that_disagrees_with_the_storage_key() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "Keep reset identity bound to its storage key".into(),
            ..Default::default()
        })
        .unwrap();
    let reset = StoryResetService::new(&ctx)
        .reserve(&story.id, &story.id)
        .unwrap();
    let connection = rusqlite::Connection::open(fixture.env().store_path()).unwrap();
    let original = serde_json::to_value(&reset).unwrap();
    for (field, value) in [
        ("project", serde_json::json!(fixture.project().get() + 1)),
        ("story", serde_json::json!(2)),
        ("token", serde_json::json!("another-reset-owner")),
    ] {
        let mut malformed = original.clone();
        malformed[field] = value;
        connection
            .execute(
                "UPDATE story_resets SET record_json=?1",
                [malformed.to_string()],
            )
            .unwrap();
        let error = fixture
            .store()
            .read(|tx| tx.story_reset(fixture.project(), StoryNo::new(1)))
            .unwrap_err();
        assert!(error.to_string().contains("identity"), "{field}: {error}");
    }
    connection
        .execute(
            "UPDATE story_resets SET record_json=?1",
            [original.to_string()],
        )
        .unwrap();
    assert_eq!(
        StoryResetService::new(&ctx)
            .get(&story.id, &reset.token)
            .unwrap()
            .token,
        reset.token
    );
}

#[test]
fn native_reset_derives_interrupts_and_never_resumes_the_todo_story() {
    for (state, awaiting) in [
        ("todo", None),
        ("in-progress", None),
        ("blocked", None),
        ("in-progress", Some("Human review is required")),
    ] {
        let fixture = ServiceFixture::new();
        let repo = fixture.cwd().canonicalize().unwrap();
        let init = storyhook::env::git_env::command(&repo)
            .args(["init", "--initial-branch=main"])
            .output()
            .unwrap();
        assert!(init.status.success(), "{init:?}");
        fixture
            .store()
            .write(|tx| tx.set_checkout_path(fixture.project(), Some(&repo)))
            .unwrap();
        let ctx = fixture.ctx().no_hooks(true);
        let stories = StoryService::new(&ctx);
        let story = stories
            .create(&NewStoryInput {
                title: "Derive reset block deliveries".into(),
                state: Some(state.into()),
                ..Default::default()
            })
            .unwrap();
        if let Some(reason) = awaiting {
            stories.set_awaiting(&story.id, reason).unwrap();
        }
        let initial = fixture
            .store()
            .read(|tx| tx.block_deliveries(fixture.project()))
            .unwrap()
            .len();
        let mut expected = initial;
        for attempt in 0..2 {
            if awaiting.is_none() && (state != "blocked" || attempt > 0) {
                expected += 1;
            }
            reset_story(&ctx, &story.id, false, &ResetCaller::default()).unwrap();
            fixture
                .store()
                .read(|tx| {
                    let deliveries = tx.block_deliveries(fixture.project())?;
                    assert_eq!(
                        deliveries.len(),
                        expected,
                        "state={state}, awaiting={awaiting:?}, attempt={attempt}"
                    );
                    assert!(
                        deliveries
                            .iter()
                            .all(|delivery| delivery.story == StoryNo::new(1)
                                && delivery.action == BlockAction::Interrupt)
                    );
                    let row = tx.story(fixture.project(), StoryNo::new(1))?.unwrap();
                    assert_eq!(row.state, "todo");
                    assert_eq!(row.awaiting.as_deref(), awaiting);
                    assert!(tx.story_resets(fixture.project())?.is_empty());
                    Ok(())
                })
                .unwrap();
        }
    }
}

#[test]
fn native_reset_admission_rolls_back_its_story_owner_and_delivery_together() {
    use storyhook::store::FaultPoint;
    use storyhook::store::fault::{FaultAction, arm};

    let fixture = ServiceFixture::new();
    let repo = fixture.cwd().canonicalize().unwrap();
    let init = storyhook::env::git_env::command(&repo)
        .args(["init", "--initial-branch=main"])
        .output()
        .unwrap();
    assert!(init.status.success(), "{init:?}");
    fixture
        .store()
        .write(|tx| tx.set_checkout_path(fixture.project(), Some(&repo)))
        .unwrap();
    let ctx = fixture.ctx().no_hooks(true);
    let story = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "Atomic reset admission".into(),
            state: Some("in-progress".into()),
            ..Default::default()
        })
        .unwrap();
    let outcome = {
        let _fault = arm(
            FaultPoint::BeforeCommit,
            FaultAction::Fail("reset admission rollback".into()),
        );
        reset_story(&ctx, &story.id, false, &ResetCaller::default())
    };
    let error = outcome.unwrap_err();
    assert!(
        error.to_string().contains("reset admission rollback"),
        "{error}"
    );
    fixture
        .store()
        .read(|tx| {
            let row = tx.story(fixture.project(), StoryNo::new(1))?.unwrap();
            assert_eq!(row.state, "in-progress");
            assert!(row.awaiting.is_none());
            assert!(tx.story_resets(fixture.project())?.is_empty());
            assert!(tx.block_deliveries(fixture.project())?.is_empty());
            Ok(())
        })
        .unwrap();
}
