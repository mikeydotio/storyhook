//! The batch the verifier would form (SH-830; spec B1 to B3), selected over
//! real Git repositories: trial merges run in private object storage and the
//! repository is left exactly as it was.

use std::path::Path;
use std::process::Output;
use std::time::{Duration, Instant};

use storyhook::service::batch_preview::{
    BatchPreview, ExclusionReason, PreviewCandidate, PreviewOutcome, PreviewRequest,
    SmoothingClass, SmoothingMark, SmoothingMode, Standing, select,
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
        smoothing: SmoothingMode::Measure,
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

// Smoothing (SH-834; council decision D1): a story that conflicts with a
// member joins only when its conflict is insertion-only on paths the base's
// `[batch] smooth` admits, and then only as the last member.

/// The base spec both stories extend, and the pointer that admits `docs/`.
const SPEC: &str = "# Spec\n\n## A\n\ntext\n\n## End\n";
const SMOOTH_DOCS: &str =
    "schema = 1\nuuid = \"u\"\nprefix = \"SH\"\n\n[batch]\nsmooth = [\"docs/\"]\n";

/// `SPEC` with one section added before `## End`.
fn spec_with(section: &str) -> String {
    format!("# Spec\n\n## A\n\ntext\n\n## {section}\n\n{section} body\n\n## End\n")
}

impl Repo {
    /// Moves `origin/dev` on by one commit writing every file, new or not.
    fn base_files(&mut self, files: &[(&str, &str)]) {
        let branch = format!("dev-tip-{}", &self.base[..8]);
        self.ok(&["checkout", "-q", "-b", &branch, &self.base]);
        self.write_all(files);
        self.ok(&["commit", "-qm", "dev moves"]);
        let tip = self.rev("HEAD");
        self.ok(&["checkout", "-q", "main"]);
        self.ok(&["update-ref", "refs/remotes/origin/dev", &tip]);
        self.base = tip;
    }

    /// A story branch off the base with one commit writing every file.
    fn story_files(&self, branch: &str, files: &[(&str, &str)]) -> String {
        self.ok(&["checkout", "-q", "-b", branch, &self.base]);
        self.write_all(files);
        self.ok(&["commit", "-qm", branch]);
        let head = self.rev("HEAD");
        self.ok(&["checkout", "-q", "main"]);
        head
    }

    fn write_all(&self, files: &[(&str, &str)]) {
        for (file, body) in files {
            let path = self.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).expect("fixture: a directory");
            std::fs::write(path, body).expect("fixture: writing a file");
        }
        self.ok(&["add", "-A"]);
    }
}

/// A repository whose base holds `docs/spec.md` and, when given, `pointer`
/// as its `.storyhook.toml`.
fn smoothing_repo(pointer: Option<&str>) -> Repo {
    let mut repo = Repo::new();
    let mut files = vec![
        ("docs/spec.md", SPEC),
        ("docs/CLAUDE.md", SPEC),
        ("src/lib.rs", SPEC),
    ];
    if let Some(pointer) = pointer {
        files.push((".storyhook.toml", pointer));
    }
    repo.base_files(&files);
    repo
}

fn admit(head: &str, rest: Vec<PreviewCandidate>, cap: u32) -> PreviewRequest {
    PreviewRequest {
        smoothing: SmoothingMode::Admit,
        ..request(head, rest, cap)
    }
}

fn union_smoothable(allowlisted: bool) -> Option<SmoothingMark> {
    Some(SmoothingMark {
        class: SmoothingClass::UnionSmoothable,
        allowlisted,
    })
}

#[test]
fn an_insertion_only_docs_conflict_is_measured_and_in_admit_mode_joins_last() {
    let repo = smoothing_repo(Some(SMOOTH_DOCS));
    repo.story_files("worktree-SH-1", &[("docs/spec.md", &spec_with("X"))]);
    let second = repo.story_files("worktree-SH-2", &[("docs/spec.md", &spec_with("Y"))]);
    let before = repo.state();

    let measured = select(request("SH-1", vec![branch("SH-2")], 2), &mut repo.merger());
    assert_eq!(members(&measured), ["SH-1"]);
    assert_eq!(
        excluded(&measured),
        [("SH-2", ExclusionReason::ConflictWithMember)]
    );
    assert_eq!(measured.excluded[0].smoothing, union_smoothable(true));
    assert_eq!(measured.smooth, ["docs/"]);
    assert_eq!(measured.smoothing_unavailable, None);

    let admitted = select(admit("SH-1", vec![branch("SH-2")], 2), &mut repo.merger());
    assert_eq!(members(&admitted), ["SH-1", "SH-2"], "{admitted:?}");
    assert_eq!(admitted.members[1].commit, second);
    assert_eq!(admitted.members[1].smoothed, ["docs/spec.md"]);
    assert!(admitted.excluded.is_empty(), "{admitted:?}");
    assert_eq!(
        admitted.describe(),
        "SH-1 + SH-2 (smoothed) (cap 2, 2 queued)"
    );
    assert_eq!(repo.state(), before, "smoothing a preview changes nothing");
}

