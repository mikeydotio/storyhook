//! Preparation is budgeted before a contrast plan exists and cannot disappear on restart.
mod store_support;

use store_support::{new_store, seed_project};
use storyhook::service::attribution::AttributionRecord;
use storyhook::store::{ProjectId, ReadOps, SqliteStore, Store, WriteOps};

const AT: &str = "2026-10-03T00:00:00Z";

fn record(project: ProjectId, id: &str) -> AttributionRecord {
    serde_json::from_value(serde_json::json!({
        "version":1,"id":id,"revision":0,
        "submission":{"project":project,"story_id":"CA-1","generation":7,"submitted_at":AT},
        "attempt":format!("attempt-{id}"),"inputs":{},"created_at":AT,
        "components":[{"id":"case","check":"case","signature":"assertion","requirement":"retain input","log":"/tmp/original.log","observed_cause":"unknown"}],
        "plans":[],"probes":[],"assessments":[],"diagnosis_ms":0,"held":true,"retired":null
    })).unwrap()
}

fn preparation(
    old: &AttributionRecord,
    completed: bool,
    clean: bool,
    ms: u64,
) -> AttributionRecord {
    let mut value = serde_json::to_value(old).unwrap();
    value["revision"] = (old.revision + 1).into();
    value["diagnosis_ms"] = ms.into();
    value["preparation"] = serde_json::json!({
        "started_at":AT,
        "completed":if completed { serde_json::json!({
            "milliseconds":ms,"log":"/tmp/preparation.log","detail":"control preparation settled",
            "cleanup_complete":clean
        }) } else { serde_json::Value::Null }
    });
    serde_json::from_value(value).unwrap()
}

fn save(store: &SqliteStore, record: &AttributionRecord) {
    assert!(
        store
            .write(|tx| tx.update_attribution(record, record.revision - 1))
            .unwrap()
    );
}

