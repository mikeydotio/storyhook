//! Attempting helpers pin their original workspace identity at the final commit boundary.
use crate::service::{Ctx, NewStoryInput, StoryService};
use crate::store::{
    BlockAction, BlockDelivery, DeliveryStatus, ReadOps, SqliteStore, Store, StoreError, StoryNo,
    WriteOps,
};
use storyhook_test_support::ServiceFixture;

fn fixture() -> (ServiceFixture, SqliteStore, BlockDelivery) {
    let fixture = ServiceFixture::new();
    // Reopen through this crate instance; the external fixture only seeds its catalog.
    let store = SqliteStore::open(fixture.env().store_path()).unwrap();
    let project = store
        .read(|tx| Ok(tx.project_by_slug("fixture")?.unwrap().id))
        .unwrap();
    let env = crate::env::Environment::at(fixture.env().home());
    let ctx = Ctx::new(&store, project, fixture.cwd(), env);
    for title in ["Pinned helper", "Separate story"] {
        StoryService::new(&ctx)
            .create(&NewStoryInput {
                title: title.into(),
                ..Default::default()
            })
            .unwrap();
    }
    store
        .write(|tx| tx.enqueue_block_delivery(project, StoryNo::new(1), BlockAction::Interrupt))
        .unwrap();
    let mut delivery = store
        .read(|tx| Ok(tx.block_deliveries(project)?.remove(0)))
        .unwrap();
    delivery.status = DeliveryStatus::Attempting;
    delivery.target = Some("acknowledged-session".into());
    assert!(
        store
            .write(|tx| tx.update_block_delivery(&delivery, DeliveryStatus::Pending))
            .unwrap()
    );
    (fixture, store, delivery)
}

fn assert_refused(result: Result<(), StoreError>, operation: &str) {
    let error = result.expect_err(operation);
    assert!(
        matches!(&error, StoreError::Invariant(_)),
        "{operation}: {error}"
    );
    assert!(
        error.to_string().contains("block delivery"),
        "{operation}: {error}"
    );
}

#[test]
fn attempting_delivery_rejects_raw_identity_imports_and_deletes_at_commit() {
    for sql in [
        "UPDATE projects SET uuid='replacement-uuid' WHERE id=?1",
        "UPDATE projects SET slug='replacement-slug' WHERE id=?1",
        "UPDATE projects SET prefix='OTHER' WHERE id=?1",
        "UPDATE projects SET checkout_path='/replacement' WHERE id=?1",
        "UPDATE stories SET created_at='2030-01-01T00:00:00Z' WHERE project_id=?1 AND story_no=1",
        "UPDATE block_deliveries SET story_no=2 WHERE project_id=?1",
        "UPDATE block_deliveries SET id=id+20 WHERE project_id=?1",
        "UPDATE block_deliveries SET status='uncertain' WHERE project_id=?1",
        "DELETE FROM block_deliveries WHERE project_id=?1",
        "DELETE FROM stories WHERE project_id=?1 AND story_no=1",
        "DELETE FROM projects WHERE id=?1",
    ] {
        let (_fixture, store, delivery) = fixture();
        let original = store
            .read(|tx| {
                Ok((
                    tx.project(delivery.project)?,
                    tx.checkout_path(delivery.project)?,
                ))
            })
            .unwrap();
        let result = store.write(|tx| {
            tx.conn.execute(sql, [delivery.project.get()])?;
            Ok(())
        });
        assert_refused(result, sql);
        assert_eq!(
            store
                .read(|tx| tx.block_deliveries(delivery.project))
                .unwrap(),
            [delivery]
        );
        assert_eq!(
            store
                .read(|tx| Ok((
                    tx.project(original.0.as_ref().unwrap().id)?,
                    tx.checkout_path(original.0.as_ref().unwrap().id)?
                )))
                .unwrap(),
            original
        );
    }
}

#[test]
fn attempting_delivery_rejects_public_purge_and_project_delete() {
    let (_fixture, store, delivery) = fixture();
    assert_refused(
        store.write(|tx| tx.purge_story(delivery.project, delivery.story).map(|_| ())),
        "purge story",
    );
    assert_refused(
        store.write(|tx| tx.delete_project(delivery.project).map(|_| ())),
        "delete project",
    );
    assert_eq!(
        store
            .read(|tx| tx.block_deliveries(delivery.project))
            .unwrap(),
        [delivery]
    );
}