#[test]
fn a_smoothable_story_never_displaces_a_clean_one_and_always_joins_last() {
    let repo = smoothing_repo(Some(SMOOTH_DOCS));
    repo.story_files("worktree-SH-1", &[("docs/spec.md", &spec_with("X"))]);
    repo.story_files("worktree-SH-2", &[("docs/spec.md", &spec_with("Y"))]);
    repo.story("worktree-SH-3", "b", "clean\n");
    let rest = || vec![branch("SH-2"), branch("SH-3")];

    let roomy = select(admit("SH-1", rest(), 3), &mut repo.merger());
    assert_eq!(members(&roomy), ["SH-1", "SH-3", "SH-2"]);
    assert!(roomy.members[1].smoothed.is_empty());
    assert_eq!(roomy.members[2].smoothed, ["docs/spec.md"]);

    let full = select(admit("SH-1", rest(), 2), &mut repo.merger());
    assert_eq!(members(&full), ["SH-1", "SH-3"]);
    assert_eq!(
        excluded(&full),
        [("SH-2", ExclusionReason::ConflictWithMember)]
    );
    assert_eq!(full.excluded[0].smoothing, union_smoothable(true));
}

#[test]
fn a_code_conflict_never_smooths_and_keeps_the_story_out() {
    let repo = smoothing_repo(Some(SMOOTH_DOCS));
    repo.story_files("worktree-SH-1", &[("src/lib.rs", &spec_with("X"))]);
    repo.story_files("worktree-SH-2", &[("src/lib.rs", &spec_with("Y"))]);

    let preview = select(admit("SH-1", vec![branch("SH-2")], 3), &mut repo.merger());

    assert_eq!(members(&preview), ["SH-1"]);
    assert_eq!(
        excluded(&preview),
        [("SH-2", ExclusionReason::ConflictWithMember)]
    );
    assert_eq!(
        preview.excluded[0].smoothing,
        union_smoothable(false),
        "the shape is measured, but src/ is not on the allowlist"
    );
}

#[test]
fn a_conflict_in_code_and_docs_together_keeps_the_story_out() {
    let repo = smoothing_repo(Some(SMOOTH_DOCS));
    repo.story_files(
        "worktree-SH-1",
        &[
            ("docs/spec.md", &spec_with("X")),
            ("src/lib.rs", &spec_with("X")),
        ],
    );
    repo.story_files(
        "worktree-SH-2",
        &[
            ("docs/spec.md", &spec_with("Y")),
            ("src/lib.rs", &spec_with("Y")),
        ],
    );

    let preview = select(admit("SH-1", vec![branch("SH-2")], 3), &mut repo.merger());

    assert_eq!(members(&preview), ["SH-1"]);
    assert_eq!(preview.excluded[0].paths, ["docs/spec.md", "src/lib.rs"]);
    assert_eq!(preview.excluded[0].smoothing, union_smoothable(false));
}

#[test]
fn a_docs_conflict_that_changes_shared_lines_is_an_agent_candidate_and_stays_out() {
    let repo = smoothing_repo(Some(SMOOTH_DOCS));
    repo.story_files(
        "worktree-SH-1",
        &[("docs/spec.md", &SPEC.replace("text", "head text"))],
    );
    repo.story_files(
        "worktree-SH-2",
        &[("docs/spec.md", &SPEC.replace("text", "other text"))],
    );

    let preview = select(admit("SH-1", vec![branch("SH-2")], 3), &mut repo.merger());

    assert_eq!(members(&preview), ["SH-1"]);
    assert_eq!(
        preview.excluded[0].smoothing,
        Some(SmoothingMark {
            class: SmoothingClass::AgentCandidate,
            allowlisted: true,
        })
    );
}

