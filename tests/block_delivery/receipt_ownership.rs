//! SH-823: receipts belong to persisted delivery owners, never to diagnostic prose.
use super::{deliveries, initialize_checkout, story};
use std::path::{Path, PathBuf};
use storyhook::daemon::block_delivery::{process_one, recover};
use storyhook::service::{NewStoryInput, RelationService, StoryService};
use storyhook::store::{
    BlockAction, BlockDelivery, DeliveryStatus, ProjectId, ReadOps, Store, StoryNo, StoryRow,
    WriteOps,
};
use storyhook_test_support::ServiceFixture;

fn row(f: &ServiceFixture, project: ProjectId, number: i64) -> StoryRow {
    f.store()
        .read(|tx| tx.story(project, StoryNo::new(number)))
        .unwrap()
        .unwrap()
}

fn active(f: &ServiceFixture, project: ProjectId, title: &str) -> String {
    let ctx = f.ctx_for(project);
    let service = StoryService::new(&ctx);
    let id = service
        .create(&NewStoryInput {
            title: title.into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    service
        .set_state(&id, "in-progress", None, None, None)
        .unwrap();
    id
}

fn helper(f: &ServiceFixture, reply: serde_json::Value) -> PathBuf {
    let script = f.cwd().join("receipt-helper.sh");
    std::fs::write(&script, format!("cat <<'REPLY'\n{reply}\nREPLY\n")).unwrap();
    script
}

fn assert_receipt(f: &ServiceFixture, delivery: &BlockDelivery, owner: &str) {
    let stored = row(f, delivery.project, delivery.story.get());
    let marker = format!("AGENT BLOCK DELIVERY #{} —", delivery.id);
    let receipts: Vec<_> = stored
        .snapshot
        .comments
        .iter()
        .filter(|comment| comment.text.starts_with(&marker))
        .collect();
    assert_eq!(receipts.len(), 1, "one receipt for {delivery:?}");
    assert_eq!(
        receipts[0].text,
        format!(
            "{marker} {} {} — {owner}\n\n{}",
            delivery.action.as_str(),
            delivery.status.as_str(),
            storyhook::text_lint::quote_evidence(&delivery.detail)
        )
    );
}

fn assert_unchanged(before: &StoryRow, after: &StoryRow) {
    assert_eq!(before.head_seq, after.head_seq);
    assert_eq!(before.head_global_seq, after.head_global_seq);
    assert_eq!(before.snapshot, after.snapshot);
}

#[test]
fn receipt_headings_name_the_owner_for_each_helper_outcome_and_action() {
    for (ok, reason, status) in [
        (true, "", DeliveryStatus::Delivered),
        (false, "pane-unavailable", DeliveryStatus::Unreached),
        (false, "interruption-failed", DeliveryStatus::Uncertain),
    ] {
        let f = ServiceFixture::new();
        initialize_checkout(&f);
        let id = active(&f, f.project(), "Receipt owner");
        active(&f, f.project(), "Unrelated active story");
        story(&f, "Unrelated inactive story");
        let others = [row(&f, f.project(), 2), row(&f, f.project(), 3)];
        // A diagnostic may mention a different story; it cannot choose the owner.
        let script = helper(
            &f,
            serde_json::json!({"ok":ok,"reason":reason,"target":"session",
                "display":"Evidence mentions SH-768.\nKeep the original words."}),
        );
        let ctx = f.ctx();
        let service = StoryService::new(&ctx);
        let historical = "AGENT BLOCK DELIVERY #107 — interrupt delivered\n\n> Historical receipt.";
        service.comment(&id, historical).unwrap();
        service.set_awaiting(&id, "temporary hold").unwrap();
        for action in [BlockAction::Interrupt, BlockAction::Resume] {
            if action == BlockAction::Resume {
                service.clear_awaiting(&id).unwrap();
            }
            assert!(process_one(f.store(), f.env(), Some(&script)).unwrap());
            let delivery = deliveries(&f).pop().unwrap();
            assert_eq!(delivery.action, action);
            assert_eq!(delivery.status, status);
            assert_receipt(&f, &delivery, &id);
            let finished = row(&f, f.project(), 1);
            assert_eq!(finished.snapshot.comments[0].text, historical);
            assert!(!process_one(f.store(), f.env(), Some(&script)).unwrap());
            assert_unchanged(&finished, &row(&f, f.project(), 1));
            for other in &others {
                assert_unchanged(other, &row(&f, f.project(), other.story_no.get()));
            }
        }
    }
}

#[test]
fn queued_receipts_preserve_project_and_story_identity() {
    let f = ServiceFixture::new();
    initialize_checkout(&f);
    let other_project = f.add_project("other", "OTHER");
    f.store()
        .write(|tx| tx.set_checkout_path(other_project, Some(f.cwd())))
        .unwrap();
    // The helper echoes the actual project and story arguments, making a crossed
    // invocation distinguishable from a correctly stored but incorrect receipt.
    let script = f.cwd().join("owners.sh");
    std::fs::write(
        &script,
        r#"printf '{"ok":true,"target":"%s:%s","display":"delivered to %s/%s"}' "$2" "$4" "$2" "$4"
"#,
    )
    .unwrap();
    let mut owners = Vec::new();
    let mut bystanders = Vec::new();
    for (project, slug) in [(f.project(), "fixture"), (other_project, "other")] {
        for number in 1..=2 {
            let id = active(&f, project, "Queued owner");
            StoryService::new(&f.ctx_for(project))
                .set_awaiting(&id, "hold")
                .unwrap();
            owners.push((project, number, slug, id));
        }
        active(&f, project, "Bystander");
        bystanders.push((project, row(&f, project, 3)));
    }
    for (index, (project, number, slug, id)) in owners.iter().enumerate() {
        let untouched: Vec<_> = owners[index + 1..]
            .iter()
            .map(|(p, n, _, _)| (*p, row(&f, *p, *n)))
            .collect();
        assert!(process_one(f.store(), f.env(), Some(&script)).unwrap());
        let delivery = f
            .store()
            .read(|tx| tx.block_deliveries(*project))
            .unwrap()
            .into_iter()
            .find(|d| d.story == StoryNo::new(*number))
            .unwrap();
        assert_eq!(delivery.status, DeliveryStatus::Delivered);
        assert_eq!(delivery.detail, format!("delivered to {slug}/{id}"));
        assert_eq!(delivery.target, Some(format!("{slug}:{id}")));
        assert_receipt(&f, &delivery, id);
        assert_eq!(row(&f, *project, *number).snapshot.comments.len(), 1);
        for (p, before) in untouched.iter().chain(bystanders.iter()) {
            assert_unchanged(before, &row(&f, *p, before.story_no.get()));
        }
    }
    assert!(!process_one(f.store(), f.env(), Some(&script)).unwrap());
    for (project, number, _, _) in owners {
        assert_eq!(row(&f, project, number).snapshot.comments.len(), 1);
    }
}

#[test]
fn recovery_receipts_name_only_the_interrupted_delivery_owner() {
    let f = ServiceFixture::new();
    initialize_checkout(&f);
    let id = active(&f, f.project(), "Interrupted owner");
    active(&f, f.project(), "Unrelated owner");
    let other = row(&f, f.project(), 2);
    StoryService::new(&f.ctx())
        .set_awaiting(&id, "hold")
        .unwrap();
    let mut delivery = deliveries(&f).remove(0);
    delivery.status = DeliveryStatus::Attempting;
    assert!(
        f.store()
            .write(|tx| tx.update_block_delivery(&delivery, DeliveryStatus::Pending))
            .unwrap()
    );
    recover(f.store(), f.env()).unwrap();
    let delivery = deliveries(&f).remove(0);
    assert_eq!(delivery.status, DeliveryStatus::Uncertain);
    assert_receipt(&f, &delivery, &id);
    let finished = row(&f, f.project(), 1);
    recover(f.store(), f.env()).unwrap();
    assert!(!process_one(f.store(), f.env(), Some(Path::new("/unused"))).unwrap());
    assert_unchanged(&finished, &row(&f, f.project(), 1));
    assert_unchanged(&other, &row(&f, f.project(), 2));
}

#[test]
fn superseded_worker_receipts_name_the_inapplicable_delivery_owner() {
    let f = ServiceFixture::new();
    initialize_checkout(&f);
    let id = active(&f, f.project(), "No longer blocked");
    active(&f, f.project(), "Unrelated owner");
    let other = row(&f, f.project(), 2);
    // Seed a stale intent to reach the worker's applicability check. Ordinary
    // service transitions supersede these silently inside their own transaction.
    f.store()
        .write(|tx| tx.enqueue_block_delivery(f.project(), StoryNo::new(1), BlockAction::Interrupt))
        .unwrap();
    assert!(process_one(f.store(), f.env(), Some(Path::new("/must-not-run"))).unwrap());
    let delivery = deliveries(&f).remove(0);
    assert_eq!(delivery.status, DeliveryStatus::Superseded);
    assert_receipt(&f, &delivery, &id);
    let finished = row(&f, f.project(), 1);
    assert!(!process_one(f.store(), f.env(), Some(Path::new("/must-not-run"))).unwrap());
    assert_unchanged(&finished, &row(&f, f.project(), 1));
    assert_unchanged(&other, &row(&f, f.project(), 2));
}

#[test]
fn dependency_fanout_records_distinct_receipts_only_for_affected_stories() {
    let f = ServiceFixture::new();
    initialize_checkout(&f);
    let blocker = story(&f, "Closed blocker");
    let ctx = f.ctx();
    let service = StoryService::new(&ctx);
    service
        .set_state(&blocker, "done", None, None, None)
        .unwrap();
    let dependents = [
        active(&f, f.project(), "First dependent"),
        active(&f, f.project(), "Second dependent"),
    ];
    for id in &dependents {
        RelationService::new(&ctx)
            .relate(id, "blocked-by", &blocker, false)
            .unwrap();
    }
    active(&f, f.project(), "Unrelated active story");
    let unrelated = row(&f, f.project(), 4);
    service.reopen(&blocker).unwrap();
    let blocker_before_delivery = row(&f, f.project(), 1);
    assert_eq!(deliveries(&f).len(), 2);
    let script = helper(
        &f,
        serde_json::json!({"ok":true,"target":"session","display":"interrupted"}),
    );
    for _ in &dependents {
        assert!(process_one(f.store(), f.env(), Some(&script)).unwrap());
    }
    for (delivery, id) in deliveries(&f).iter().zip(&dependents) {
        assert_eq!(delivery.status, DeliveryStatus::Delivered);
        assert_receipt(&f, delivery, id);
        assert_eq!(
            row(&f, f.project(), delivery.story.get())
                .snapshot
                .comments
                .len(),
            1
        );
    }
    assert_unchanged(&unrelated, &row(&f, f.project(), 4));
    assert_unchanged(&blocker_before_delivery, &row(&f, f.project(), 1));
    assert!(!process_one(f.store(), f.env(), Some(&script)).unwrap());
}
