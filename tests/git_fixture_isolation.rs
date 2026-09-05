//! Git repository fixtures must not inherit repository-targeting variables.
//!
//! A linked-worktree git-dir is the sharp fixture: `git init` classifies its
//! non-`.git` basename as bare and writes that answer, plus later fixture
//! identity, into the repository's shared config. The parent tests below own
//! throwaway source repositories; only their child processes receive the
//! poisoned environment.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use storyhook_test_support::{TestEnv, git, scratch_dir, without_rust_comments};

const RUST_CHILD: &str = "SH572_RUST_FIXTURE_CHILD";
const ENVIRONMENT_CHILD: &str = "SH572_GIT_ENVIRONMENT_CHILD";
const ENVIRONMENT_OUTPUT: &str = "SH572_GIT_ENVIRONMENT_OUTPUT";
const ORIGINAL_NAME: &str = "Outer Repository Owner";
const ORIGINAL_EMAIL: &str = "owner@example.test";

fn clean_git(cwd: &Path, args: &[&str]) -> Output {
    storyhook::env::git_env::command(cwd)
        .args(args)
        .output()
        .expect("running isolated git")
}

fn git_stdout(cwd: &Path, args: &[&str]) -> String {
    let output = clean_git(cwd, args);
    assert!(
        output.status.success(),
        "`git {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn environment(path: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()))
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .filter(|(name, _)| name != "_")
        .collect()
}

fn tracked_files(pathspecs: &[&str]) -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut args = vec!["ls-files", "-z", "--"];
    args.extend_from_slice(pathspecs);
    let listed = clean_git(root, &args);
    assert!(
        listed.status.success(),
        "`git ls-files` failed, so the fixture scan proved nothing: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let relative = std::str::from_utf8(entry)
                .expect("a UTF-8 tracked path")
                .to_owned();
            let text = std::fs::read_to_string(root.join(&relative))
                .unwrap_or_else(|error| panic!("reading {relative}: {error}"));
            (relative, text)
        })
        .collect()
}

struct PoisonedRepository {
    _root: tempfile::TempDir,
    root: std::path::PathBuf,
    worktree: std::path::PathBuf,
    git_dir: std::path::PathBuf,
    index: std::path::PathBuf,
}

