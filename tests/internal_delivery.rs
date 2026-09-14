//! Managed replacement revokes only unattempted effects in the selected story.
use storyhook::cli::{parse_invocation, split_global_flags};
use storyhook::invoke::dispatch;
use storyhook::output::Response;
use storyhook::service::{Ctx, NewStoryInput, StoryService};
use storyhook::store::{
    BlockAction, DeliveryStatus, ProjectId, ReadOps, SqliteStore, Store, StoryNo, WriteOps,
};
use storyhook_test_support::ServiceFixture;

fn create(ctx: &Ctx<'_, SqliteStore>, title: &str) -> StoryNo {
    let id = StoryService::new(ctx)
        .create(&NewStoryInput {
            title: title.into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    StoryNo::parse_id("SH", &id).unwrap()
}

fn seed(f: &ServiceFixture, project: ProjectId, story: StoryNo, status: DeliveryStatus) -> i64 {
    f.store()
        .write(|tx| {
            tx.enqueue_block_delivery(project, story, BlockAction::Interrupt)?;
            let mut delivery = tx.block_deliveries(project)?.pop().unwrap();
            if status != DeliveryStatus::Pending {
                delivery.status = status;
                delivery.target = Some("original-session-identity".into());
                delivery.detail = "Existing acknowledgement".into();
                assert!(tx.update_block_delivery(&delivery, DeliveryStatus::Pending)?);
            }
            Ok(delivery.id)
        })
        .unwrap()
}

fn revoke(f: &ServiceFixture, id: &str) -> Result<serde_json::Value, String> {
    let args = [
        "--project",
        "fixture",
        "internal",
        "supersede-block-deliveries",
        id,
        "--json",
    ]
    .map(str::to_owned);
    let (flags, args) = split_global_flags(&args).map_err(|error| error.to_string())?;
    assert_eq!(flags.project.as_deref(), Some("fixture"));
    assert!(flags.json);
    let invocation = parse_invocation(&args).map_err(|error| error.to_string())?;
    let response = dispatch(&f.ctx(), invocation).map_err(|error| error.to_string())?;
    match response {
        Response::RawJson(json) => serde_json::from_str(&json).map_err(|error| error.to_string()),
        other => Err(format!("expected protocol receipt, got {other:?}")),
    }
}

#[test]
fn revocation_is_scoped_idempotent_and_preserves_attempted_identity_and_history() {
    let f = ServiceFixture::new();
    let selected = create(&f.ctx(), "Selected story");
    let neighbor = create(&f.ctx(), "Neighbor story");
    let foreign = f.add_project("foreign", "SH");
    let same_number = create(&f.ctx_for(foreign), "Foreign story");
    assert_eq!(selected, same_number);
    let pending = seed(&f, f.project(), selected, DeliveryStatus::Pending);
    let resume = f
        .store()
        .write(|tx| {
            tx.enqueue_block_delivery(f.project(), selected, BlockAction::Resume)?;
            Ok(tx.block_deliveries(f.project())?.pop().unwrap().id)
        })
        .unwrap();
    let attempting = seed(&f, f.project(), selected, DeliveryStatus::Attempting);
    let delivered = seed(&f, f.project(), selected, DeliveryStatus::Delivered);
    let adjacent = seed(&f, f.project(), neighbor, DeliveryStatus::Pending);
    seed(&f, foreign, same_number, DeliveryStatus::Pending);
    let before = f
        .store()
        .read(|tx| tx.story(f.project(), selected))
        .unwrap();
    let receipt = revoke(&f, "1").expect("the managed replacement protocol must exist");
    assert_eq!(
        receipt,
        serde_json::json!({
            "protocol_version": 1, "project": "fixture", "story_id": "SH-1", "superseded": 2
        })
    );
    let rows = f
        .store()
        .read(|tx| tx.block_deliveries(f.project()))
        .unwrap();
    let row = |id| rows.iter().find(|row| row.id == id).unwrap();
    assert_eq!(row(pending).status, DeliveryStatus::Superseded);
    assert!(!row(pending).detail.is_empty());
    assert_eq!(row(resume).status, DeliveryStatus::Superseded);
    assert_eq!(row(resume).action, BlockAction::Resume);
    assert_eq!(row(attempting).status, DeliveryStatus::Attempting);
    assert_eq!(row(delivered).status, DeliveryStatus::Delivered);
    for id in [attempting, delivered] {
        assert_eq!(row(id).target.as_deref(), Some("original-session-identity"));
        assert_eq!(row(id).detail, "Existing acknowledgement");
    }
    assert_eq!(row(adjacent).status, DeliveryStatus::Pending);
    assert_eq!(
        f.store().read(|tx| tx.block_deliveries(foreign)).unwrap()[0].status,
        DeliveryStatus::Pending
    );
    assert_eq!(
        f.store()
            .read(|tx| tx.story(f.project(), selected))
            .unwrap(),
        before
    );
    assert_eq!(revoke(&f, "SH-1").unwrap()["superseded"], 0);
}

#[test]
fn missing_or_foreign_story_refusal_revokes_nothing() {
    let f = ServiceFixture::new();
    let story = create(&f.ctx(), "Owned story");
    seed(&f, f.project(), story, DeliveryStatus::Pending);
    for id in ["SH-99999", "OTHER-1", "0", "SH-01"] {
        assert!(revoke(&f, id).is_err(), "refuse {id}");
    }
    assert_eq!(
        f.store()
            .read(|tx| tx.block_deliveries(f.project()))
            .unwrap()[0]
            .status,
        DeliveryStatus::Pending
    );
}

#[test]
fn internal_protocol_refuses_ambiguous_or_incomplete_requests() {
    for request in [
        "internal",
        "internal supersede-block-deliveries",
        "internal supersede-block-deliveries SH-1 SH-2",
        "internal supersede-block-deliveries --force",
        "internal unknown SH-1",
    ] {
        let args: Vec<_> = request.split_whitespace().map(str::to_owned).collect();
        assert!(parse_invocation(&args).is_err(), "refuse {request}");
    }
}
