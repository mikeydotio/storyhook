//! Real Git operations must establish intervention trees without borrowing checkout state.
use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};
use storyhook::{
    daemon::verification::VerificationCancellation as Cancellation,
    service::attribution::{DetectorRelation, PreparedTrees, ProbeSide, TreeIntervention},
    store::GateInputs,
};

/// Fixture patience, not a production diagnosis allowance or timing assertion.
const PREPARATION_PATIENCE: Duration = Duration::from_secs(300);

struct Repo {
    dir: tempfile::TempDir,
    base: String,
}

impl Repo {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("sh870-trees-")
            .tempdir_in("/tmp")
            .unwrap();
        let mut repo = Self {
            dir,
            base: String::new(),
        };
        repo.git(&["init", "--quiet", "--initial-branch=fixture"]);
        repo.git(&["config", "user.email", "fixture@localhost"]);
        repo.git(&["config", "user.name", "Fixture"]);
        repo.git(&["config", "commit.gpgsign", "false"]);
        storyhook_test_support::approve_fixture_identity(
            repo.dir.path(),
            "Fixture",
            "fixture@localhost",
        );
        repo.write("src/lib.rs", "pub fn answer() -> u32 { 42 }\n");
        repo.write(
            "tests/check.rs",
            "#[test] fn answer() { assert_eq!(sut::answer(), 42); }\n",
        );
        repo.write("fixtures/input.txt", "42\n");
        repo.base = repo.commit();
        repo
    }
    fn git(&self, args: &[&str]) -> String {
        let out = storyhook::env::git_env::command(self.dir.path())
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout)
            .unwrap()
            .trim_end_matches('\n')
            .into()
    }
    fn write(&self, name: &str, content: &str) {
        let path = self.dir.path().join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
    fn commit(&self) -> String {
        self.git(&["add", "."]);
        self.git(&["commit", "--quiet", "-m", "fixture"]);
        self.git(&["rev-parse", "HEAD"])
    }
    fn prepare(
        &self,
        protected: &[&str],
        intervention: TreeIntervention,
    ) -> Result<PreparedTrees, storyhook::error::AppError> {
        PreparedTrees::prepare(
            self.dir.path(),
            &GateInputs {
                head: Some(self.git(&["rev-parse", "HEAD"])),
                base: Some(self.base.clone()),
                tree: Some(self.git(&["rev-parse", "HEAD^{tree}"])),
                ..GateInputs::default()
            },
            &protected.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
            intervention,
            Instant::now() + storyhook_test_support::load_grace::graced_now(PREPARATION_PATIENCE),
            &Cancellation::default(),
        )
    }
}

fn read(root: &Path, path: &str) -> String {
    fs::read_to_string(root.join(path)).unwrap()
}

#[test]
fn native_base_control_uses_pinned_objects_and_preserves_dirty_checkout() {
    let repo = Repo::new();
    repo.write("src/lib.rs", "pub fn answer() -> u32 { 41 }\n");
    repo.commit();
    repo.write("src/lib.rs", "uncommitted work must survive\n");
    let before = repo.git(&["status", "--porcelain=v1"]);
    let objects = repo.git(&["count-objects", "-v"]);
    let trees = repo
        .prepare(&["tests/check.rs"], TreeIntervention::Unchanged)
        .unwrap();
    assert_eq!(
        trees.trees().1,
        repo.git(&["rev-parse", &format!("{}^{{tree}}", repo.base)])
    );
    assert_ne!(trees.trees().0, trees.trees().1);
    assert_eq!(trees.relation(), DetectorRelation::Unchanged);
    assert!(trees.detector().starts_with("sha256:"));
    let candidate = trees.materialize(ProbeSide::Candidate).unwrap();
    let control = trees.materialize(ProbeSide::Control).unwrap();
    assert!(read(candidate.path(), "src/lib.rs").contains("41"));
    assert!(read(control.path(), "src/lib.rs").contains("42"));
    assert_eq!(
        read(candidate.path(), "tests/check.rs"),
        read(control.path(), "tests/check.rs")
    );
    assert!(
        std::str::from_utf8(trees.patch())
            .unwrap()
            .contains("diff --git a/src/lib.rs b/src/lib.rs")
    );
    assert_eq!(repo.git(&["status", "--porcelain=v1"]), before);
    assert_eq!(repo.git(&["count-objects", "-v"]), objects);
    assert_eq!(
        read(repo.dir.path(), "src/lib.rs"),
        "uncommitted work must survive\n"
    );
}

