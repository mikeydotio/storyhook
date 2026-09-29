//! The durable verification batch record (SH-831): its lifecycle, the one live
//! batch per project, compare-and-swap updates, retention and restart
//! survival.
mod store_support;

use store_support::{create_story, new_store, seed_project};
use storyhook::domain::gate_verdict::GateVerdict;
use storyhook::store::{
    BatchBisection, BatchGate, BatchId, BatchMember, BatchPhase, BisectionOf, BisectionOutcome,
    GlobalSeq, ProjectId, ReadOps, SqliteStore, Store, StoreError, StoryNo, VerificationBatch,
    WriteOps,
};

const AT: &str = "2026-01-01T00:00:00Z";

fn oid(digit: char) -> String {
    digit.to_string().repeat(40)
}

fn member(story: StoryNo, prefix: &str, position: u32) -> BatchMember {
    BatchMember {
        merge_commit: None,
        merge_tree: None,
        story,
        story_id: story.to_id(prefix),
        generation: GlobalSeq::new(100 + i64::from(position)),
        head_commit: oid(char::from_digit(position + 1, 10).unwrap()),
        pull_request: format!("https://github.com/acme/widgets/pull/{}", position + 1),
        position,
        branch: None,
    }
}

fn batch(store: &SqliteStore, project: ProjectId, prefix: &str) -> VerificationBatch {
    let first = create_story(store, project, "head", AT);
    let second = create_story(store, project, "member", AT);
    let id = BatchId::generate();
    let members = vec![member(first, prefix, 0), member(second, prefix, 1)];
    VerificationBatch {
        bisects: None,
        bisection: None,
        branch: id.branch(),
        id,
        project,
        project_slug: "first".into(),
        head: members[0].story_id.clone(),
        base_branch: "dev".into(),
        base_commit: oid('a'),
        tip: oid('b'),
        pull_request: None,
        phase: BatchPhase::Assembled,
        members,
        excluded: Vec::new(),
        gate: None,
        detail: None,
        retired: false,
        revision: 0,
        created_at: AT.into(),
        updated_at: AT.into(),
    }
}

fn ended(batch: &VerificationBatch, phase: BatchPhase) -> VerificationBatch {
    let mut next = batch.advance(phase, AT).unwrap();
    next.retired = true;
    next
}

#[test]
fn a_batch_moves_through_its_phases_and_survives_a_reopen() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "first", "AA");
    let assembled = batch(&store, project, "AA");
    store
        .write(|tx| tx.insert_verification_batch(&assembled))
        .unwrap();

    let submitted = assembled.advance(BatchPhase::Submitted, AT).unwrap();
    assert!(
        store
            .write(|tx| tx.update_verification_batch(&submitted, 0))
            .unwrap()
    );
    let gating = submitted.advance(BatchPhase::Gating, AT).unwrap();
    assert!(
        store
            .write(|tx| tx.update_verification_batch(&gating, 1))
            .unwrap()
    );
    let mut released = gating.advance(BatchPhase::Released, AT).unwrap();
    released.gate = Some(BatchGate {
        verdict: GateVerdict::Certified,
        tree: Some(oid('c')),
        detail: "passed".into(),
        seconds: 7,
    });
    assert!(
        store
            .write(|tx| tx.update_verification_batch(&released, 2))
            .unwrap()
    );

    let reopened = SqliteStore::open(store.path()).unwrap();
    assert_eq!(
        reopened
            .read(|tx| tx.verification_batches(project))
            .unwrap(),
        vec![released]
    );
}

#[test]
fn one_live_batch_per_project_and_an_ended_one_does_not_count() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "first", "AA");
    let other = seed_project(&store, "second", "BB");
    let first = batch(&store, project, "AA");
    store
        .write(|tx| tx.insert_verification_batch(&first))
        .unwrap();
    let second = batch(&store, project, "AA");
    assert!(
        store
            .write(|tx| tx.insert_verification_batch(&second))
            .is_err(),
        "a second live batch of one project is refused"
    );
    let elsewhere = batch(&store, other, "BB");
    store
        .write(|tx| tx.insert_verification_batch(&elsewhere))
        .expect("another project's live batch is independent");

    let abandoned = first.advance(BatchPhase::Abandoned, AT).unwrap();
    assert!(
        store
            .write(|tx| tx.update_verification_batch(&abandoned, 0))
            .unwrap()
    );
    store
        .write(|tx| tx.insert_verification_batch(&second))
        .expect("an ended batch frees the project's live slot");
    let ids: Vec<_> = store
        .read(|tx| tx.verification_batches(project))
        .unwrap()
        .into_iter()
        .map(|b| (b.id, b.phase))
        .collect();
    assert_eq!(
        ids,
        vec![
            (first.id.clone(), BatchPhase::Abandoned),
            (second.id.clone(), BatchPhase::Assembled)
        ]
    );
}