#[test]
fn explicit_terminal_acknowledgement_releases_only_its_delivery() {
    for status in [
        DeliveryStatus::Delivered,
        DeliveryStatus::Unreached,
        DeliveryStatus::Uncertain,
        DeliveryStatus::Superseded,
    ] {
        let (_fixture, store, mut delivery) = fixture();
        delivery.status = status;
        store.write(|tx| {
            assert!(tx.update_block_delivery(&delivery, DeliveryStatus::Attempting)?);
            tx.conn.execute("UPDATE projects SET uuid='released-uuid', slug='released-slug', checkout_path='/released' WHERE id=?1", [delivery.project.get()])?;
            tx.purge_story(delivery.project, delivery.story)?;
            Ok(())
        }).unwrap();
        assert!(
            store
                .read(|tx| tx.story(delivery.project, delivery.story))
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn failed_cas_and_nonterminal_updates_do_not_release_the_pin() {
    for (expected, status) in [
        (DeliveryStatus::Pending, DeliveryStatus::Delivered),
        (DeliveryStatus::Attempting, DeliveryStatus::Attempting),
        (DeliveryStatus::Attempting, DeliveryStatus::Pending),
    ] {
        let (_fixture, store, mut delivery) = fixture();
        let original = delivery.clone();
        delivery.status = status;
        assert_refused(
            store.write(|tx| {
                assert_eq!(
                    tx.update_block_delivery(&delivery, expected)?,
                    expected == DeliveryStatus::Attempting
                );
                tx.conn.execute(
                    "UPDATE projects SET uuid='replacement' WHERE id=?1",
                    [delivery.project.get()],
                )?;
                Ok(())
            }),
            "unacknowledged identity transfer",
        );
        assert_eq!(
            store
                .read(|tx| tx.block_deliveries(delivery.project))
                .unwrap(),
            [original]
        );
    }
}

#[test]
fn acknowledging_one_attempt_does_not_release_a_second_attempt() {
    let (_fixture, store, mut delivery) = fixture();
    store
        .write(|tx| {
            tx.enqueue_block_delivery(delivery.project, delivery.story, BlockAction::Resume)
        })
        .unwrap();
    let mut second = store
        .read(|tx| Ok(tx.block_deliveries(delivery.project)?.remove(1)))
        .unwrap();
    second.status = DeliveryStatus::Attempting;
    assert!(
        store
            .write(|tx| tx.update_block_delivery(&second, DeliveryStatus::Pending))
            .unwrap()
    );
    delivery.status = DeliveryStatus::Delivered;
    assert!(
        store
            .write(|tx| tx.update_block_delivery(&delivery, DeliveryStatus::Attempting))
            .unwrap()
    );
    assert_refused(
        store.write(|tx| {
            tx.conn.execute(
                "UPDATE projects SET uuid='replacement' WHERE id=?1",
                [delivery.project.get()],
            )?;
            Ok(())
        }),
        "another attempt still owns the identity",
    );
    second.status = DeliveryStatus::Uncertain;
    assert!(
        store
            .write(|tx| tx.update_block_delivery(&second, DeliveryStatus::Attempting))
            .unwrap()
    );
    store
        .write(|tx| {
            tx.conn.execute(
                "UPDATE projects SET uuid='released' WHERE id=?1",
                [delivery.project.get()],
            )?;
            Ok(())
        })
        .unwrap();
}

#[test]
fn attempting_delivery_allows_diagnostics_and_ordinary_story_comments() {
    let (fixture, store, mut delivery) = fixture();
    delivery.detail = "helper still owns its workspace".into();
    store
        .write(|tx| {
            assert!(tx.update_block_delivery(&delivery, DeliveryStatus::Attempting)?);
            tx.rename_project(delivery.project, "New display name")
        })
        .unwrap();
    let env = crate::env::Environment::at(fixture.env().home());
    let ctx = Ctx::new(&store, delivery.project, fixture.cwd(), env);
    StoryService::new(&ctx)
        .comment("SH-1", "Wait for the active helper to finish")
        .unwrap();
    assert_eq!(
        store
            .read(|tx| tx.block_deliveries(delivery.project))
            .unwrap(),
        [delivery]
    );
}

#[test]
fn attempting_delivery_rejects_an_imported_replacement_snapshot() {
    let (_fixture, store, delivery) = fixture();
    let original = store
        .read(|tx| Ok(tx.story(delivery.project, delivery.story)?.unwrap()))
        .unwrap();
    let mut imported = original.snapshot.clone();
    imported.created_at = "2030-01-01T00:00:00Z".into();
    assert_refused(
        store.write(|tx| tx.put_story(delivery.project, &imported, original.head_seq)),
        "imported replacement story",
    );
    assert_eq!(
        store
            .read(|tx| Ok(tx.story(delivery.project, delivery.story)?.unwrap()))
            .unwrap(),
        original
    );
}
