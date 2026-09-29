//! The batch the verifier would form (SH-830; spec B1 to B3), selected over
//! real Git repositories: trial merges run in private object storage and the
//! repository is left exactly as it was.

use std::path::Path;
use std::process::Output;
use std::time::{Duration, Instant};

use storyhook::service::batch_preview::{
    BatchPreview, ExclusionReason, PreviewCandidate, PreviewOutcome, PreviewRequest, Standing,
    select,
};
use storyhook::service::trial_merge::{PrivateTrialMerger, TrialMerge, TrialMerger};
use storyhook_test_support::scratch_dir;
use tempfile::TempDir;

/// Room for a preview's trial merges on a loaded machine: patience, graced
/// by load, never a claim about how fast a merge runs (the deadline test uses
/// an instant that has already passed instead).
const PREVIEW_PATIENCE: Duration = Duration::from_secs(600);

/// A repository whose `origin/dev` starts at one base commit, with story
/// branches grown from it.
struct Repo {
    dir: TempDir,
    base: String,
}

impl Repo {
    fn new() -> Self {
        let dir = scratch_dir();
        let repo = Self {
            dir,
            base: String::new(),
        };
        repo.ok(&["init", "-q", "-b", "main"]);
        repo.ok(&["config", "user.email", "t@t"]);
        repo.ok(&["config", "user.name", "t"]);
        repo.ok(&["config", "commit.gpgsign", "false"]);
        storyhook_test_support::approve_fixture_identity(repo.path(), "t", "t@t");
        for file in ["a", "b", "c", "d", "e"] {
            repo.write(file, &format!("{file} base\n"));
        }
        repo.ok(&["add", "."]);
        repo.ok(&["commit", "-qm", "base"]);
        let base = repo.rev("HEAD");
        repo.ok(&["update-ref", "refs/remotes/origin/dev", &base]);
        Self { base, ..repo }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn git(&self, args: &[&str]) -> Output {
        storyhook::env::git_env::command(self.path())
            .args(args)
            .output()
            .expect("fixture: running git")
    }

    fn ok(&self, args: &[&str]) -> String {
        let output = self.git(args);
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn rev(&self, rev: &str) -> String {
        self.ok(&["rev-parse", rev])
    }

    fn write(&self, file: &str, body: &str) {
        std::fs::write(self.path().join(file), body).expect("fixture: writing a file");
    }

    /// A story branch off the base with one commit writing `file`.
    fn story(&self, branch: &str, file: &str, body: &str) -> String {
        self.ok(&["checkout", "-q", "-b", branch, &self.base]);
        self.write(file, body);
        self.ok(&["commit", "-qam", branch]);
        let head = self.rev("HEAD");
        self.ok(&["checkout", "-q", "main"]);
        head
    }

    /// Moves `origin/dev` on by one commit writing `file`.
    fn advance_base(&mut self, file: &str, body: &str) {
        self.ok(&["checkout", "-q", "-b", "dev-tip", &self.base]);
        self.write(file, body);
        self.ok(&["commit", "-qam", "dev moves"]);
        let tip = self.rev("HEAD");
        self.ok(&["checkout", "-q", "main"]);
        self.ok(&["update-ref", "refs/remotes/origin/dev", &tip]);
        self.base = tip;
    }

    fn merger(&self) -> PrivateTrialMerger {
        PrivateTrialMerger::open(self.path()).expect("opening private object storage")
    }

    /// Everything a preview must leave alone: refs, HEAD, the index file and
    /// the repository's object store.
    fn state(&self) -> (String, String, Vec<u8>, String) {
        (
            self.ok(&["for-each-ref"]),
            self.rev("HEAD"),
            std::fs::read(self.path().join(".git/index")).expect("fixture: reading the index"),
            self.ok(&["count-objects", "-v"]),
        )
    }
}

fn branch(story: &str) -> PreviewCandidate {
    PreviewCandidate {
        story_id: story.into(),
        standing: Standing::Branch(format!("worktree-{story}")),
    }
}

fn ineligible(story: &str, reason: ExclusionReason, detail: &str) -> PreviewCandidate {
    PreviewCandidate {
        story_id: story.into(),
        standing: Standing::Ineligible {
            reason,
            detail: detail.into(),
        },
    }
}

fn request(head: &str, rest: Vec<PreviewCandidate>, cap: u32) -> PreviewRequest {
    PreviewRequest {
        computed_at: "2026-01-01T00:00:00Z".into(),
        head: branch(head),
        queue_depth: rest.len() + 1,
        rest,
        base_branch: Some("dev".into()),
        cap,
        live_lanes: Some(cap),
        deadline: Instant::now() + storyhook_test_support::load_grace::graced_now(PREVIEW_PATIENCE),
    }
}

fn members(preview: &BatchPreview) -> Vec<&str> {
    preview
        .members
        .iter()
        .map(|member| member.story_id.as_str())
        .collect()
}

fn excluded(preview: &BatchPreview) -> Vec<(&str, ExclusionReason)> {
    preview
        .excluded
        .iter()
        .map(|entry| (entry.story_id.as_str(), entry.reason))
        .collect()
}

#[test]
fn a_clean_pair_forms_a_batch_of_two() {
    let repo = Repo::new();
    let head = repo.story("worktree-SH-1", "a", "one\n");
    let second = repo.story("worktree-SH-2", "b", "two\n");
    let before = repo.state();

    let preview = select(request("SH-1", vec![branch("SH-2")], 2), &mut repo.merger());

    assert_eq!(preview.outcome, PreviewOutcome::Batch, "{preview:?}");
    assert_eq!(members(&preview), ["SH-1", "SH-2"]);
    assert_eq!(preview.members[0].commit, head);
    assert_eq!(preview.members[1].commit, second);
    assert!(preview.excluded.is_empty(), "{preview:?}");
    assert_eq!(preview.base_commit.as_deref(), Some(repo.base.as_str()));
    assert_eq!(
        preview.head_tree.as_deref(),
        Some(repo.rev(&format!("{head}^{{tree}}")).as_str()),
        "the head descends from the base, so its merge tree is its own tree"
    );
    assert_eq!(
        repo.state(),
        before,
        "a preview changes nothing in the repository"
    );
}

#[test]
fn a_conflicting_pair_keeps_the_head_and_excludes_the_story_that_conflicts_with_it() {
    let repo = Repo::new();
    repo.story("worktree-SH-1", "a", "head's a\n");
    repo.story("worktree-SH-2", "a", "another a\n");

    let preview = select(request("SH-1", vec![branch("SH-2")], 3), &mut repo.merger());

    assert_eq!(members(&preview), ["SH-1"]);
    assert_eq!(
        excluded(&preview),
        [("SH-2", ExclusionReason::ConflictWithMember)]
    );
    assert_eq!(preview.excluded[0].paths, ["a"]);
}

#[test]
fn a_story_that_conflicts_with_a_later_member_is_excluded_and_the_batch_goes_on() {
    let repo = Repo::new();
    repo.story("worktree-SH-1", "a", "head\n");
    repo.story("worktree-SH-2", "b", "member\n");
    repo.story(
        "worktree-SH-3",
        "b",
        "clashes with the member, not the head\n",
    );
    repo.story("worktree-SH-4", "c", "still joins\n");

    let preview = select(
        request(
            "SH-1",
            vec![branch("SH-2"), branch("SH-3"), branch("SH-4")],
            4,
        ),
        &mut repo.merger(),
    );

    assert_eq!(members(&preview), ["SH-1", "SH-2", "SH-4"]);
    assert_eq!(
        excluded(&preview),
        [("SH-3", ExclusionReason::ConflictWithMember)]
    );
    assert_eq!(preview.excluded[0].paths, ["b"]);
}

#[test]
fn a_story_that_conflicts_with_the_base_is_told_apart_from_a_member_conflict() {
    let mut repo = Repo::new();
    repo.story("worktree-SH-2", "d", "written before dev moved\n");
    repo.advance_base("d", "dev's own d\n");
    repo.story("worktree-SH-1", "a", "head, from the new base\n");

    let preview = select(request("SH-1", vec![branch("SH-2")], 3), &mut repo.merger());

    assert_eq!(members(&preview), ["SH-1"]);
    assert_eq!(
        excluded(&preview),
        [("SH-2", ExclusionReason::ConflictWithBase)]
    );
    assert_eq!(preview.excluded[0].paths, ["d"]);
}

#[test]
fn the_cap_stops_membership_but_every_later_story_is_still_tried() {
    let repo = Repo::new();
    repo.story("worktree-SH-1", "a", "head\n");
    repo.story("worktree-SH-2", "b", "member\n");
    repo.story("worktree-SH-3", "c", "clean but the batch is full\n");
    repo.story("worktree-SH-4", "b", "clashes with the member\n");

    let preview = select(
        request(
            "SH-1",
            vec![branch("SH-2"), branch("SH-3"), branch("SH-4")],
            2,
        ),
        &mut repo.merger(),
    );

    assert_eq!(members(&preview), ["SH-1", "SH-2"]);
    assert_eq!(
        excluded(&preview),
        [
            ("SH-3", ExclusionReason::Cap),
            ("SH-4", ExclusionReason::ConflictWithMember),
        ]
    );
    assert_eq!(preview.cap, 2);
}

#[test]
fn a_cap_below_one_counts_as_one() {
    let repo = Repo::new();
    repo.story("worktree-SH-1", "a", "head\n");
    repo.story("worktree-SH-2", "b", "clean\n");

    let preview = select(request("SH-1", vec![branch("SH-2")], 0), &mut repo.merger());

    assert_eq!(preview.cap, 1);
    assert_eq!(members(&preview), ["SH-1"]);
    assert_eq!(excluded(&preview), [("SH-2", ExclusionReason::Cap)]);
}

#[test]
fn held_blocked_landing_pending_and_unsubmitted_stories_are_listed_in_order_without_a_merge() {
    let repo = Repo::new();
    repo.story("worktree-SH-1", "a", "head\n");
    repo.story("worktree-SH-3", "b", "joins\n");

    let preview = select(
        request(
            "SH-1",
            vec![
                ineligible("SH-2", ExclusionReason::Blocked, "blocked by SH-9"),
                branch("SH-3"),
                ineligible("SH-4", ExclusionReason::LandingPending, "a landing awaits"),
                ineligible("SH-5", ExclusionReason::Unsubmitted, "no leased branch"),
                branch("SH-6"),
                ineligible("SH-7", ExclusionReason::Held, "human-only"),
            ],
            5,
        ),
        &mut repo.merger(),
    );

    assert_eq!(members(&preview), ["SH-1", "SH-3"]);
    assert_eq!(
        excluded(&preview),
        [
            ("SH-2", ExclusionReason::Blocked),
            ("SH-4", ExclusionReason::LandingPending),
            ("SH-5", ExclusionReason::Unsubmitted),
            ("SH-6", ExclusionReason::Unsubmitted),
            ("SH-7", ExclusionReason::Held),
        ]
    );
    assert_eq!(
        preview.excluded[0].detail.as_deref(),
        Some("blocked by SH-9")
    );
    assert!(
        preview.excluded[3]
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("worktree-SH-6")),
        "a leased branch that names no commit cannot be submitted: {preview:?}"
    );
}

#[test]
fn an_empty_queue_gives_the_head_alone() {
    let repo = Repo::new();
    repo.story("worktree-SH-1", "a", "head\n");

    let preview = select(request("SH-1", Vec::new(), 3), &mut repo.merger());

    assert_eq!(preview.outcome, PreviewOutcome::Batch);
    assert_eq!(members(&preview), ["SH-1"]);
    assert!(preview.excluded.is_empty());
    assert_eq!(preview.queue_depth, 1);
}

#[test]
fn a_head_that_conflicts_with_its_base_forms_no_batch() {
    let mut repo = Repo::new();
    repo.story("worktree-SH-1", "e", "head's e\n");
    repo.story("worktree-SH-2", "b", "would be clean\n");
    repo.advance_base("e", "dev's e\n");

    let preview = select(request("SH-1", vec![branch("SH-2")], 3), &mut repo.merger());

    assert_eq!(preview.outcome, PreviewOutcome::HeadConflict, "{preview:?}");
    assert_eq!(preview.head_conflict, ["e"]);
    assert_eq!(members(&preview), ["SH-1"]);
    assert!(
        preview.excluded.is_empty(),
        "no sweep: the head keeps today's conflict hold (B3)"
    );
    assert!(preview.head_tree.is_none());
}

#[test]
fn a_missing_base_or_an_unsubmitted_attempt_makes_the_preview_unavailable() {
    let repo = Repo::new();
    repo.story("worktree-SH-1", "a", "head\n");

    let mut unsubmitted = request("SH-1", Vec::new(), 2);
    unsubmitted.base_branch = None;
    let preview = select(unsubmitted, &mut repo.merger());
    assert_eq!(preview.outcome, PreviewOutcome::Unavailable);
    assert!(
        preview.detail.as_deref().unwrap().contains("base branch"),
        "{preview:?}"
    );

    let mut elsewhere = request("SH-1", Vec::new(), 2);
    elsewhere.base_branch = Some("release".into());
    let preview = select(elsewhere, &mut repo.merger());
    assert_eq!(preview.outcome, PreviewOutcome::Unavailable);
    assert!(
        preview
            .detail
            .as_deref()
            .unwrap()
            .contains("origin/release"),
        "{preview:?}"
    );
    assert!(preview.members.is_empty());
}

#[test]
fn the_head_is_tried_at_the_exact_commit_its_submission_pushed() {
    let repo = Repo::new();
    let pushed = repo.story("worktree-SH-1", "a", "pushed\n");
    repo.ok(&["checkout", "-q", "worktree-SH-1"]);
    repo.write("a", "committed after the push\n");
    repo.ok(&["commit", "-qam", "later"]);
    repo.ok(&["checkout", "-q", "main"]);

    let mut exact = request("SH-1", Vec::new(), 1);
    exact.head.standing = Standing::Commit(pushed.clone());
    let preview = select(exact, &mut repo.merger());

    assert_eq!(preview.members[0].commit, pushed);
}

#[test]
fn a_passed_deadline_stops_every_later_trial_merge() {
    let repo = Repo::new();
    repo.story("worktree-SH-1", "a", "head\n");
    repo.story("worktree-SH-2", "b", "clean\n");

    let mut late = request(
        "SH-1",
        vec![
            branch("SH-2"),
            ineligible("SH-3", ExclusionReason::Blocked, "blocked"),
        ],
        3,
    );
    late.deadline = Instant::now();
    let preview = select(late, &mut repo.merger());

    assert_eq!(members(&preview), ["SH-1"]);
    assert_eq!(
        excluded(&preview),
        [
            ("SH-2", ExclusionReason::TrialFailed),
            ("SH-3", ExclusionReason::Blocked),
        ]
    );
    assert!(
        preview.excluded[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("deadline")
    );
}

#[test]
fn a_recorded_member_merge_has_the_batch_and_the_member_as_parents_and_is_never_signed() {
    let repo = Repo::new();
    let head = repo.story("worktree-SH-1", "a", "head\n");
    // A signer that always fails: a trial merge that signed would fail. Git
    // 2.54's commit-tree ignores commit.gpgSign, older ones honored it; the
    // merger passes --no-gpg-sign so no version can reach a signer.
    repo.ok(&["config", "commit.gpgsign", "true"]);
    repo.ok(&["config", "gpg.program", "false"]);
    let before = repo.state();
    let mut merger = repo.merger();

    let TrialMerge::Clean { tree } = merger.merge(&repo.base, &head).unwrap() else {
        panic!("a story off its base merges cleanly");
    };
    let commit = merger.commit(&repo.base, &head, &tree).unwrap();

    assert_eq!(
        merger.resolve(&format!("{commit}^1")).unwrap().as_deref(),
        Some(repo.base.as_str())
    );
    assert_eq!(
        merger.resolve(&format!("{commit}^2")).unwrap().as_deref(),
        Some(head.as_str())
    );
    assert_eq!(merger.resolve(&format!("{commit}^3")).unwrap(), None);
    assert_eq!(
        repo.git(&["cat-file", "-e", &commit]).status.code(),
        Some(1),
        "the merge commit lives only in private object storage"
    );
    assert_eq!(repo.state(), before);
}

#[test]
fn an_option_shaped_or_unpinned_argument_is_refused() {
    let repo = Repo::new();
    let mut merger = repo.merger();
    assert!(merger.resolve("--all").is_err());
    assert!(merger.resolve("").is_err());
    assert!(merger.merge("HEAD", &repo.base).is_err());
    assert!(merger.merge(&repo.base, "main").is_err());
}

#[test]
fn a_conflicted_trial_merge_reports_its_index_entries_and_records() {
    let repo = Repo::new();
    let head = repo.story("worktree-SH-1", "a", "head's a\n");
    let other = repo.story("worktree-SH-2", "a", "another a\n");
    let mut merger = repo.merger();
    let TrialMerge::Clean { tree } = merger.merge(&repo.base, &head).unwrap() else {
        panic!("a story off its base merges cleanly");
    };
    let batch = merger.commit(&repo.base, &head, &tree).unwrap();

    let TrialMerge::Conflict { paths, shape } = merger.merge(&batch, &other).unwrap() else {
        panic!("two stories that rewrite `a` conflict");
    };

    assert_eq!(paths, ["a"]);
    assert_eq!(shape.paths(), paths);
    let stages: Vec<(&str, u8, &str)> = shape
        .stages
        .iter()
        .map(|entry| (entry.mode.as_str(), entry.stage, entry.path.as_str()))
        .collect();
    assert_eq!(
        stages,
        [("100644", 1, "a"), ("100644", 2, "a"), ("100644", 3, "a")]
    );
    assert!(
        shape
            .records
            .iter()
            .any(|record| record.kind == "CONFLICT (contents)" && record.paths == ["a"]),
        "{shape:?}"
    );
    assert_ne!(
        shape.tree, tree,
        "the conflicted tree is written, not the batch's"
    );
}