#[test]
fn a_stale_revision_or_a_move_out_of_an_end_is_not_written() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "first", "AA");
    let assembled = batch(&store, project, "AA");
    store
        .write(|tx| tx.insert_verification_batch(&assembled))
        .unwrap();
    let mut stale = assembled.advance(BatchPhase::Submitted, AT).unwrap();
    stale.revision = 6;
    assert!(
        !store
            .write(|tx| tx.update_verification_batch(&stale, 5))
            .unwrap(),
        "an expected revision the row no longer has writes nothing"
    );
    let mut skipped = assembled.clone();
    skipped.revision = 2;
    assert!(
        store
            .write(|tx| tx.update_verification_batch(&skipped, 0))
            .is_err(),
        "the revision advances by exactly one"
    );
    let abandoned = assembled.advance(BatchPhase::Abandoned, AT).unwrap();
    assert!(
        store
            .write(|tx| tx.update_verification_batch(&abandoned, 0))
            .unwrap()
    );
    let mut revived = abandoned.clone();
    revived.phase = BatchPhase::Gating;
    revived.revision = 2;
    assert!(
        !store
            .write(|tx| tx.update_verification_batch(&revived, 1))
            .unwrap(),
        "an ended batch never becomes live again"
    );
    let mut retired = abandoned.clone();
    retired.retired = true;
    retired.revision = 2;
    assert!(
        store
            .write(|tx| tx.update_verification_batch(&retired, 1))
            .unwrap(),
        "an ended batch may still record its retirement"
    );
    assert_eq!(
        store.read(|tx| tx.verification_batches(project)).unwrap(),
        vec![retired]
    );
}

#[test]
fn an_invalid_record_is_refused_before_it_is_written() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "first", "AA");
    let valid = batch(&store, project, "AA");
    let mut lone = valid.clone();
    lone.members.truncate(1);
    let mut misplaced = valid.clone();
    misplaced.members[1].position = 5;
    let mut renamed = valid.clone();
    renamed.branch = "main".into();
    let mut short = valid.clone();
    short.tip = "abc".into();
    let mut headless = valid.clone();
    headless.head = "AA-99".into();
    let mut ahead = valid.clone();
    ahead.revision = 1;
    let mut ended_on_insert = valid.clone();
    ended_on_insert.phase = BatchPhase::Released;
    for (why, record) in [
        ("one member", lone),
        ("positions out of order", misplaced),
        ("a branch not named for the id", renamed),
        ("a short object id", short),
        ("a head that is not the first member", headless),
        ("an insert above revision zero", ahead),
        ("an insert that is already ended", ended_on_insert),
    ] {
        let result = store.write(|tx| tx.insert_verification_batch(&record));
        assert!(
            matches!(result, Err(StoreError::Validation(_))),
            "{why}: {result:?}"
        );
    }
    assert!(
        store
            .read(|tx| tx.verification_batches(project))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn retention_prunes_only_retired_ended_batches_beyond_the_newest() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "first", "AA");
    let mut kept = Vec::new();
    for index in 0..5 {
        let record = batch(&store, project, "AA");
        store
            .write(|tx| tx.insert_verification_batch(&record))
            .unwrap();
        let end = if index == 1 {
            // Ended but not yet retired: its pull request may still be open.
            record.advance(BatchPhase::Abandoned, AT).unwrap()
        } else {
            ended(&record, BatchPhase::Released)
        };
        assert!(
            store
                .write(|tx| tx.update_verification_batch(&end, 0))
                .unwrap()
        );
        kept.push(end);
    }
    let live = batch(&store, project, "AA");
    store
        .write(|tx| tx.insert_verification_batch(&live))
        .unwrap();

    let pruned = store
        .write(|tx| tx.prune_verification_batches(project, 2))
        .unwrap();
    assert_eq!(pruned, 2, "retired batches 0 and 2 go; 3 and 4 are newest");
    let remaining: Vec<_> = store
        .read(|tx| tx.verification_batches(project))
        .unwrap()
        .into_iter()
        .map(|b| b.id)
        .collect();
    assert_eq!(
        remaining,
        vec![
            kept[1].id.clone(),
            kept[3].id.clone(),
            kept[4].id.clone(),
            live.id.clone()
        ]
    );
}

