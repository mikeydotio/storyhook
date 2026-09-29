//! A verification batch's branch (SH-831; spec B4), assembled over real Git
//! repositories: merge commits in queue order onto the trial-merge base, every
//! member tip reachable, the repository's own identity, and no ref, HEAD or
//! index changed.

use std::path::Path;
use std::process::Output;

use storyhook::daemon::verification::VerificationCancellation as Cancellation;
use storyhook::service::batch_assembly::{AssemblyMember, assemble};
use storyhook::service::trial_merge::{PrivateTrialMerger, TrialMerge, TrialMerger};
use storyhook_test_support::scratch_dir;
use tempfile::TempDir;

const BRANCH: &str = "storyhook/verify-batch/0123456789ab";

/// A repository with a base commit and story branches grown from it.
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
        for file in ["a", "b", "c", "d"] {
            std::fs::write(repo.path().join(file), format!("{file} base\n")).unwrap();
        }
        repo.ok(&["add", "."]);
        repo.ok(&["commit", "-qm", "base"]);
        let base = repo.ok(&["rev-parse", "HEAD"]);
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

    /// A story branch off the base with one commit writing `file`.
    fn story(&self, story: &str, file: &str, body: &str) -> AssemblyMember {
        let branch = format!("worktree-{story}");
        self.ok(&["checkout", "-q", "-b", &branch, &self.base]);
        std::fs::write(self.path().join(file), body).unwrap();
        self.ok(&["commit", "-qam", story]);
        let commit = self.ok(&["rev-parse", "HEAD"]);
        self.ok(&["checkout", "-q", "main"]);
        AssemblyMember {
            story_id: story.into(),
            branch,
            commit,
        }
    }

    /// Everything assembly must leave alone: refs, HEAD and the index file.
    fn state(&self) -> (String, String, Vec<u8>) {
        (
            self.ok(&["for-each-ref"]),
            self.ok(&["rev-parse", "HEAD"]),
            std::fs::read(self.path().join(".git/index")).unwrap(),
        )
    }
}

#[test]
fn members_merge_in_queue_order_as_merge_commits_with_every_tip_reachable() {
    let repo = Repo::new();
    let members = vec![
        repo.story("SH-5", "a", "a from SH-5\n"),
        repo.story("SH-3", "b", "b from SH-3\n"),
        repo.story("SH-9", "c", "c from SH-9\n"),
    ];
    let before = repo.state();

    let assembly = assemble(
        repo.path(),
        BRANCH,
        &repo.base,
        &members,
        &Cancellation::default(),
    )
    .expect("clean members assemble");

    assert_eq!(assembly.merges.len(), 3);
    assert_eq!(assembly.tip, assembly.merges[2]);
    // The first-parent chain is the batch branch itself: base, then one
    // merge per member in queue order.
    let chain: Vec<String> = repo
        .ok(&["rev-list", "--first-parent", &assembly.tip])
        .lines()
        .map(str::to_owned)
        .collect();
    let mut expected: Vec<String> = assembly.merges.iter().rev().cloned().collect();
    expected.push(repo.base.clone());
    assert_eq!(chain, expected);
    let mut onto = repo.base.clone();
    for (merge, member) in assembly.merges.iter().zip(&members) {
        assert_eq!(
            repo.ok(&["rev-list", "--parents", "-n", "1", merge]),
            format!("{merge} {onto} {}", member.commit),
            "a --no-ff merge of {} onto the batch so far",
            member.story_id
        );
        assert_eq!(
            repo.ok(&["log", "-1", "--format=%s", merge]),
            format!(
                "Merge {} ({}) into {BRANCH}",
                member.story_id, member.branch
            )
        );
        assert!(
            repo.git(&["merge-base", "--is-ancestor", &member.commit, &assembly.tip])
                .status
                .success(),
            "{} is reachable from the batch tip",
            member.story_id
        );
        onto = merge.clone();
    }
    assert_eq!(
        repo.ok(&["rev-parse", &format!("{}^{{tree}}", assembly.tip)]),
        assembly.tree
    );
    for (file, body) in [
        ("a", "a from SH-5"),
        ("b", "b from SH-3"),
        ("c", "c from SH-9"),
        ("d", "d base"),
    ] {
        assert_eq!(
            repo.ok(&["show", &format!("{}:{file}", assembly.tip)]),
            body
        );
    }
    assert_eq!(repo.state(), before, "no ref, HEAD or index changed");
}