#[test]
fn native_transplant_preserves_new_detector_without_the_production_change() {
    let repo = Repo::new();
    repo.write(
        "tests/new.rs",
        "#[test] fn new_detector() { assert_eq!(sut::answer(), 42); }\n",
    );
    repo.write("src/lib.rs", "pub fn answer() -> u32 { 0 }\n");
    repo.commit();
    let trees = repo
        .prepare(
            &["tests/new.rs"],
            TreeIntervention::Transplant(vec!["tests/new.rs".into()]),
        )
        .unwrap();
    let control = trees.materialize(ProbeSide::Control).unwrap();
    assert_eq!(
        read(control.path(), "tests/new.rs"),
        read(repo.dir.path(), "tests/new.rs")
    );
    assert!(read(control.path(), "src/lib.rs").contains("42"));
    assert!(
        matches!(trees.relation(), DetectorRelation::Transplant { patch } if patch.starts_with("sha256:"))
    );
    assert!(
        repo.prepare(&["tests/new.rs"], TreeIntervention::Unchanged)
            .is_err()
    );
}

#[test]
fn native_fixture_ablation_preserves_assertion_and_candidate_production() {
    let repo = Repo::new();
    repo.write("fixtures/input.txt", "broken\n");
    repo.write("fixtures/added.txt", "new\n");
    repo.write("src/lib.rs", "pub fn answer() -> u32 { 99 }\n");
    repo.commit();
    let trees = repo
        .prepare(
            &["tests/check.rs", "src/lib.rs"],
            TreeIntervention::Ablation(vec![
                "fixtures/input.txt".into(),
                "fixtures/added.txt".into(),
            ]),
        )
        .unwrap();
    let control = trees.materialize(ProbeSide::Control).unwrap();
    assert_eq!(read(control.path(), "fixtures/input.txt"), "42\n");
    assert!(!control.path().join("fixtures/added.txt").exists());
    for path in ["tests/check.rs", "src/lib.rs"] {
        assert_eq!(read(control.path(), path), read(repo.dir.path(), path));
    }
    assert!(matches!(
        trees.relation(),
        DetectorRelation::Ablation { .. }
    ));
}

#[test]
fn native_preparation_refuses_detector_changes_and_ambiguous_interventions() {
    let repo = Repo::new();
    repo.write(
        "tests/check.rs",
        "#[test] fn answer() { assert!(false); }\n",
    );
    repo.commit();
    assert!(
        repo.prepare(&["tests/check.rs"], TreeIntervention::Unchanged)
            .is_err()
    );
    assert!(
        repo.prepare(
            &["tests/check.rs"],
            TreeIntervention::Ablation(vec!["tests/check.rs".into()])
        )
        .is_err()
    );
    for paths in [
        vec![],
        vec!["missing"],
        vec!["tests/check.rs", "tests/check.rs"],
        vec!["../escape"],
        vec!["./tests/check.rs"],
        vec!["tests/*"],
        vec![".git/config"],
    ] {
        assert!(
            repo.prepare(
                &["tests/check.rs"],
                TreeIntervention::Transplant(paths.into_iter().map(String::from).collect())
            )
            .is_err()
        );
    }
    assert!(repo.prepare(&[], TreeIntervention::Unchanged).is_err());
}

#[test]
fn native_preparation_refuses_foreign_tree_moving_refs_conflicts_and_withdrawal() {
    let repo = Repo::new();
    repo.write("src/lib.rs", "candidate\n");
    let head = repo.commit();
    let tree = repo.git(&["rev-parse", "HEAD^{tree}"]);
    let protected = vec!["tests/check.rs".into()];
    let run = |head: &str, base: &str, tree: &str, deadline, cancellation: &Cancellation| {
        PreparedTrees::prepare(
            repo.dir.path(),
            &GateInputs {
                head: Some(head.into()),
                base: Some(base.into()),
                tree: Some(tree.into()),
                ..GateInputs::default()
            },
            &protected,
            TreeIntervention::Unchanged,
            deadline,
            cancellation,
        )
    };
    let later =
        Instant::now() + storyhook_test_support::load_grace::graced_now(PREPARATION_PATIENCE);
    for wrong in ["HEAD", "--help", "0000000000000000000000000000000000000000"] {
        assert!(run(wrong, &repo.base, &tree, later, &Cancellation::default()).is_err());
    }
    assert!(
        run(
            &head,
            &repo.base,
            &repo.git(&["rev-parse", &format!("{}^{{tree}}", repo.base)]),
            later,
            &Cancellation::default()
        )
        .is_err()
    );
    assert!(
        run(
            &head,
            &repo.base,
            &tree,
            Instant::now(),
            &Cancellation::default()
        )
        .is_err()
    );
    repo.git(&["checkout", "--quiet", "--detach", &repo.base]);
    repo.write("src/lib.rs", "other\n");
    let base = repo.commit();
    assert!(run(&head, &base, &tree, later, &Cancellation::default()).is_err());
}