#[test]
fn the_deny_floor_holds_an_allowlisted_path_and_add_add_is_never_smoothed() {
    let repo = smoothing_repo(Some(SMOOTH_DOCS));
    repo.story_files(
        "worktree-SH-1",
        &[
            ("docs/CLAUDE.md", &spec_with("X")),
            ("docs/new.md", "head's new file\n"),
        ],
    );
    repo.story_files("worktree-SH-2", &[("docs/CLAUDE.md", &spec_with("Y"))]);
    repo.story_files("worktree-SH-3", &[("docs/new.md", "another new file\n")]);

    let preview = select(
        admit("SH-1", vec![branch("SH-2"), branch("SH-3")], 3),
        &mut repo.merger(),
    );

    assert_eq!(members(&preview), ["SH-1"]);
    assert_eq!(
        excluded(&preview),
        [
            ("SH-2", ExclusionReason::ConflictWithMember),
            ("SH-3", ExclusionReason::ConflictWithMember)
        ]
    );
    assert_eq!(
        preview.excluded[0].smoothing, None,
        "docs/CLAUDE.md is floored"
    );
    assert_eq!(
        preview.excluded[1].smoothing, None,
        "add/add has no base side"
    );
}

#[test]
fn a_member_cannot_widen_its_own_allowlist() {
    let repo = smoothing_repo(None);
    repo.story_files("worktree-SH-1", &[("docs/spec.md", &spec_with("X"))]);
    repo.story_files(
        "worktree-SH-2",
        &[
            ("docs/spec.md", &spec_with("Y")),
            (".storyhook.toml", SMOOTH_DOCS),
        ],
    );

    let preview = select(admit("SH-1", vec![branch("SH-2")], 3), &mut repo.merger());

    assert_eq!(members(&preview), ["SH-1"]);
    assert!(preview.smooth.is_empty(), "the base has no [batch] table");
    assert_eq!(preview.excluded[0].smoothing, union_smoothable(false));
}

#[test]
fn without_a_batch_table_admit_forms_exactly_the_batch_it_formed_before() {
    let repo = smoothing_repo(None);
    repo.story_files("worktree-SH-1", &[("docs/spec.md", &spec_with("X"))]);
    repo.story_files("worktree-SH-2", &[("docs/spec.md", &spec_with("Y"))]);
    repo.story("worktree-SH-3", "b", "clean\n");
    let rest = || vec![branch("SH-2"), branch("SH-3")];

    let measured = select(request("SH-1", rest(), 3), &mut repo.merger());
    let admitted = select(admit("SH-1", rest(), 3), &mut repo.merger());

    assert_eq!(members(&admitted), ["SH-1", "SH-3"]);
    assert_eq!(members(&admitted), members(&measured));
    assert_eq!(excluded(&admitted), excluded(&measured));
    assert!(admitted.smooth.is_empty());
}

#[test]
fn a_docs_conflict_with_the_base_is_never_smoothed() {
    let mut repo = smoothing_repo(Some(SMOOTH_DOCS));
    repo.story_files("worktree-SH-2", &[("docs/spec.md", &spec_with("Y"))]);
    repo.base_files(&[("docs/spec.md", &spec_with("Dev"))]);
    repo.story("worktree-SH-1", "a", "head\n");

    let preview = select(admit("SH-1", vec![branch("SH-2")], 3), &mut repo.merger());

    assert_eq!(members(&preview), ["SH-1"]);
    assert_eq!(
        excluded(&preview),
        [("SH-2", ExclusionReason::ConflictWithBase)]
    );
    assert_eq!(preview.excluded[0].smoothing, None);
}

#[test]
fn an_invalid_batch_table_smooths_nothing_and_says_why() {
    let pointer =
        "schema = 1\nuuid = \"u\"\nprefix = \"SH\"\n\n[batch]\nsmooth = [\"docs/*.md\"]\n";
    let repo = smoothing_repo(Some(pointer));
    repo.story_files("worktree-SH-1", &[("docs/spec.md", &spec_with("X"))]);
    repo.story_files("worktree-SH-2", &[("docs/spec.md", &spec_with("Y"))]);

    let preview = select(admit("SH-1", vec![branch("SH-2")], 3), &mut repo.merger());

    assert_eq!(members(&preview), ["SH-1"]);
    assert_eq!(preview.excluded[0].smoothing, None);
    let why = preview.smoothing_unavailable.as_deref().unwrap();
    assert!(why.contains("docs/*.md"), "{why}");
}

#[test]
fn the_users_conflict_style_does_not_change_what_smooths() {
    let repo = smoothing_repo(Some(SMOOTH_DOCS));
    repo.story_files("worktree-SH-1", &[("docs/spec.md", &spec_with("X"))]);
    repo.story_files("worktree-SH-2", &[("docs/spec.md", &spec_with("Y"))]);
    for style in ["merge", "zdiff3"] {
        repo.ok(&["config", "merge.conflictStyle", style]);
        let preview = select(admit("SH-1", vec![branch("SH-2")], 2), &mut repo.merger());
        assert_eq!(
            members(&preview),
            ["SH-1", "SH-2"],
            "merge.conflictStyle={style}"
        );
    }
}