#[test]
fn the_batch_tree_is_the_tree_the_trial_merges_accepted() {
    let repo = Repo::new();
    let members = vec![
        repo.story("SH-1", "a", "a from SH-1\n"),
        repo.story("SH-2", "b", "b from SH-2\n"),
    ];
    let mut merger = PrivateTrialMerger::open(repo.path()).unwrap();
    let mut trial = repo.base.clone();
    let mut tree = String::new();
    for member in &members {
        match merger.merge(&trial, &member.commit).unwrap() {
            TrialMerge::Clean { tree: merged } => {
                trial = merger.commit(&trial, &member.commit, &merged).unwrap();
                tree = merged;
            }
            TrialMerge::Conflict { paths } => panic!("fixture members conflict: {paths:?}"),
        }
    }

    let assembly = assemble(
        repo.path(),
        BRANCH,
        &repo.base,
        &members,
        &Cancellation::default(),
    )
    .unwrap();

    assert_eq!(assembly.tree, tree);
    assert_ne!(
        assembly.tip, trial,
        "real merges carry the repository's identity and time, not the trial's"
    );
}

#[test]
fn merges_carry_the_repository_identity_and_are_never_signed() {
    let repo = Repo::new();
    // A signer that would fail (or prompt) if assembly ever asked for one.
    // Current Git's commit-tree ignores commit.gpgSign; assembly also passes
    // --no-gpg-sign for any Git that lets the setting reach it.
    repo.ok(&["config", "commit.gpgsign", "true"]);
    repo.ok(&["config", "gpg.program", "/usr/bin/false"]);
    let members = vec![
        repo.story_unsigned("SH-1", "a"),
        repo.story_unsigned("SH-2", "b"),
    ];

    let assembly = assemble(
        repo.path(),
        BRANCH,
        &repo.base,
        &members,
        &Cancellation::default(),
    )
    .expect("assembly never signs");

    for merge in &assembly.merges {
        assert_eq!(
            repo.ok(&["log", "-1", "--format=%an <%ae>|%cn <%ce>|%G?", merge]),
            "t <t@t>|t <t@t>|N"
        );
    }
}

impl Repo {
    /// [`Repo::story`] with signing off for the fixture's own commit.
    fn story_unsigned(&self, story: &str, file: &str) -> AssemblyMember {
        let branch = format!("worktree-{story}");
        self.ok(&["checkout", "-q", "-b", &branch, &self.base]);
        std::fs::write(self.path().join(file), format!("{file} from {story}\n")).unwrap();
        self.ok(&["-c", "commit.gpgsign=false", "commit", "-qam", story]);
        let commit = self.ok(&["rev-parse", "HEAD"]);
        self.ok(&["checkout", "-q", "main"]);
        AssemblyMember {
            story_id: story.into(),
            branch,
            commit,
        }
    }
}

#[test]
fn a_conflicting_member_is_an_error_naming_it_and_its_paths() {
    let repo = Repo::new();
    let members = vec![
        repo.story("SH-1", "a", "a from SH-1\n"),
        repo.story("SH-2", "a", "a from SH-2\n"),
    ];
    let before = repo.state();

    let error = assemble(
        repo.path(),
        BRANCH,
        &repo.base,
        &members,
        &Cancellation::default(),
    )
    .unwrap_err()
    .to_string();

    assert!(error.contains("SH-2"), "{error}");
    assert!(error.contains("conflicts"), "{error}");
    assert!(error.ends_with(" a"), "{error}");
    assert_eq!(repo.state(), before);
}

#[test]
fn an_unusable_identity_is_an_error_not_a_foreign_author() {
    let repo = Repo::new();
    let members = vec![repo.story("SH-1", "a", "a from SH-1\n")];
    // An empty name overrides any global identity and git refuses it.
    repo.ok(&["config", "user.name", ""]);

    let error = assemble(
        repo.path(),
        BRANCH,
        &repo.base,
        &members,
        &Cancellation::default(),
    )
    .unwrap_err()
    .to_string();

    assert!(
        error.contains("could not record the merge of SH-1"),
        "{error}"
    );
}

#[test]
fn short_or_missing_inputs_are_refused() {
    let repo = Repo::new();
    let members = vec![repo.story("SH-1", "a", "a from SH-1\n")];

    let mut short = members.clone();
    short[0].commit.truncate(12);
    assert!(
        assemble(
            repo.path(),
            BRANCH,
            &repo.base,
            &short,
            &Cancellation::default()
        )
        .is_err()
    );
    assert!(
        assemble(
            repo.path(),
            BRANCH,
            "HEAD",
            &members,
            &Cancellation::default()
        )
        .is_err()
    );
    assert!(
        assemble(
            repo.path(),
            BRANCH,
            &repo.base,
            &[],
            &Cancellation::default()
        )
        .is_err()
    );
}