#[test]
fn deleting_the_project_deletes_its_batches() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "first", "AA");
    let other = seed_project(&store, "second", "BB");
    let deleted = batch(&store, project, "AA");
    store
        .write(|tx| tx.insert_verification_batch(&deleted))
        .unwrap();
    let kept = batch(&store, other, "BB");
    store
        .write(|tx| tx.insert_verification_batch(&kept))
        .unwrap();
    store.write(|tx| tx.delete_project(project)).unwrap();
    assert!(
        store
            .read(|tx| tx.verification_batches(project))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store.read(|tx| tx.verification_batches(other)).unwrap(),
        vec![kept]
    );
}

#[test]
fn a_payload_that_disagrees_with_its_columns_cannot_be_stored() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "first", "AA");
    let record = batch(&store, project, "AA");
    let payload = serde_json::to_string(&record).unwrap();
    let conn = rusqlite::Connection::open(store.path()).unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
    for (id, revision, live) in [
        ("ffffffffffff", 0, 1),
        (record.id.as_str(), 3, 1),
        (record.id.as_str(), 0, 0),
    ] {
        let result = conn.execute(
            "INSERT INTO verification_batches(id, project_id, revision, live, payload) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, project.get(), revision, live, payload],
        );
        assert!(result.is_err(), "{id} {revision} {live} was stored");
    }
    conn.execute(
        "INSERT INTO verification_batches(id, project_id, revision, live, payload) VALUES (?1, ?2, 0, 1, ?3)",
        rusqlite::params![record.id.as_str(), project.get(), payload],
    )
    .expect("the consistent row is stored");
}

/// A batch record can hold a landing and its end (SH-832), and a landing
/// batch stays live: it still counts as the project's one live batch.
#[test]
fn a_certified_batch_moves_through_landing_to_landed() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "first", "AA");
    let assembled = batch(&store, project, "AA");
    let submitted = assembled.advance(BatchPhase::Submitted, AT).unwrap();
    let gating = submitted.advance(BatchPhase::Gating, AT).unwrap();
    let landing = gating.advance(BatchPhase::Landing, AT).unwrap();
    store
        .write(|tx| {
            tx.insert_verification_batch(&assembled)?;
            assert!(tx.update_verification_batch(&submitted, 0)?);
            assert!(tx.update_verification_batch(&gating, 1)?);
            assert!(tx.update_verification_batch(&landing, 2)?);
            Ok(())
        })
        .unwrap();
    let second = batch(&store, project, "AA");
    assert!(
        store
            .write(|tx| tx.insert_verification_batch(&second))
            .is_err(),
        "a landing batch is still the project's live batch"
    );
    assert!(landing.advance(BatchPhase::Abandoned, AT).is_err());
    let landed = landing.advance(BatchPhase::Landed, AT).unwrap();
    assert!(
        store
            .write(|tx| tx.update_verification_batch(&landed, 3))
            .unwrap()
    );
    let reopened = SqliteStore::open(store.path()).unwrap();
    assert_eq!(
        reopened
            .read(|tx| tx.verification_batches(project))
            .unwrap(),
        vec![landed]
    );
    assert!(
        store
            .write(|tx| tx.insert_verification_batch(&second))
            .is_ok(),
        "a landed batch ends"
    );
}

