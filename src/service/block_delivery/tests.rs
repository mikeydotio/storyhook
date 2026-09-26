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

/// A fixture story in a project whose checkout is the fixture's own.
fn fixture_story() -> (
    storyhook_test_support::ServiceFixture,
    SqliteStore,
    ProjectId,
    StoryNo,
) {
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
            title: "Backstop".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    let story = StoryNo::parse_id("SH", &id).unwrap();
    (f, store, project, story)
}

/// Appends `event` to `story` through the service funnel inside `tx`.
fn funnel(
    tx: &mut impl WriteOps,
    project: ProjectId,
    story: StoryNo,
    event: crate::domain::StoryEvent,
) -> Result<crate::domain::StorySnapshot, AppError> {
    let states = tx.state_map(project)?;
    crate::service::append_and_fold(
        tx,
        project,
        story,
        "SH",
        &states,
        crate::store::ExpectedSeq::Any,
        &[event],
        &crate::domain::provenance::Provenance::unrecorded(),
    )
}

fn moved(state: &str) -> crate::domain::StoryEvent {
    crate::domain::StoryEvent::StoryStateChanged {
        at: "2026-09-25T00:00:00Z".into(),
        state: state.into(),
    }
}

/// SH-772's class detector: a block-relevant append that nothing derives
/// delivery edges for fails loudly, names what it was, and writes nothing.
#[test]
fn a_block_relevant_append_outside_derivation_is_refused() {
    let (_f, store, project, story) = fixture_story();
    let error = store
        .write(|tx| {
            funnel(tx, project, story, moved("in-progress"))?;
            Ok(())
        })
        .unwrap_err()
        .to_string();
    assert!(error.contains("SH-1"), "{error}");
    assert!(error.contains("StoryStateChanged"), "{error}");
    assert!(error.contains("outside block-edge derivation"), "{error}");
    let row = store.read(|tx| tx.story(project, story)).unwrap().unwrap();
    assert_eq!(row.snapshot.state, "todo", "the refused write rolled back");
}

#[test]
fn an_inert_append_outside_derivation_is_accepted() {
    let (_f, store, project, story) = fixture_story();
    store
        .write(|tx| {
            funnel(
                tx,
                project,
                story,
                crate::domain::StoryEvent::StoryCommentAdded {
                    at: "2026-09-25T00:00:00Z".into(),
                    text: "a delivery outcome, say".into(),
                },
            )?;
            Ok(())
        })
        .unwrap();
}

#[test]
fn a_block_relevant_append_inside_derivation_is_accepted() {
    let (_f, store, project, story) = fixture_story();
    store
        .write(|tx| {
            derive_block_edges(tx, project, SubmissionGate::NotASubmission, |tx| {
                funnel(tx, project, story, moved("in-progress"))?;
                Ok(())
            })
        })
        .unwrap();
    // Derivation ends with the closure, so a later raw append is refused again.
    assert!(
        store
            .write(|tx| {
                derive_block_edges(tx, project, SubmissionGate::NotASubmission, |_| Ok(()))?;
                funnel(tx, project, story, moved("todo"))?;
                Ok(())
            })
            .is_err()
    );
}

#[test]
fn a_refold_outside_derivation_is_refused() {
    let (_f, store, project, story) = fixture_story();
    let error = store
        .write(|tx| {
            let states = tx.state_map(project)?;
            crate::service::refold_story(tx, project, story, "SH", &states)?;
            Ok(())
        })
        .unwrap_err()
        .to_string();
    assert!(error.contains("a refold"), "{error}");
    store
        .write(|tx| {
            derive_block_edges(tx, project, SubmissionGate::NotASubmission, |tx| {
                let states = tx.state_map(project)?;
                crate::service::refold_story(tx, project, story, "SH", &states)?;
                Ok(())
            })
        })
        .unwrap();
}

#[test]
fn a_nested_derivation_is_refused() {
    let (_f, store, project, _) = fixture_story();
    let error = store
        .write(|tx| {
            derive_block_edges(tx, project, SubmissionGate::NotASubmission, |tx| {
                derive_block_edges(tx, project, SubmissionGate::NotASubmission, |_| Ok(()))
            })
        })
        .unwrap_err()
        .to_string();
    assert!(error.contains("already running"), "{error}");
}
