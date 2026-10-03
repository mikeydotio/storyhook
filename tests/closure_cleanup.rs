//! Closure intent is committed with the final projection, independent of its producer.
use storyhook::domain::SuperState;
use storyhook::service::{NewStoryInput, StoryService};
use storyhook::store::{ReadOps, Store, StoreError, StoryNo, WriteOps};
use storyhook_test_support::ServiceFixture;

fn request(f: &ServiceFixture) -> Option<serde_json::Value> {
    let conn = rusqlite::Connection::open(f.env().store_path()).unwrap();
    let mut stmt = conn
        .prepare("SELECT record_json FROM closure_cleanups WHERE project_id=?1 AND story_no=1")
        .unwrap();
    stmt.query_map([f.project().get()], |row| row.get::<_, String>(0))
        .unwrap()
        .next()
        .map(|r| serde_json::from_str(&r.unwrap()).unwrap())
}

fn fixture() -> ServiceFixture {
    let f = ServiceFixture::new();
    StoryService::new(&f.ctx())
        .create(&NewStoryInput {
            title: "Close from any door".into(),
            ..Default::default()
        })
        .unwrap();
    f
}

#[test]
fn every_closed_state_gets_one_durable_request_and_reopen_invalidates_it() {
    for state in ["done", "dropped", "cancelled"] {
        let f = fixture();
        let ctx = f.ctx();
        storyhook::service::ConfigService::new(&ctx)
            .add_state("cancelled", SuperState::Closed, None, None)
            .unwrap();
        let service = StoryService::new(&ctx);
        assert!(request(&f).is_none());
        service.set_state("SH-1", state, None, None, None).unwrap();
        let first = request(&f).unwrap();
        assert_eq!(first["completed"], false);
        service
            .comment("SH-1", "Discussion must not enqueue another cleanup")
            .unwrap();
        let row = f
            .store()
            .read(|tx| tx.story(f.project(), StoryNo::new(1)))
            .unwrap()
            .unwrap();
        f.store()
            .write(|tx| tx.put_story(f.project(), &row.snapshot, row.head_seq))
            .unwrap();
        assert_eq!(request(&f).unwrap()["token"], first["token"]);
        service.reopen("SH-1").unwrap();
        assert!(request(&f).is_none());
        service.set_state("SH-1", state, None, None, None).unwrap();
        assert_ne!(request(&f).unwrap()["token"], first["token"]);
    }
}