#[test]
fn native_materialization_refuses_symlinks_and_submodules() {
    for submodule in [false, true] {
        let repo = Repo::new();
        repo.write("src/lib.rs", "changed\n");
        if submodule {
            repo.git(&["add", "."]);
            repo.git(&[
                "update-index",
                "--add",
                "--cacheinfo",
                "160000",
                &repo.base,
                "dependency",
            ]);
            repo.git(&["commit", "--quiet", "-m", "gitlink"]);
        } else {
            std::os::unix::fs::symlink("/tmp", repo.dir.path().join("escape")).unwrap();
        }
        if !submodule {
            repo.commit();
        }
        let trees = repo
            .prepare(&["tests/check.rs"], TreeIntervention::Unchanged)
            .unwrap();
        assert!(trees.materialize(ProbeSide::Candidate).is_err());
    }
}

#[test]
fn native_inputs_do_not_inherit_repository_index_hooks_or_settings() {
    use std::os::unix::fs::PermissionsExt;
    let repo = Repo::new();
    repo.write("tests/new.rs", "#[test] fn detector() { assert!(true); }\n");
    repo.write("src/lib.rs", "changed\n");
    repo.commit();
    repo.git(&["config", "core.splitIndex", "true"]);
    let marker = repo.dir.path().join("hook-ran");
    repo.write(
        ".git/hooks/post-index-change",
        &format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    );
    fs::set_permissions(
        repo.dir.path().join(".git/hooks/post-index-change"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let before = fs::read(repo.dir.path().join(".git/index")).unwrap();
    let _trees = repo
        .prepare(
            &["tests/new.rs"],
            TreeIntervention::Transplant(vec!["tests/new.rs".into()]),
        )
        .unwrap();
    assert!(
        !marker.exists(),
        "native preparation invoked the source repository's hook"
    );
    assert_eq!(
        fs::read(repo.dir.path().join(".git/index")).unwrap(),
        before
    );
    assert!(
        !fs::read_dir(repo.dir.path().join(".git"))
            .unwrap()
            .any(|p| p
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("sharedindex."))
    );
}

#[test]
fn native_inputs_are_rechecked_and_cleanup_is_explicit() {
    use std::os::unix::fs::PermissionsExt;
    let repo = Repo::new();
    repo.write("src/lib.rs", "changed\n");
    repo.commit();
    let trees = repo
        .prepare(&["tests/check.rs"], TreeIntervention::Unchanged)
        .unwrap();
    for mutation in ["content", "mode", "extra", "missing", "link", "directory"] {
        let directory = trees.materialize(ProbeSide::Candidate).unwrap();
        directory.verify_unchanged().unwrap();
        let file = directory.path().join("tests/check.rs");
        match mutation {
            "content" => fs::write(&file, "different detector").unwrap(),
            "mode" => fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap(),
            "extra" => fs::write(directory.path().join("extra"), "unexpected input").unwrap(),
            "missing" => fs::remove_file(&file).unwrap(),
            "link" => {
                fs::remove_file(&file).unwrap();
                std::os::unix::fs::symlink("/dev/zero", &file).unwrap();
            }
            "directory" => fs::create_dir(directory.path().join("extra-empty-directory")).unwrap(),
            _ => unreachable!(),
        }
        assert!(directory.verify_unchanged().is_err(), "accepted {mutation}");
        let root = directory.path().to_path_buf();
        directory.close().unwrap();
        assert!(!root.exists());
    }
}
