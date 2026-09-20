//! Durable recovery identity and evidence must survive replay and restart.
mod store_support;

use serde_json::json;
use store_support::{create_story, new_store, seed_project};
use storyhook::store::{
    GlobalSeq, ProjectId, ProjectRecovery, ProjectRecoveryObservation, ReadOps, SqliteStore, Store,
    StoreError, WriteOps,
};

fn record(project: ProjectId, id: &str) -> ProjectRecovery {
    ProjectRecovery {
        id: id.into(),
        project,
        code: "missing-certification".into(),
        locus: ".storyhook.toml#verify.gate".into(),
        revision: 0,
        active: true,
        state: json!({"version":1,"phase":"assessment-pending"}),
    }
}

#[test]
fn active_fault_identity_is_unique_per_project_and_closed_history_survives() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "first", "AA");
    let other = seed_project(&store, "second", "BB");
    let first = record(project, "first");
    assert!(
        store
            .write(|tx| tx.insert_project_recovery(&first))
            .unwrap()
    );
    assert!(
        !store
            .write(|tx| tx.insert_project_recovery(&record(project, "duplicate")))
            .unwrap()
    );
    assert!(
        store
            .write(|tx| tx.insert_project_recovery(&record(other, "other")))
            .unwrap()
    );
    let mut closed = first.clone();
    closed.active = false;
    closed.revision = 1;
    assert!(
        store
            .write(|tx| tx.update_project_recovery(&closed, 0))
            .unwrap()
    );
    assert!(
        store
            .write(|tx| tx.insert_project_recovery(&record(project, "new-lineage")))
            .unwrap()
    );
    let reopened = SqliteStore::open(store.path()).unwrap();
    assert_eq!(
        reopened.read(|tx| tx.project_recoveries(project)).unwrap(),
        vec![closed, record(project, "new-lineage")]
    );
    assert_eq!(
        reopened.read(|tx| tx.project_recoveries(other)).unwrap(),
        vec![record(other, "other")]
    );
}

#[test]
fn revision_and_identity_guards_refuse_stale_or_cross_project_writes() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "first", "AA");
    let other = seed_project(&store, "second", "BB");
    let first = record(project, "fault");
    store
        .write(|tx| tx.insert_project_recovery(&first))
        .unwrap();
    let mut next = first.clone();
    next.revision = 1;
    next.state = json!({"version":1,"phase":"decision-recorded"});
    assert!(
        store
            .write(|tx| tx.update_project_recovery(&next, 0))
            .unwrap()
    );
    assert!(
        !store
            .write(|tx| tx.update_project_recovery(&next, 0))
            .unwrap()
    );
    let mut wrong = next.clone();
    wrong.revision = 2;
    wrong.project = other;
    assert!(
        !store
            .write(|tx| tx.update_project_recovery(&wrong, 1))
            .unwrap()
    );
    wrong.project = project;
    wrong.locus = "different-gate".into();
    assert!(
        !store
            .write(|tx| tx.update_project_recovery(&wrong, 1))
            .unwrap()
    );
    assert!(
        store
            .write(|tx| tx.update_project_recovery(&next, 1))
            .is_err()
    );
    assert_eq!(
        store.read(|tx| tx.project_recoveries(project)).unwrap(),
        vec![next]
    );
}

#[test]
fn observations_are_immutable_replayable_and_atomic_with_coordination() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "first", "AA");
    let story = create_story(
        &store,
        project,
        "unjudged submission",
        "2026-09-19T00:00:00Z",
    );
    let recovery = record(project, "fault");
    let observation = ProjectRecoveryObservation {
        recovery_id: recovery.id.clone(),
        project,
        story,
        generation: GlobalSeq::new(1),
        attempt_id: "attempt-1".into(),
        observed_at: "2026-09-19T00:00:00Z".into(),
        evidence: json!({"tree":"original","head":"original-head"}),
    };
    let rollback: Result<(), StoreError> = store.write(|tx| {
        tx.insert_project_recovery(&recovery)?;
        tx.insert_project_recovery_observation(&observation)?;
        Err(StoreError::Validation(
            "injected failure before commit".into(),
        ))
    });
    assert!(rollback.is_err());
    assert!(
        store
            .read(|tx| tx.project_recoveries(project))
            .unwrap()
            .is_empty()
    );
    store
        .write(|tx| {
            tx.insert_project_recovery(&recovery)?;
            assert!(tx.insert_project_recovery_observation(&observation)?);
            Ok(())
        })
        .unwrap();
    assert!(
        !store
            .write(|tx| tx.insert_project_recovery_observation(&observation))
            .unwrap()
    );
    let mut conflicting = observation.clone();
    conflicting.evidence["tree"] = "rewritten".into();
    assert!(
        store
            .write(|tx| tx.insert_project_recovery_observation(&conflicting))
            .is_err()
    );
    let reopened = SqliteStore::open(store.path()).unwrap();
    assert_eq!(
        reopened
            .read(|tx| tx.project_recovery_observations(project, "fault"))
            .unwrap(),
        vec![observation]
    );
    let conn = rusqlite::Connection::open(store.path()).unwrap();
    assert!(
        conn.execute("UPDATE project_recovery_observations SET evidence='{}'", [])
            .is_err()
    );
    assert!(
        conn.execute("DELETE FROM project_recovery_observations", [])
            .is_err()
    );
    assert!(
        conn.execute("UPDATE project_recoveries SET locus='replacement'", [])
            .is_err()
    );
    drop(conn);
    store.write(|tx| tx.delete_project(project)).unwrap();
    assert!(
        store
            .read(|tx| tx.project_recoveries(project))
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .read(|tx| tx.project_recovery_observations(project, "fault"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn concurrent_observers_acquire_only_one_fault_identity() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "first", "AA");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let path = store.path().to_owned();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let store = SqliteStore::open(path).unwrap();
                barrier.wait();
                store
                    .write(|tx| {
                        tx.insert_project_recovery(&record(project, &format!("observer-{i}")))
                    })
                    .unwrap()
            })
        })
        .collect();
    assert_eq!(
        handles
            .into_iter()
            .map(|h| usize::from(h.join().unwrap()))
            .sum::<usize>(),
        1
    );
    assert_eq!(
        store
            .read(|tx| tx.project_recoveries(project))
            .unwrap()
            .len(),
        1
    );
}