#[test]
fn rollback_and_transient_closed_projection_do_not_schedule_cleanup() {
    let f = fixture();
    let row = f
        .store()
        .read(|tx| tx.story(f.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    let mut closed = row.snapshot.clone();
    closed.state = "done".into();
    closed.superstate = SuperState::Closed;
    closed.closed_at = Some(f.env().now());
    let result: Result<(), StoreError> = f.store().write(|tx| {
        tx.put_story(f.project(), &closed, row.head_seq)?;
        Err(StoreError::Invariant("roll back this closure".into()))
    });
    assert!(result.is_err());
    assert!(request(&f).is_none());
    f.store()
        .write(|tx| {
            tx.put_story(f.project(), &closed, row.head_seq)?;
            tx.put_story(f.project(), &row.snapshot, row.head_seq)
        })
        .unwrap();
    assert!(request(&f).is_none());
    f.store()
        .write(|tx| tx.put_story(f.project(), &closed, row.head_seq))
        .unwrap();
    assert!(
        request(&f).is_some(),
        "the projection boundary covers producers without a state-change callback"
    );
    // Restore the event-derived projection after exercising the raw store seam.
    f.store()
        .write(|tx| tx.put_story(f.project(), &row.snapshot, row.head_seq))
        .unwrap();
}

#[test]
fn stale_worker_cannot_change_a_reopened_or_reclosed_lifecycle() {
    let f = fixture();
    let ctx = f.ctx();
    let service = StoryService::new(&ctx);
    service.set_state("SH-1", "done", None, None, None).unwrap();
    let mut stale = f
        .store()
        .read(|tx| tx.closure_cleanup(f.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    service.reopen("SH-1").unwrap();
    stale.completed = true;
    assert!(
        !f.store()
            .write(|tx| tx.update_closure_cleanup(&stale))
            .unwrap()
    );
    service.set_state("SH-1", "done", None, None, None).unwrap();
    assert!(
        !f.store()
            .write(|tx| tx.update_closure_cleanup(&stale))
            .unwrap()
    );
    let current = f
        .store()
        .read(|tx| tx.closure_cleanup(f.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert!(!current.completed);
    assert_ne!(current.token, stale.token);
}

#[test]
fn restart_and_projection_rebuild_preserve_completed_receipts() {
    let f = fixture();
    StoryService::new(&f.ctx())
        .set_state("SH-1", "done", None, None, None)
        .unwrap();
    let mut receipt = f
        .store()
        .read(|tx| tx.closure_cleanup(f.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    receipt.completed = true;
    f.store()
        .write(|tx| tx.update_closure_cleanup(&receipt))
        .unwrap();
    let reopened = storyhook::store::SqliteStore::open(f.env().store_path()).unwrap();
    assert_eq!(
        reopened
            .read(|tx| tx.closure_cleanup(f.project(), StoryNo::new(1)))
            .unwrap(),
        Some(receipt.clone())
    );
    let row = reopened
        .read(|tx| tx.story(f.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    reopened
        .write(|tx| tx.put_story(f.project(), &row.snapshot, row.head_seq))
        .unwrap();
    storyhook::store::repair_read_model(&reopened, f.project()).unwrap();
    assert_eq!(
        reopened
            .read(|tx| tx.closure_cleanup(f.project(), StoryNo::new(1)))
            .unwrap(),
        Some(receipt)
    );
}

#[test]
fn migration_backfills_closed_stories_once_and_preserves_token_identity() {
    let f = fixture();
    StoryService::new(&f.ctx())
        .set_state("SH-1", "done", None, None, None)
        .unwrap();
    StoryService::new(&f.ctx())
        .create(&NewStoryInput {
            title: "Still open".into(),
            ..Default::default()
        })
        .unwrap();
    let conn = rusqlite::Connection::open(f.env().store_path()).unwrap();
    conn.execute_batch("DROP TABLE closure_cleanups;").unwrap();
    conn.execute_batch(include_str!("../src/store/schema/0053_closure_cleanup.sql"))
        .unwrap();
    let rows = f
        .store()
        .read(|tx| tx.closure_cleanups(f.project()))
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].story, StoryNo::new(1));
    assert!(!rows[0].completed);
    assert!(!rows[0].token.is_empty());
    let first = rows[0].clone();
    f.store().migrate().unwrap();
    assert_eq!(
        f.store()
            .read(|tx| tx.closure_cleanup(f.project(), StoryNo::new(1)))
            .unwrap(),
        Some(first)
    );
}

#[test]
fn a_closed_story_in_a_readable_non_git_root_completes_without_a_warning() {
    let f = fixture();
    f.store()
        .write(|tx| tx.set_checkout_path(f.project(), Some(f.cwd())))
        .unwrap();
    StoryService::new(&f.ctx())
        .set_state("SH-1", "done", None, None, None)
        .unwrap();
    storyhook::daemon::cleanup::tick_closures(f.store(), f.env()).unwrap();
    let receipt = request(&f).unwrap();
    assert_eq!(receipt["completed"], true, "{receipt}");
    assert!(receipt["detail"].is_null(), "{receipt}");
    let row = f
        .store()
        .read(|tx| tx.story(f.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert!(
        row.snapshot.comments.is_empty(),
        "{:?}",
        row.snapshot.comments
    );
}

#[test]
fn an_unobservable_or_damaged_root_retains_its_cleanup_request() {
    use std::os::unix::fs::PermissionsExt;

    for damage in [
        "missing",
        "unreadable",
        "git-dir",
        "git-file",
        "git-link",
        "bare",
        "orphan",
        "orphan-link",
        "registered-remote",
    ] {
        let f = fixture();
        let root = f.cwd().join("root");
        std::fs::create_dir(&root).unwrap();
        match damage {
            "missing" => std::fs::remove_dir(&root).unwrap(),
            "unreadable" => {
                std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o000)).unwrap()
            }
            "git-dir" => std::fs::create_dir(root.join(".git")).unwrap(),
            "git-file" => std::fs::write(root.join(".git"), "gitdir: /no-such-repository").unwrap(),
            "git-link" => std::os::unix::fs::symlink("missing", root.join(".git")).unwrap(),
            "bare" => std::fs::write(root.join("HEAD"), "ref: refs/heads/main\n").unwrap(),
            "orphan" => std::fs::create_dir_all(root.join(".codex/worktrees/SH-1")).unwrap(),
            "orphan-link" => std::os::unix::fs::symlink("missing", root.join(".codex")).unwrap(),
            "registered-remote" => f.link_origin("https://example.com/owner/repository.git"),
            _ => unreachable!(),
        }
        f.store()
            .write(|tx| tx.set_checkout_path(f.project(), Some(&root)))
            .unwrap();
        StoryService::new(&f.ctx())
            .set_state("SH-1", "done", None, None, None)
            .unwrap();
        let observation = storyhook::service::resources::ResourceService::new(&f.ctx())
            .resolve("SH-1", &Default::default());
        let result = storyhook::daemon::cleanup::tick_closures(f.store(), f.env());
        if damage == "unreadable" {
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let refusal = observation.unwrap_err();
        assert!(
            refusal.to_string().contains(&root.display().to_string()),
            "{damage}: {refusal}"
        );
        let receipt = request(&f).unwrap();
        assert_eq!(receipt["completed"], false, "{damage}: {receipt}");
        match result {
            Ok(()) => {
                assert!(
                    receipt["detail"]
                        .as_str()
                        .unwrap()
                        .contains("resource-unverifiable"),
                    "{damage}: {receipt}"
                );
                assert!(receipt["retry_at"].is_string(), "{damage}: {receipt}");
            }
            Err(error) => assert!(
                error.to_string().contains(&root.display().to_string()),
                "{damage}: {error}"
            ),
        }
    }
}

#[test]
fn a_closed_story_without_local_resources_completes_without_a_checkout() {
    let f = fixture();
    f.store()
        .write(|tx| tx.set_checkout_path(f.project(), None))
        .unwrap();
    StoryService::new(&f.ctx())
        .set_state("SH-1", "done", None, None, None)
        .unwrap();
    storyhook::daemon::cleanup::tick_closures(f.store(), f.env()).unwrap();
    let receipt = request(&f).unwrap();
    assert_eq!(receipt["completed"], true);
    assert!(receipt["detail"].is_null());
}

#[test]
fn computed_epic_closure_enqueues_parent_and_child() {
    let f = ServiceFixture::new();
    let ctx = f.ctx();
    storyhook::service::ConfigService::new(&ctx)
        .add_type("epic", None, None)
        .unwrap();
    let stories = StoryService::new(&ctx);
    let parent = stories
        .create(&NewStoryInput {
            title: "Folder".into(),
            story_type: Some("epic".into()),
            ..Default::default()
        })
        .unwrap();
    let child = stories
        .create(&NewStoryInput {
            title: "Child".into(),
            ..Default::default()
        })
        .unwrap();
    storyhook::service::RelationService::new(&ctx)
        .relate(&parent.id, "parent-of", &child.id, false)
        .unwrap();
    stories
        .set_state(&child.id, "done", None, None, None)
        .unwrap();
    let requests = f
        .store()
        .read(|tx| tx.closure_cleanups(f.project()))
        .unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|r| !r.completed));
    f.store()
        .write(|tx| tx.set_checkout_path(f.project(), None))
        .unwrap();
    storyhook::daemon::cleanup::tick_closures(f.store(), f.env()).unwrap();
    assert!(
        f.store()
            .read(|tx| tx.closure_cleanups(f.project()))
            .unwrap()
            .iter()
            .all(|r| r.completed)
    );
    stories.reopen(&child.id).unwrap();
    assert!(
        f.store()
            .read(|tx| tx.closure_cleanups(f.project()))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn state_definition_refold_enqueues_closure_without_a_state_change_event() {
    let f = fixture();
    let ctx = f.ctx();
    let config = storyhook::service::ConfigService::new(&ctx);
    config
        .add_state("reviewed", SuperState::Open, None, None)
        .unwrap();
    StoryService::new(&ctx)
        .set_state("SH-1", "reviewed", None, None, None)
        .unwrap();
    // Imported closure history can outlive a temporary OPEN reclassification.
    f.store()
        .write(|tx| {
            tx.append_events(
                f.project(),
                StoryNo::new(1),
                storyhook::store::ExpectedSeq::Any,
                &[storyhook::domain::StoryEvent::StoryClosedAndArchived {
                    at: f.env().now(),
                    state: "reviewed".into(),
                }],
                &storyhook::domain::provenance::Provenance::unrecorded(),
            )?;
            for rebuilt in storyhook::store::rebuild::rebuild(tx, f.project())? {
                tx.put_story(f.project(), &rebuilt.snapshot.unwrap(), rebuilt.head_seq)?;
            }
            Ok(())
        })
        .unwrap();
    assert!(request(&f).is_none());
    // A catalog replacement refolds existing events under the new definition.
    f.store()
        .write(|tx| {
            let mut states = tx.states(f.project())?;
            states
                .iter_mut()
                .find(|s| s.slug == "reviewed")
                .unwrap()
                .super_state = SuperState::Closed;
            tx.put_states(f.project(), &states)?;
            for rebuilt in storyhook::store::rebuild::rebuild(tx, f.project())? {
                tx.put_story(f.project(), &rebuilt.snapshot.unwrap(), rebuilt.head_seq)?;
            }
            Ok(())
        })
        .unwrap();
    assert!(request(&f).is_some());
}

#[test]
fn cli_move_set_and_close_commit_the_same_cleanup_intent() {
    for args in [
        vec!["move", "SH-1", "done"],
        vec!["set", "SH-1", "--state", "done"],
        vec!["close", "SH-1", "No longer needed"],
    ] {
        let f = fixture();
        let input = args.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let invocation = storyhook::cli::parse_invocation(&input).unwrap();
        storyhook::invoke::dispatch(&f.ctx(), invocation).unwrap();
        assert!(request(&f).is_some(), "{args:?}");
    }
}

#[test]
fn dashboard_move_commits_cleanup_and_the_project_notification() {
    use storyhook::api::{
        http::TrustedHosts,
        rest::{Changed, route},
    };
    use storyhook::daemon::http1::{Header, Method};
    let f = fixture();
    f.store()
        .write(|tx| tx.set_checkout_path(f.project(), Some(f.cwd())))
        .unwrap();
    let headers = [
        ("Host", "localhost"),
        ("X-Storyhook", "1"),
        ("Content-Type", "application/json"),
    ]
    .into_iter()
    .map(|(name, value)| Header::from_bytes(name, value).unwrap())
    .collect::<Vec<_>>();
    let routed = route(
        f.store(),
        f.env(),
        &Method::Post,
        "/api/repos/fixture/story/SH-1/move",
        &headers,
        r#"{"state":"done"}"#,
        &TrustedHosts::default(),
    );
    assert_eq!(routed.reply.status, 200);
    assert_eq!(routed.changed, Some(Changed::Project("fixture".into())));
    assert!(request(&f).is_some());
}

#[test]
fn repairing_a_malformed_snapshot_preserves_the_closed_lifecycle_receipt() {
    let f = fixture();
    StoryService::new(&f.ctx())
        .set_state("SH-1", "done", None, None, None)
        .unwrap();
    let receipt = request(&f).unwrap();
    let row = f
        .store()
        .read(|tx| tx.story(f.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    let conn = rusqlite::Connection::open(f.env().store_path()).unwrap();
    conn.execute(
        "UPDATE stories SET snapshot='{}' WHERE project_id=?1 AND story_no=1",
        [f.project().get()],
    )
    .unwrap();
    let result = f
        .store()
        .write(|tx| tx.put_story(f.project(), &row.snapshot, row.head_seq));
    // Preserve fixture integrity even on the expected regression failure.
    conn.execute(
        "UPDATE stories SET snapshot=?1 WHERE project_id=?2 AND story_no=1",
        rusqlite::params![
            serde_json::to_string(&row.snapshot).unwrap(),
            f.project().get()
        ],
    )
    .unwrap();
    result.unwrap();
    assert_eq!(request(&f).unwrap(), receipt);
}