impl PoisonedRepository {
    fn new() -> Self {
        let root = scratch_dir();
        let repository = root.path().join("source");
        let worktree = root.path().join("linked");
        std::fs::create_dir(&repository).expect("creating source repository");
        assert!(
            clean_git(&repository, &["init", "-q", "-b", "main"])
                .status
                .success(),
            "initializing source repository"
        );
        for args in [
            ["config", "user.name", ORIGINAL_NAME].as_slice(),
            ["config", "user.email", ORIGINAL_EMAIL].as_slice(),
            ["config", "core.bare", "false"].as_slice(),
            ["commit", "-qm", "source", "--allow-empty"].as_slice(),
        ] {
            let output = clean_git(&repository, args);
            assert!(
                output.status.success(),
                "preparing source repository with `git {}`: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let output = clean_git(
            &repository,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                &worktree.display().to_string(),
                "HEAD",
            ],
        );
        assert!(
            output.status.success(),
            "creating linked worktree: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let git_dir =
            std::path::PathBuf::from(git_stdout(&worktree, &["rev-parse", "--absolute-git-dir"]));
        let index = git_dir.join("index");
        Self {
            _root: root,
            root: repository,
            worktree,
            git_dir,
            index,
        }
    }

    fn poison(&self, command: &mut Command) {
        command
            .env("GIT_DIR", &self.git_dir)
            .env("GIT_WORK_TREE", &self.worktree)
            .env("GIT_INDEX_FILE", &self.index);
    }

    fn assert_unchanged(&self) {
        assert_eq!(
            git_stdout(&self.root, &["config", "--get", "core.bare"]),
            "false"
        );
        assert_eq!(
            git_stdout(&self.root, &["config", "--get", "user.name"]),
            ORIGINAL_NAME
        );
        assert_eq!(
            git_stdout(&self.root, &["config", "--get", "user.email"]),
            ORIGINAL_EMAIL
        );
    }
}

#[test]
fn rust_project_builder_child() {
    if std::env::var_os(RUST_CHILD).is_none() {
        return;
    }
    let env = TestEnv::isolated();
    let _project = env.project().git().build();
}

#[test]
fn rust_fixture_git_environment_child() {
    if std::env::var_os(ENVIRONMENT_CHILD).is_none() {
        return;
    }
    let output =
        PathBuf::from(std::env::var_os(ENVIRONMENT_OUTPUT).expect("the environment output path"));
    let env = TestEnv::isolated();
    git(
        &env,
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[&output.display().to_string()],
    );
}

#[test]
fn rust_project_builder_does_not_inherit_git_targeting() {
    let source = PoisonedRepository::new();
    let mut child = Command::new(std::env::current_exe().expect("locating this test binary"));
    child.args(["--exact", "rust_project_builder_child", "--nocapture"]);
    child.env(RUST_CHILD, "1");
    source.poison(&mut child);
    let output = child.output().expect("running Rust fixture child");
    assert!(
        output.status.success(),
        "the poisoned Rust fixture must still build:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    source.assert_unchanged();
}

#[test]
fn shell_project_builder_does_not_inherit_git_targeting() {
    let source = PoisonedRepository::new();
    let library = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/story/tests/lib.sh");
    let mut child = Command::new("bash");
    child
        .args([
            "-c",
            ". \"$1\"; mk_story_repo >/dev/null",
            "sh572-shell-fixture",
            &library.display().to_string(),
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"));
    source.poison(&mut child);
    let output = child.output().expect("running shell fixture child");
    assert!(
        output.status.success(),
        "the poisoned shell fixture must still build:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    source.assert_unchanged();
}

#[test]
fn rust_and_shell_fixture_git_build_the_same_child_environment() {
    let fixture = scratch_dir();
    let shim = fixture.path().join("bin");
    std::fs::create_dir(&shim).expect("creating Git shim directory");
    let fake_git = shim.join("git");
    std::fs::write(&fake_git, "#!/bin/sh\n/usr/bin/env >\"$1\"\n")
        .expect("writing Git environment probe");
    std::fs::set_permissions(&fake_git, std::fs::Permissions::from_mode(0o755))
        .expect("making Git environment probe executable");
    let path = std::env::join_paths(std::iter::once(shim.clone()).chain(std::env::split_paths(
        &std::env::var_os("PATH").expect("PATH is set"),
    )))
    .expect("building probe PATH");

    let rust_output = fixture.path().join("rust-environment");
    let mut rust_child = Command::new(std::env::current_exe().expect("locating this test binary"));
    rust_child
        .args([
            "--exact",
            "rust_fixture_git_environment_child",
            "--nocapture",
        ])
        .env(ENVIRONMENT_CHILD, "1")
        .env(ENVIRONMENT_OUTPUT, &rust_output)
        .env("PATH", &path)
        .env("GIT_DIR", "/smuggled/rust/git-dir")
        .env("GIT_AUTHOR_NAME", "smuggled author");
    let output = rust_child
        .output()
        .expect("running Rust Git environment probe");
    assert!(
        output.status.success(),
        "Rust Git environment probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rust_environment = environment(&rust_output);

    let shell_output = fixture.path().join("shell-environment");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/test-env.sh");
    let mut shell = Command::new("bash");
    shell
        .args([
            "-c",
            ". \"$1\"; storyhook_fixture_git \"$2\"",
            "sh572-environment-probe",
            &script.display().to_string(),
            &shell_output.display().to_string(),
        ])
        .env_clear()
        .envs(&rust_environment)
        .env("GIT_DIR", "/smuggled/shell/git-dir")
        .env("GIT_AUTHOR_NAME", "smuggled author");
    let output = shell.output().expect("running shell Git environment probe");
    assert!(
        output.status.success(),
        "shell Git environment probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(environment(&shell_output), rust_environment);
    assert!(!rust_environment.contains_key("GIT_DIR"));
    assert!(!rust_environment.contains_key("GIT_AUTHOR_NAME"));
}

#[test]
fn shared_rust_fixture_support_has_one_git_constructor() {
    let files = tracked_files(&[
        "crates/storyhook-test-support/src/*.rs",
        "crates/storyhook-test-support/src/**/*.rs",
    ]);
    assert!(
        files.len() >= 10,
        "fixture-support scan found only {files:?}"
    );
    let marker = format!("Command::new(\"{}\")", "git");
    let offenders: Vec<String> = files
        .into_iter()
        .filter(|(_, text)| without_rust_comments(text).contains(&marker))
        .map(|(relative, _)| relative)
        .collect();
    assert!(
        offenders.is_empty(),
        "{offenders:?} construct ambient Git directly; use the production Git environment constructor"
    );
    assert!(format!("let command = {marker};").contains(&marker));
}

#[test]
fn every_plugin_test_reaches_git_through_the_shared_library() {
    let files = tracked_files(&["plugins/story/tests/test-*.sh"]);
    assert!(
        files.len() >= 40,
        "plugin fixture scan found only {files:?}"
    );
    let offenders: Vec<String> = files
        .into_iter()
        .filter(|(_, text)| !text.lines().any(|line| line.contains("lib.sh\"")))
        .map(|(relative, _)| relative)
        .collect();
    assert!(
        offenders.is_empty(),
        "{offenders:?} bypass plugins/story/tests/lib.sh and its isolated Git constructor"
    );

    let e2e = tracked_files(&["scripts/run-e2e.sh"]);
    assert_eq!(e2e.len(), 1, "the E2E fixture scan must find its harness");
    let raw_git: Vec<&str> = e2e[0]
        .1
        .lines()
        .map(str::trim_start)
        .filter(|line| line.starts_with("git "))
        .collect();
    assert!(
        raw_git.is_empty(),
        "run-e2e.sh builds fixture repositories with ambient Git: {raw_git:?}"
    );
    assert!("git init -q -b main".trim_start().starts_with("git "));
}
