//! Batch conflict smoothing (SH-834; spec B8, council decision D1 on
//! SH-834): a story whose conflict with a member is insertion-only on paths
//! the base's `[batch] smooth` admits joins the batch last, its merge commit
//! carries the union, and the batch pull request and every GREEN it touches
//! name the resolution; any other conflict never reaches the resolution and
//! keeps the story out.

use super::batch_harness::*;
use super::batch_preview::{Board, git};
use super::*;
use storyhook::service::trial_merge::{ConflictRecord, ConflictShape, StageEntry};
use storyhook::store::{BatchExclusionReason, BatchPhase};

const SPEC: &str = "# Spec\n\n## A\n\ntext\n\n## End\n";
const SMOOTH_DOCS: &str = "\n[batch]\nsmooth = [\"docs/\"]\n";

fn spec_with(section: &str) -> String {
    format!("# Spec\n\n## A\n\ntext\n\n## {section}\n\n{section} body\n\n## End\n")
}

/// A board whose base holds `docs/spec.md`, `src/lib.rs` and a `[batch]`
/// table admitting `docs/`, with the identity batch merge commits need.
fn smoothing_board(stories: &[(&str, &str)], lanes: u32) -> Board {
    let board = Board::with_base(
        stories,
        &[("docs/spec.md", SPEC), ("src/lib.rs", SPEC)],
        Some(SMOOTH_DOCS),
    );
    git(&board.root, &["config", "user.name", "t"]);
    git(&board.root, &["config", "user.email", "t@t"]);
    board.live_run(lanes);
    board
}

fn greens(board: &Board, id: &str) -> Vec<String> {
    story_row(&board.fixture, id)
        .snapshot
        .comments
        .iter()
        .filter(|comment| {
            comment
                .text
                .starts_with(storyhook::service::VERIFICATION_GREEN_PREFIX)
        })
        .map(|comment| comment.text.clone())
        .collect()
}

#[test]
fn an_insertion_only_docs_conflict_is_smoothed_into_the_batch_and_named_where_it_lands() {
    let x = spec_with("X");
    let y = spec_with("Y");
    let board = smoothing_board(&[("docs/spec.md", &x), ("docs/spec.md", &y)], 2);
    let ids = board.stories.clone();
    let batcher = Batcher::new(&board, Gate::CertifiesTip);

    assert_eq!(tick(&board, &batcher), TickResult::Completed);

    let batch = &batches(&board)[0];
    assert_eq!(batch.phase, BatchPhase::Landed, "{:?}", batch.detail);
    assert_eq!(member_ids(batch), ids);
    assert!(batch.members[0].resolution.is_none());
    let resolution = batch.members[1]
        .resolution
        .as_ref()
        .expect("the last member's merge carries the resolution");
    assert_eq!(resolution.strategy, "union-insertions/1");
    assert_eq!(resolution.conflicted_with, [ids[0].clone()]);
    let paths: Vec<&str> = resolution.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["docs/spec.md"]);
    assert_eq!(
        git(
            &board.root,
            &["show", &format!("{}:docs/spec.md", batch.tip)]
        ),
        "# Spec\n\n## A\n\ntext\n\n## X\n\nX body\n\n## Y\n\nY body\n\n## End"
    );
    assert_eq!(
        git(&board.root, &["log", "-1", "--format=%B", &batch.tip])
            .lines()
            .filter(|line| line.starts_with("Storyhook-"))
            .collect::<Vec<_>>(),
        [
            format!("Storyhook-Batch: {}", batch.id),
            "Storyhook-Resolution: union-insertions/1".to_owned(),
            format!("Storyhook-Conflicted-With: {}", ids[0]),
            "Storyhook-Resolved-File: \"docs/spec.md\"".to_owned(),
        ]
    );

    let body = batcher.publications.lock().unwrap()[0].body.clone();
    for said in [
        "Automated conflict resolution (union-insertions/1)",
        &format!("merge commit {}", batch.tip),
        "- `docs/spec.md`",
        "No model wrote it",
        &format!("git show --remerge-diff {}", batch.tip),
    ] {
        assert!(
            body.contains(said),
            "{said:?} missing from the batch PR: {body}"
        );
    }
    for id in &ids {
        assert_eq!(story_row(&board.fixture, id).state, "done", "{id}");
        let green = greens(&board, id);
        assert_eq!(green.len(), 1, "{id}: {green:?}");
        assert!(
            green[0]
                .contains("automated conflict resolution (union-insertions/1) of `docs/spec.md`"),
            "{id}: {}",
            green[0]
        );
    }
    let record = &board.records()[0];
    assert_eq!(
        record["preview"]["members"][1]["smoothed"][0], "docs/spec.md",
        "{record}"
    );
}

