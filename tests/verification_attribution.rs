//! Durable causal evidence survives restart without granting certification.
mod store_support;

use store_support::{new_store, seed_project};
use storyhook::service::attribution::{AttributionRecord, FailureCause, FailureComponent};
use storyhook::service::attribution::{
    ContrastPlan, DetectorRelation, DiagnosticProbe, ProbeOutcome, ProbeResult, ProbeSide,
};
use storyhook::store::{
    GateInputs, GateSubmission, GlobalSeq, ProjectId, ReadOps, SqliteStore, Store, WriteOps,
};

fn record(project: ProjectId) -> AttributionRecord {
    AttributionRecord {
        version: 1,
        id: "diagnosis-1".into(),
        revision: 0,
        submission: GateSubmission {
            project,
            story_id: "CA-1".into(),
            generation: Some(GlobalSeq::new(7)),
            submitted_at: Some("2026-10-03T00:00:00Z".into()),
        },
        attempt: "attempt-1".into(),
        inputs: GateInputs::default(),
        created_at: "2026-10-03T00:00:00Z".into(),
        preparation: None,
        components: vec![FailureComponent {
            id: "failure-1".into(),
            check: "fixture".into(),
            signature: "expected retained submission".into(),
            requirement: "keep uncertain work verifying".into(),
            log: "/tmp/gate.log".into(),
            observed_cause: FailureCause::Unknown,
        }],
        plans: vec![],
        probes: vec![],
        assessments: vec![],
        diagnosis_ms: 0,
        held: true,
        retired: None,
    }
}

#[test]
fn attribution_hold_and_budget_survive_reopen_and_stale_writes() {
    let (root, store) = new_store();
    let project = seed_project(&store, "causal", "CA");
    let mut evidence = record(project);
    store.write(|tx| tx.insert_attribution(&evidence)).unwrap();
    let mut stale = evidence.clone();
    evidence.revision = 1;
    evidence.diagnosis_ms = 300_000;
    assert!(
        store
            .write(|tx| tx.update_attribution(&evidence, 0))
            .unwrap()
    );
    stale.revision = 1;
    assert!(!store.write(|tx| tx.update_attribution(&stale, 0)).unwrap());
    drop(store);
    let reopened = SqliteStore::open(root.path().join("store.db")).unwrap();
    assert_eq!(
        reopened.read(|tx| tx.attributions(project)).unwrap(),
        vec![evidence.clone()]
    );
    evidence.revision = 2;
    evidence.diagnosis_ms = 0;
    assert!(
        reopened
            .write(|tx| tx.update_attribution(&evidence, 1))
            .is_err(),
        "restart cannot refund consumed allowance"
    );
    assert!(
        reopened
            .read(|tx| tx.gate_attempts(project))
            .unwrap()
            .is_empty(),
        "diagnosis cannot issue a gate verdict"
    );
}

#[test]
fn immutable_submission_and_original_failures_cannot_be_rewritten() {
    let (_root, store) = new_store();
    let project = seed_project(&store, "causal", "CA");
    let evidence = record(project);
    store.write(|tx| tx.insert_attribution(&evidence)).unwrap();
    let mutations: &[fn(&mut AttributionRecord)] = &[
        |r| r.submission.generation = Some(GlobalSeq::new(8)),
        |r| r.attempt = "other-attempt".into(),
        |r| r.components.clear(),
        |r| r.components[0].signature = "new-signature".into(),
        |r| r.inputs.tree = Some("new-tree".into()),
        |r| r.components[0].observed_cause = FailureCause::CandidateCaused,
    ];
    for mutate in mutations {
        let mut changed = evidence.clone();
        changed.revision = 1;
        mutate(&mut changed);
        assert!(
            store
                .write(|tx| tx.update_attribution(&changed, 0))
                .is_err()
        );
        assert_eq!(
            store.read(|tx| tx.attributions(project)).unwrap(),
            vec![evidence.clone()]
        );
    }
}