/// Migration 52 rebuilds the table with the landing phases: every row, its
/// rowid order (listing and retention read it) and the one-live-batch index
/// survive, and the new phases can be stored.
#[test]
fn migration_52_keeps_every_batch_in_order_and_the_live_index() {
    use storyhook::store::MIGRATIONS;
    let dir = storyhook_test_support::scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    let before_52 = MIGRATIONS
        .iter()
        .take_while(|migration| migration.version < 52)
        .copied()
        .collect::<Vec<_>>();
    store.migrate_with(&before_52).unwrap();
    let project = seed_project(&store, "first", "AA");
    let first = batch(&store, project, "AA");
    let second = batch(&store, project, "AA");
    let ended = ended(&first, BatchPhase::Released);
    store
        .write(|tx| {
            tx.insert_verification_batch(&first)?;
            assert!(tx.update_verification_batch(&ended, 0)?);
            tx.insert_verification_batch(&second)?;
            Ok(())
        })
        .unwrap();

    store.migrate().unwrap();

    assert_eq!(
        store.read(|tx| tx.verification_batches(project)).unwrap(),
        vec![ended, second.clone()],
        "rows and their order survive the rebuild"
    );
    let third = batch(&store, project, "AA");
    assert!(
        store
            .write(|tx| tx.insert_verification_batch(&third))
            .is_err(),
        "the one-live-batch index survives the rebuild"
    );
    let landing = second
        .advance(BatchPhase::Submitted, AT)
        .and_then(|next| next.advance(BatchPhase::Gating, AT))
        .and_then(|next| next.advance(BatchPhase::Landing, AT))
        .unwrap();
    let mut path = second;
    for next in [BatchPhase::Submitted, BatchPhase::Gating] {
        let moved = path.advance(next, AT).unwrap();
        assert!(
            store
                .write(|tx| tx.update_verification_batch(&moved, path.revision))
                .unwrap()
        );
        path = moved;
    }
    assert!(
        store
            .write(|tx| tx.update_verification_batch(&landing, path.revision))
            .unwrap(),
        "the landing phase is storable after migration 52"
    );
}

/// A red batch ends released and keeps recording its bisection (SH-833):
/// an ended record may change everything but its phase, and a probe batch
/// of its prefix is then the project's one live batch.
#[test]
fn a_released_batch_records_its_bisection_while_its_probe_is_live() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "first", "AA");
    let first = create_story(&store, project, "head", AT);
    let second = create_story(&store, project, "member", AT);
    let third = create_story(&store, project, "third", AT);
    let id = BatchId::generate();
    let members = vec![
        member(first, "AA", 0),
        member(second, "AA", 1),
        member(third, "AA", 2),
    ];
    let parent = VerificationBatch {
        bisects: None,
        bisection: None,
        branch: id.branch(),
        id,
        project,
        project_slug: "first".into(),
        head: members[0].story_id.clone(),
        base_branch: "dev".into(),
        base_commit: oid('a'),
        tip: oid('b'),
        pull_request: None,
        phase: BatchPhase::Gating,
        members,
        excluded: Vec::new(),
        gate: None,
        detail: None,
        retired: false,
        revision: 0,
        created_at: AT.into(),
        updated_at: AT.into(),
    };
    store
        .write(|tx| tx.insert_verification_batch(&parent))
        .unwrap();
    let mut released = parent.advance(BatchPhase::Released, AT).unwrap();
    released.bisection = Some(BatchBisection::default());
    assert!(
        store
            .write(|tx| tx.update_verification_batch(&released, 0))
            .unwrap()
    );
    assert!(
        released.needs_finalization(),
        "its bisection has no outcome yet"
    );

    let probe_id = BatchId::generate();
    let probe = VerificationBatch {
        bisects: Some(BisectionOf {
            parent: parent.id.clone(),
            prefix: 2,
        }),
        branch: probe_id.branch(),
        id: probe_id,
        members: parent.members[..2].to_vec(),
        tip: oid('c'),
        phase: BatchPhase::Assembled,
        ..parent.clone()
    };
    store
        .write(|tx| tx.insert_verification_batch(&probe))
        .expect("the probe is the project's only live batch");

    let mut finished = released.clone();
    finished.revision += 1;
    finished.bisection = Some(BatchBisection {
        probes: Vec::new(),
        outcome: Some(BisectionOutcome::Inconclusive {
            detail: "fixture".into(),
        }),
    });
    assert!(
        store
            .write(|tx| tx.update_verification_batch(&finished, released.revision))
            .unwrap(),
        "an ended record records its bisection's end in the same phase"
    );
    let stored = store.read(|tx| tx.verification_batches(project)).unwrap();
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[0], finished);
    assert!(!stored[0].needs_finalization());
    assert_eq!(stored[1].bisects.as_ref().unwrap().parent, parent.id);
}
