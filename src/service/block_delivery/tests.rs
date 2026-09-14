//! Pending authority can be revoked without changing started effects or target identity.
use super::*;
use crate::store::{DeliveryStatus, SqliteStore};

#[test]
fn supersession_only_narrows_pending_authority() {
    let f = storyhook_test_support::ServiceFixture::new();
    let store = SqliteStore::open(f.store().path()).unwrap();
    let project = ProjectId::new(f.project().get());
    let ctx = Ctx::new(
        &store,
        project,
        f.cwd(),
        crate::env::Environment::at(f.cwd()),
    )
    .no_hooks(true);
    let id = crate::service::StoryService::new(&ctx)
        .create(&crate::service::NewStoryInput {
            title: "Delivery authority".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    let story = StoryNo::parse_id("SH", &id).unwrap();
    store
        .write(|tx| {
            tx.enqueue_block_delivery(project, story, BlockAction::Interrupt)?;
            let mut attempting = tx.block_deliveries(project)?.remove(0);
            attempting.status = DeliveryStatus::Attempting;
            attempting.target = Some("original identity".into());
            tx.update_block_delivery(&attempting, DeliveryStatus::Pending)?;
            tx.enqueue_block_delivery(project, story, BlockAction::Resume)?;
            assert_eq!(
                supersede_pending(tx, project, story, "session replacement reserved")?,
                1
            );
            assert_eq!(supersede_pending(tx, project, story, "repeat")?, 0);
            let rows = tx.block_deliveries(project)?;
            assert_eq!(rows[0], attempting);
            assert_eq!(rows[1].status, DeliveryStatus::Superseded);
            assert_eq!(rows[1].detail, "session replacement reserved");
            Ok(())
        })
        .unwrap();
}