#[test]
fn retirement_is_terminal_and_new_submissions_retain_the_old_evidence() {
    let (_root, store) = new_store();
    let project = seed_project(&store, "causal", "CA");
    let mut evidence = record(project);
    store.write(|tx| tx.insert_attribution(&evidence)).unwrap();
    evidence.revision = 1;
    evidence.held = false;
    evidence.retired = Some("superseded by generation 8".into());
    assert!(
        store
            .write(|tx| tx.update_attribution(&evidence, 0))
            .unwrap()
    );
    let mut reopened = evidence.clone();
    reopened.revision = 2;
    reopened.held = true;
    reopened.retired = None;
    assert!(
        store
            .write(|tx| tx.update_attribution(&reopened, 1))
            .is_err()
    );
    let mut replacement = record(project);
    replacement.id = "diagnosis-2".into();
    replacement.attempt = "attempt-2".into();
    replacement.submission.generation = Some(GlobalSeq::new(8));
    store
        .write(|tx| tx.insert_attribution(&replacement))
        .unwrap();
    assert_eq!(
        store.read(|tx| tx.attributions(project)).unwrap(),
        vec![evidence, replacement]
    );
}

#[test]
fn malformed_and_forged_causal_rows_fail_loudly() {
    let (_root, store) = new_store();
    let project = seed_project(&store, "causal", "CA");
    let mut evidence = record(project);
    evidence.components[0].observed_cause = FailureCause::CandidateCaused;
    assert!(store.write(|tx| tx.insert_attribution(&evidence)).is_err());
    evidence.components[0].observed_cause = FailureCause::Unknown;
    store.write(|tx| tx.insert_attribution(&evidence)).unwrap();
    let conn = rusqlite::Connection::open(store.path()).unwrap();
    conn.execute("UPDATE verification_attributions SET payload=json_set(payload, '$.created_at', 'not a timestamp')", []).unwrap();
    assert!(
        store
            .read(|tx| tx.attributions(project))
            .unwrap_err()
            .to_string()
            .contains("invalid attribution")
    );
}

fn plan() -> ContrastPlan {
    ContrastPlan {
        component: "failure-1".into(),
        candidate_tree: "a".repeat(40),
        base: "b".repeat(40),
        control_tree: "c".repeat(40),
        detector: "retained-detector".into(),
        relation: DetectorRelation::Unchanged,
        argv: vec!["runner".into(), "--exact".into(), "fixture".into()],
    }
}

fn reserve(record: &mut AttributionRecord, index: usize) {
    if record.plans.is_empty() {
        record.plans.push(plan());
    }
    record.probes.push(DiagnosticProbe {
        id: format!("{}-probe-{index}", record.id),
        plan: 0,
        side: ProbeSide::Candidate,
        started_at: record.created_at.clone(),
        completed: None,
    });
    record.revision += 1;
}

fn complete(record: &mut AttributionRecord, milliseconds: u64) {
    let probe = record.probes.last_mut().unwrap();
    probe.completed = Some(ProbeResult {
        tree: "a".repeat(40),
        detector: "retained-detector".into(),
        executions: 1,
        outcome: ProbeOutcome::Passed,
        environment: None,
        log: format!("/tmp/{}.log", probe.id),
        execution_id: probe.id.clone(),
        cleanup_complete: true,
        milliseconds,
    });
    record.diagnosis_ms += milliseconds;
    record.revision += 1;
}

fn save(store: &SqliteStore, record: &AttributionRecord) {
    assert!(
        store
            .write(|tx| tx.update_attribution(record, record.revision - 1))
            .unwrap()
    );
}

#[test]
fn physical_starts_must_be_reserved_and_completed_evidence_cannot_change() {
    let (_root, store) = new_store();
    let project = seed_project(&store, "causal", "CA");
    let mut evidence = record(project);
    store.write(|tx| tx.insert_attribution(&evidence)).unwrap();
    reserve(&mut evidence, 0);
    let mut unreserved = evidence.clone();
    complete(&mut unreserved, 1);
    unreserved.revision = 1;
    assert!(
        store
            .write(|tx| tx.update_attribution(&unreserved, 0))
            .is_err(),
        "completed evidence cannot invent a physical start"
    );
    save(&store, &evidence);
    complete(&mut evidence, 1);
    save(&store, &evidence);
    let mutations: &[fn(&mut AttributionRecord)] = &[
        |r| r.probes[0].completed = None,
        |r| {
            r.probes[0].completed.as_mut().unwrap().outcome = ProbeOutcome::Failed {
                signature: "new failure".into(),
            }
        },
        |r| r.probes[0].side = ProbeSide::Control,
        |r| r.plans[0].detector = "weaker detector".into(),
    ];
    for mutate in mutations {
        let mut changed = evidence.clone();
        changed.revision += 1;
        mutate(&mut changed);
        assert!(
            store
                .write(|tx| tx.update_attribution(&changed, evidence.revision))
                .is_err()
        );
    }
}

