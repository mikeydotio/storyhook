//! Native preflight protects cleanup authority while retaining appended observations.
use super::*;
use crate::service::{NewStoryInput, StoryService};
use crate::store::SqliteStore;
use storyhook_test_support::ServiceFixture;

#[test]
fn preflight_accepts_observations_but_refuses_lifecycle_changes() {
    let fixture = ServiceFixture::new();
    let store = SqliteStore::open(fixture.env().store_path()).unwrap();
    let project = store
        .read(|tx| Ok(tx.project_by_slug("fixture")?.unwrap().id))
        .unwrap();
    let env = crate::env::Environment::at(fixture.env().home());
    let ctx = Ctx::new(&store, project, fixture.cwd(), env);
    let stories = StoryService::new(&ctx);
    let story = stories
        .create(&NewStoryInput {
            title: "Reset with concurrent observations".into(),
            ..Default::default()
        })
        .unwrap();
    let number = StoryNo::new(1);
    let original = store
        .read(|tx| Ok(tx.story(project, number)?.unwrap().head_seq))
        .unwrap();
    stories
        .comment(&story.id, "Worker interruption could not reach the session")
        .unwrap();
    store
        .read(|tx| {
            let current = tx.story(project, number)?.unwrap().head_seq;
            assert!(preflight_authority_unchanged(
                tx, project, number, original, current
            )?);
            Ok(())
        })
        .unwrap();
    stories
        .set_awaiting(&story.id, "Wait for human review")
        .unwrap();
    store
        .read(|tx| {
            let current = tx.story(project, number)?.unwrap().head_seq;
            assert!(!preflight_authority_unchanged(
                tx, project, number, original, current
            )?);
            assert!(!preflight_authority_unchanged(
                tx, project, number, current, original
            )?);
            Ok(())
        })
        .unwrap();
}

#[test]
fn preflight_refuses_a_project_retarget_before_the_first_reservation() {
    let fixture = ServiceFixture::new();
    let store = SqliteStore::open(fixture.env().store_path()).unwrap();
    let (original, checkout) = store
        .read(|tx| {
            let project = tx.project_by_slug("fixture")?.unwrap();
            let checkout = tx.checkout_path(project.id)?.unwrap();
            Ok((project, checkout))
        })
        .unwrap();
    assert!(
        store
            .read(|tx| preflight_project_unchanged(tx, &original, &checkout))
            .unwrap()
    );
    store
        .write(|tx| tx.set_checkout_path(original.id, Some(std::path::Path::new("/retargeted"))))
        .unwrap();
    assert!(
        !store
            .read(|tx| preflight_project_unchanged(tx, &original, &checkout))
            .unwrap()
    );
    store
        .write(|tx| {
            tx.set_checkout_path(original.id, Some(&checkout))?;
            tx.set_prefix(original.id, "OTHER")
        })
        .unwrap();
    assert!(
        !store
            .read(|tx| preflight_project_unchanged(tx, &original, &checkout))
            .unwrap()
    );
}