#[test]
fn a_code_conflict_never_reaches_the_resolution_and_keeps_the_story_out() {
    let board = smoothing_board(
        &[
            ("a", "head\n"),
            ("a", "clashes with the head\n"),
            ("b", "clean\n"),
        ],
        3,
    );
    let ids = board.stories.clone();
    let batcher = Batcher::new(&board, Gate::CertifiesTip);

    assert_eq!(tick(&board, &batcher), TickResult::Completed);

    let batch = &batches(&board)[0];
    assert_eq!(member_ids(batch), [ids[0].clone(), ids[2].clone()]);
    assert!(
        batch
            .members
            .iter()
            .all(|member| member.resolution.is_none())
    );
    assert!(
        !batcher
            .calls()
            .contains(&format!("submit-member {}", ids[1])),
        "{:?}",
        batcher.calls()
    );
    assert!(
        queued(&board).iter().any(|(id, _)| *id == ids[1]),
        "it stays queued"
    );
    let record = &board.records()[0];
    let excluded = &record["preview"]["excluded"][0];
    assert_eq!(excluded["reason"], "conflict-with-member", "{record}");
    assert_eq!(excluded["smoothing"]["allowlisted"], false, "{record}");
}

/// The batch step classifies the conflict again with Git itself: a preview
/// that misreads a code conflict as a smoothable docs one is caught before
/// anything is written, and the story is left out with the reason.
#[test]
fn a_code_conflict_the_preview_misread_as_smoothable_is_refused_at_assembly() {
    // An insertion-only conflict, the one shape smoothing unites, but in
    // code: only the allowlist, read again from the base, keeps it out.
    let x = spec_with("X");
    let y = spec_with("Y");
    let board = smoothing_board(
        &[("src/lib.rs", &x), ("src/lib.rs", &y), ("b", "clean\n")],
        3,
    );
    let ids = board.stories.clone();
    let spec = git(&board.root, &["rev-parse", "origin/dev:docs/spec.md"]);
    let mut batcher = Batcher::new(&board, Gate::CertifiesTip);
    batcher.lie = Some(Lie {
        head: batcher.story_head(1),
        shape: ConflictShape {
            tree: "f".repeat(40),
            stages: (1..=3)
                .map(|stage| StageEntry {
                    mode: "100644".into(),
                    oid: spec.clone(),
                    stage,
                    path: "docs/spec.md".into(),
                })
                .collect(),
            records: vec![ConflictRecord {
                kind: "CONFLICT (contents)".into(),
                paths: vec!["docs/spec.md".into()],
            }],
        },
        text: b"a\n<<<<<<< o\nx\n||||||| b\n=======\ny\n>>>>>>> t\n".to_vec(),
    });

    assert_eq!(tick(&board, &batcher), TickResult::Completed);

    let record = &board.records()[0];
    assert_eq!(
        record["preview"]["members"][2]["smoothed"][0], "docs/spec.md",
        "the preview was deceived: {record}"
    );
    let batch = &batches(&board)[0];
    assert_eq!(member_ids(batch), [ids[0].clone(), ids[2].clone()]);
    assert!(
        batch
            .members
            .iter()
            .all(|member| member.resolution.is_none())
    );
    let left_out = batch
        .excluded
        .iter()
        .find(|excluded| excluded.story_id == ids[1])
        .expect("the misread story is left out");
    assert_eq!(left_out.reason, BatchExclusionReason::ConflictNotSmoothable);
    assert!(
        left_out.detail.contains("[batch] smooth"),
        "{}",
        left_out.detail
    );
    assert!(
        !git(&board.root, &["log", "--format=%B", &batch.tip]).contains("Storyhook-Resolution"),
        "no resolution was written"
    );
    assert_eq!(batch.phase, BatchPhase::Landed, "{:?}", batch.detail);
    assert!(
        queued(&board).iter().any(|(id, _)| *id == ids[1]),
        "it stays queued"
    );
}

/// A red batch whose culprit is the smoothed member tells its agent that
/// the red may come from the automated resolution, and how to repair it
/// (SH-834 D7); the member before it lands on its own receipt.
#[test]
fn a_smoothed_culprit_is_returned_with_its_resolution_named() {
    let x = spec_with("X");
    let y = spec_with("Y");
    let board = smoothing_board(&[("docs/spec.md", &x), ("docs/spec.md", &y)], 2);
    let ids = board.stories.clone();
    let batcher = Batcher::new(&board, Gate::Culprits(vec![1]));

    tick(&board, &batcher);

    let batch = &batches(&board)[0];
    assert!(batch.members[1].resolution.is_some(), "{batch:?}");
    let reds: Vec<String> = story_row(&board.fixture, &ids[1])
        .snapshot
        .comments
        .iter()
        .filter(|comment| comment.text.starts_with("CENTRAL VERIFICATION RED"))
        .map(|comment| comment.text.clone())
        .collect();
    assert_eq!(reds.len(), 1, "{reds:?}");
    for said in [
        format!(
            "{} joined the batch through an automated conflict resolution (union-insertions/1) of `docs/spec.md` with {}",
            ids[1], ids[0]
        ),
        format!("in merge commit {}", batch.tip),
        "the red may come from that resolution".to_owned(),
        "merge `dev` into this branch, resolve those files yourself".to_owned(),
    ] {
        assert!(reds[0].contains(&said), "{said:?} missing from {}", reds[0]);
    }
}