#[test]
fn diagnosis_allowance_is_shared_across_attempts_of_one_submission() {
    for time_limit in [false, true] {
        let (_root, store) = new_store();
        let project = seed_project(&store, "causal", "CA");
        let mut first = record(project);
        store.write(|tx| tx.insert_attribution(&first)).unwrap();
        for index in 0..if time_limit { 1 } else { 8 } {
            reserve(&mut first, index);
            save(&store, &first);
            complete(&mut first, if time_limit { 300_000 } else { 1 });
            save(&store, &first);
        }
        let mut retry = record(project);
        retry.id = "diagnosis-retry".into();
        retry.attempt = "attempt-retry".into();
        store.write(|tx| tx.insert_attribution(&retry)).unwrap();
        reserve(&mut retry, 0);
        assert!(
            store.write(|tx| tx.update_attribution(&retry, 0)).is_err(),
            "new attempt cannot reset the submission allowance"
        );
        retry = record(project);
        retry.id = "diagnosis-new-submission".into();
        retry.submission.generation = Some(GlobalSeq::new(8));
        store.write(|tx| tx.insert_attribution(&retry)).unwrap();
        reserve(&mut retry, 0);
        save(&store, &retry);
    }
}

#[test]
fn interrupted_reservations_prevent_another_attempt_from_spending_the_allowance() {
    let (_root, store) = new_store();
    let project = seed_project(&store, "causal", "CA");
    let mut first = record(project);
    store.write(|tx| tx.insert_attribution(&first)).unwrap();
    reserve(&mut first, 0);
    save(&store, &first);
    let mut retry = record(project);
    retry.id = "retry".into();
    retry.attempt = "retry".into();
    store.write(|tx| tx.insert_attribution(&retry)).unwrap();
    reserve(&mut retry, 0);
    assert!(store.write(|tx| tx.update_attribution(&retry, 0)).is_err());
}

#[test]
fn prefix_rename_cannot_refund_starts_time_or_unsettled_execution() {
    use storyhook::service::ProjectService;
    for limit in ["starts", "time", "unsettled"] {
        let (root, store) = new_store();
        let project = seed_project(&store, "causal", "CA");
        let other = seed_project(&store, "other", "OT");
        let mut first = record(project);
        store.write(|tx| tx.insert_attribution(&first)).unwrap();
        for index in 0..if limit == "starts" { 8 } else { 1 } {
            reserve(&mut first, index);
            save(&store, &first);
            if limit != "unsettled" {
                complete(&mut first, if limit == "time" { 300_000 } else { 1 });
                save(&store, &first);
            }
        }
        ProjectService::new(&store, root.path())
            .set_prefix(project, "NW", &root.path().join("backups"))
            .unwrap();
        drop(store);
        let store = SqliteStore::open(root.path().join("store.db")).unwrap();
        assert_eq!(store.read(|tx| tx.attributions(project)).unwrap(), [first]);
        for (name, owner, id, generation, refused) in [
            ("same", project, "NW-1", 7, true),
            ("other-story", project, "NW-2", 7, false),
            ("other-project", other, "OT-1", 7, false),
            ("new-submission", project, "NW-1", 8, false),
        ] {
            let mut retry = record(owner);
            retry.id = name.into();
            retry.submission.story_id = id.into();
            retry.submission.generation = Some(GlobalSeq::new(generation));
            store.write(|tx| tx.insert_attribution(&retry)).unwrap();
            reserve(&mut retry, 0);
            let result = store.write(|tx| tx.update_attribution(&retry, 0));
            assert_eq!(result.is_err(), refused, "{limit}: {name}: {result:?}");
        }
    }
}