#[test]
fn unfinished_preparation_blocks_later_attempts_after_rename_and_restart() {
    let (root, store) = new_store();
    let project = seed_project(&store, "causal", "CA");
    let first = record(project, "first");
    store.write(|tx| tx.insert_attribution(&first)).unwrap();
    let reserved = preparation(&first, false, false, 0);
    save(&store, &reserved);
    let view = storyhook::service::gate_cost::view::EvidenceView::new(project, "CA-1", vec![])
        .with_attributions(project, vec![reserved.clone()]);
    assert!(view.render().contains("Preparation"));
    assert!(view.render().contains("unsettled"));
    storyhook::service::ProjectService::new(&store, root.path())
        .set_prefix(project, "NW", &root.path().join("backups"))
        .unwrap();
    drop(store);
    let store = SqliteStore::open(root.path().join("store.db")).unwrap();
    assert_eq!(
        store.read(|tx| tx.attributions(project)).unwrap(),
        std::slice::from_ref(&reserved)
    );
    let mut later = record(project, "later");
    later.submission.story_id = "NW-1".into();
    store.write(|tx| tx.insert_attribution(&later)).unwrap();
    let next = preparation(&later, false, false, 0);
    let error = store
        .write(|tx| tx.update_attribution(&next, 0))
        .unwrap_err();
    assert!(error.to_string().contains("unsettled"), "{error}");
    let complete = preparation(&reserved, true, true, 300_000);
    save(&store, &complete);
    let error = store
        .write(|tx| tx.update_attribution(&next, 0))
        .unwrap_err();
    assert!(error.to_string().contains("exhausted"), "{error}");
    assert!(
        store
            .read(|tx| tx.gate_attempts(project))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn preparation_completion_requires_a_prior_reservation_and_is_immutable() {
    let (_root, store) = new_store();
    let project = seed_project(&store, "causal", "CA");
    let first = record(project, "first");
    let mut unreserved = preparation(&first, false, false, 0);
    unreserved.revision = 0;
    assert!(
        store
            .write(|tx| tx.insert_attribution(&unreserved))
            .is_err()
    );
    store.write(|tx| tx.insert_attribution(&first)).unwrap();
    let completed = preparation(&first, true, true, 100);
    assert!(
        store
            .write(|tx| tx.update_attribution(&completed, 0))
            .is_err()
    );
    let reserved = preparation(&first, false, false, 0);
    save(&store, &reserved);
    let mut erased = first.clone();
    erased.revision = 2;
    assert!(store.write(|tx| tx.update_attribution(&erased, 1)).is_err());
    let complete = preparation(&reserved, true, true, 100);
    save(&store, &complete);
    for (field, replacement) in [
        ("preparation", serde_json::Value::Null),
        ("diagnosis_ms", 99.into()),
    ] {
        let mut value = serde_json::to_value(&complete).unwrap();
        value["revision"] = 3.into();
        value[field] = replacement;
        let changed: AttributionRecord = serde_json::from_value(value).unwrap();
        assert!(
            store
                .write(|tx| tx.update_attribution(&changed, 2))
                .is_err()
        );
    }
    let mut changed = preparation(&complete, true, true, 101);
    assert!(
        store
            .write(|tx| tx.update_attribution(&changed, 2))
            .is_err()
    );
    changed = complete.clone();
    changed.revision = 3;
    changed.held = false;
    changed.retired = Some("retain completed preparation".into());
    save(&store, &changed);
}

#[test]
fn unknown_preparation_cleanup_stays_held_but_does_not_charge_another_submission() {
    let (_root, store) = new_store();
    let project = seed_project(&store, "causal", "CA");
    let other = seed_project(&store, "other", "OT");
    let first = record(project, "first");
    store.write(|tx| tx.insert_attribution(&first)).unwrap();
    let reserved = preparation(&first, false, false, 0);
    save(&store, &reserved);
    let completed = preparation(&reserved, true, false, 1);
    save(&store, &completed);
    for (id, owner, story, generation, refused) in [
        ("same", project, "CA-1", 7, true),
        ("next", project, "CA-1", 8, false),
        ("other-story", project, "CA-2", 7, false),
        ("other-project", other, "OT-1", 7, false),
    ] {
        let mut next = record(owner, id);
        next.submission.story_id = story.into();
        next.submission.generation = Some(storyhook::store::GlobalSeq::new(generation));
        store.write(|tx| tx.insert_attribution(&next)).unwrap();
        let reserved = preparation(&next, false, false, 0);
        assert_eq!(
            store
                .write(|tx| tx.update_attribution(&reserved, 0))
                .is_err(),
            refused,
            "{id}"
        );
    }
}

#[test]
fn preparation_cannot_spend_new_time_in_its_start_transaction() {
    let (_root, store) = new_store();
    let project = seed_project(&store, "causal", "CA");
    let first = record(project, "first");
    store.write(|tx| tx.insert_attribution(&first)).unwrap();
    let reserved = preparation(&first, false, false, 300_000);
    assert!(
        store
            .write(|tx| tx.update_attribution(&reserved, 0))
            .is_err()
    );
}

#[test]
fn preparation_and_probes_share_one_unsettled_operation_fence() {
    use storyhook::service::attribution::{
        ContrastPlan, DetectorRelation, DiagnosticProbe, ProbeSide,
    };
    let (_root, store) = new_store();
    let project = seed_project(&store, "causal", "CA");
    let first = record(project, "first");
    store.write(|tx| tx.insert_attribution(&first)).unwrap();
    let reserved = preparation(&first, false, false, 0);
    save(&store, &reserved);
    let probe = |old: &AttributionRecord| {
        let mut next = old.clone();
        next.revision += 1;
        next.plans.push(ContrastPlan {
            component: "case".into(),
            candidate_tree: "a".repeat(40),
            base: "b".repeat(40),
            control_tree: "c".repeat(40),
            detector: "detector".into(),
            relation: DetectorRelation::Unchanged,
            argv: vec!["runner".into()],
        });
        next.probes.push(DiagnosticProbe {
            id: "probe".into(),
            plan: 0,
            side: ProbeSide::Candidate,
            started_at: AT.into(),
            completed: None,
        });
        next
    };
    let denied = probe(&reserved);
    assert!(
        store
            .write(|tx| tx.update_attribution(&denied, reserved.revision))
            .is_err()
    );
    let completed = preparation(&reserved, true, true, 10);
    let mut mixed = probe(&completed);
    mixed.revision = reserved.revision + 1;
    assert!(
        store
            .write(|tx| tx.update_attribution(&mixed, reserved.revision))
            .is_err(),
        "settle preparation before reserving a probe"
    );
    save(&store, &completed);
    let mut execution = probe(&completed);
    save(&store, &execution);
    let next = record(project, "later");
    store.write(|tx| tx.insert_attribution(&next)).unwrap();
    let preparing = preparation(&next, false, false, 0);
    let error = store
        .write(|tx| tx.update_attribution(&preparing, 0))
        .unwrap_err();
    assert!(error.to_string().contains("unsettled"), "{error}");
    execution.probes[0].completed = Some(storyhook::service::attribution::ProbeResult {
        tree: "a".repeat(40),
        detector: "detector".into(),
        executions: 1,
        outcome: storyhook::service::attribution::ProbeOutcome::Passed,
        environment: None,
        log: "/tmp/probe.log".into(),
        execution_id: "physical-probe".into(),
        cleanup_complete: false,
        milliseconds: 5,
    });
    execution.diagnosis_ms += 5;
    execution.revision += 1;
    save(&store, &execution);
    let error = store
        .write(|tx| tx.update_attribution(&preparing, 0))
        .unwrap_err();
    assert!(
        error.to_string().contains("unsettled"),
        "a completed check is not proof of cleanup: {error}"
    );
}