#[test]
fn assessments_must_describe_evidence_already_complete_at_the_named_revision() {
    use storyhook::service::attribution::AttributionAssessment;
    let (_root, store) = new_store();
    let project = seed_project(&store, "causal", "CA");
    let mut evidence = record(project);
    store.write(|tx| tx.insert_attribution(&evidence)).unwrap();
    reserve(&mut evidence, 0);
    save(&store, &evidence);
    let mut forged = evidence.clone();
    complete(&mut forged, 1);
    forged.assessments.push(AttributionAssessment {
        component: "failure-1".into(),
        evidence_revision: evidence.revision,
        cause: FailureCause::Unknown,
        probes: vec![evidence.probes[0].id.clone()],
        detail: "The comparison is incomplete".into(),
    });
    assert!(
        store
            .write(|tx| tx.update_attribution(&forged, evidence.revision))
            .is_err(),
        "a completion in this update did not exist at the named revision"
    );
    complete(&mut evidence, 1);
    save(&store, &evidence);
    forged = evidence.clone();
    forged.revision += 1;
    forged.assessments.push(AttributionAssessment {
        component: "failure-1".into(),
        evidence_revision: 0,
        cause: FailureCause::Unknown,
        probes: vec![evidence.probes[0].id.clone()],
        detail: "The comparison is incomplete".into(),
    });
    assert!(
        store
            .write(|tx| tx.update_attribution(&forged, evidence.revision))
            .is_err(),
        "a completed probe cannot be backdated to revision zero"
    );
    forged.assessments[0].evidence_revision = evidence.revision;
    forged.assessments[0].probes.clear();
    assert!(
        store
            .write(|tx| tx.update_attribution(&forged, evidence.revision))
            .is_err(),
        "an assessment cannot omit the component's prior probes"
    );
    forged.assessments[0]
        .probes
        .push(evidence.probes[0].id.clone());
    save(&store, &forged);
}

#[test]
fn reservation_cannot_admit_using_an_allowance_consumed_in_the_same_update() {
    let (_root, store) = new_store();
    let project = seed_project(&store, "causal", "CA");
    let mut evidence = record(project);
    store.write(|tx| tx.insert_attribution(&evidence)).unwrap();
    reserve(&mut evidence, 0);
    evidence.diagnosis_ms = 300_000;
    assert!(
        store
            .write(|tx| tx.update_attribution(&evidence, 0))
            .is_err(),
        "settle consumed time before making a new launch reservation"
    );
}

#[test]
fn verifier_control_epoch_survives_stop_start_and_rollback() {
    let (_root, store) = new_store();
    let project = seed_project(&store, "causal", "CA");
    assert_eq!(
        store
            .read(|tx| tx.verification_control_revision(project))
            .unwrap(),
        0
    );
    for (index, enabled) in [false, true, true].into_iter().enumerate() {
        store
            .write(|tx| tx.put_verification_enabled(project, enabled))
            .unwrap();
        assert_eq!(
            store
                .read(|tx| tx.verification_control_revision(project))
                .unwrap(),
            index as i64 + 1
        );
    }
    drop(store);
    let store = SqliteStore::open(_root.path().join("store.db")).unwrap();
    assert_eq!(
        store
            .read(|tx| tx.verification_control_revision(project))
            .unwrap(),
        3
    );
    assert!(store.read(|tx| tx.verification_enabled(project)).unwrap());
    let rolled_back: Result<(), storyhook::store::StoreError> = store.write(|tx| {
        tx.put_verification_enabled(project, false)?;
        Err(storyhook::store::StoreError::Validation(
            "simulate transaction rollback".into(),
        ))
    });
    assert!(rolled_back.is_err());
    assert!(store.read(|tx| tx.verification_enabled(project)).unwrap());
    assert_eq!(
        store
            .read(|tx| tx.verification_control_revision(project))
            .unwrap(),
        3
    );
}

#[test]
fn control_epoch_migration_preserves_permission_and_overflow_refuses_the_whole_write() {
    let root = storyhook_test_support::scratch_dir();
    let store = SqliteStore::open(root.path().join("store.db")).unwrap();
    store
        .migrate_with(&storyhook::store::migrate::MIGRATIONS[..55])
        .unwrap();
    let project = seed_project(&store, "causal", "CA");
    let conn = rusqlite::Connection::open(store.path()).unwrap();
    conn.execute(
        "INSERT INTO verification_control(project_id,enabled) VALUES (?1,0)",
        [project.get()],
    )
    .unwrap();
    assert_eq!(
        store
            .read(|tx| tx.verification_control_revision(project))
            .unwrap(),
        0
    );
    store.migrate().unwrap();
    assert!(!store.read(|tx| tx.verification_enabled(project)).unwrap());
    assert_eq!(
        store
            .read(|tx| tx.verification_control_revision(project))
            .unwrap(),
        0
    );
    assert!(
        conn.execute("UPDATE verification_control SET revision=-1", [])
            .is_err()
    );
    conn.execute("UPDATE verification_control SET revision=?1", [i64::MAX])
        .unwrap();
    assert!(
        store
            .write(|tx| tx.put_verification_enabled(project, true))
            .is_err()
    );
    assert!(!store.read(|tx| tx.verification_enabled(project)).unwrap());
    assert_eq!(
        store
            .read(|tx| tx.verification_control_revision(project))
            .unwrap(),
        i64::MAX
    );
}
