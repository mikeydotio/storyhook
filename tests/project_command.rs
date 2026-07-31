//! `story project init` and `story project list`.
//!
//! The lifecycle verbs became a group because all three of them — init, deinit,
//! list — are about the *project* rather than about a story, and because the
//! two that change something needed the same thing: a way to name a checkout
//! other than "wherever you happen to be standing".
//!
//! The path argument is what most of this file is about. It is carried as text
//! and resolved against the **client's** working directory, which is the only
//! interesting part: over the daemon the process doing the work has a working
//! directory of its own — wherever it was spawned, often `/` — so a relative
//! path resolved there would create a project somewhere nobody named.

use std::path::Path;

use storyhook_test_support::TestEnv;

/// Runs `story project …` in `cwd` and returns the assertion handle.
fn project(env: &TestEnv, cwd: &Path, args: &[&str]) -> assert_cmd::assert::Assert {
    let mut cmd = env.story(cwd);
    cmd.arg("project");
    cmd.args(args);
    cmd.assert()
}

/// A directory inside the environment's scratch space, created but not
/// initialized.
fn bare_dir(env: &TestEnv, name: &str) -> std::path::PathBuf {
    let dir = env.home().join(name);
    std::fs::create_dir_all(&dir).expect("creating a bare directory");
    dir
}

// ---------------------------------------------------------------------------
// init
// ---------------------------------------------------------------------------

#[test]
fn project_init_creates_a_usable_project_in_the_working_directory() {
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "repo");

    project(&env, &dir, &["init"]).success();

    env.story(&dir).arg("summary").assert().success();
    env.story(&dir)
        .args(["new", "The first story"])
        .assert()
        .success()
        .stdout(predicates::str::contains("SH-1"));
    assert!(dir.join("AGENTS.md").exists(), "init generates AGENTS.md");
}

#[test]
fn project_init_takes_a_path_and_initializes_that_directory_not_the_cwd() {
    let env = TestEnv::isolated();
    let here = bare_dir(&env, "here");
    let there = bare_dir(&env, "there");

    project(&env, &here, &["init", there.to_str().unwrap()]).success();

    // The named directory is a project…
    env.story(&there).arg("summary").assert().success();
    assert!(there.join(".storyhook.toml").exists());
    // …and the one the command ran in is not.
    assert!(!here.join(".storyhook.toml").exists());
    env.story(&here).arg("summary").assert().failure();
}

#[test]
fn a_relative_path_resolves_against_the_directory_the_client_ran_in() {
    // The daemon's own working directory is wherever it was spawned. If this
    // resolved there, `story project init ./sub` would create a project at a
    // path nobody named — and the failure would be invisible, because a
    // project *would* appear, just not here.
    let env = TestEnv::isolated();
    let here = bare_dir(&env, "here");
    let sub = bare_dir(&env, "here/sub");

    project(&env, &here, &["init", "./sub"]).success();

    assert!(sub.join(".storyhook.toml").exists(), "created under `here`");
    env.story(&sub).arg("summary").assert().success();
}

#[test]
fn project_init_is_idempotent_and_does_not_rewind_the_story_counter() {
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "repo");
    project(&env, &dir, &["init"]).success();
    env.story(&dir).args(["new", "First"]).assert().success();

    project(&env, &dir, &["init"]).success();

    env.story(&dir)
        .args(["new", "Second"])
        .assert()
        .success()
        .stdout(predicates::str::contains("SH-2"));
}

#[test]
fn re_initializing_leaves_the_prefix_alone() {
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "repo");
    project(&env, &dir, &["init", "--prefix", "ZZ"]).success();

    project(&env, &dir, &["init", "--prefix", "QQ"]).success();

    env.story(&dir)
        .args(["new", "First"])
        .assert()
        .success()
        .stdout(predicates::str::contains("ZZ-1"));
}

#[test]
fn project_init_records_a_display_name_when_asked() {
    // `--name` had exactly one home before this: `web register --name`. The
    // catalog *is* the projects table, so a flag that is accepted and dropped
    // would be worse than one that does not exist.
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "repo");

    project(&env, &dir, &["init", "--name", "Nicely Named"]).success();

    project(&env, &dir, &["list"])
        .success()
        .stdout(predicates::str::contains("Nicely Named"));
}

#[test]
fn project_init_can_skip_the_agent_instructions() {
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "repo");

    project(&env, &dir, &["init", "--no-agents-md"]).success();

    assert!(!dir.join("AGENTS.md").exists());
}

#[test]
fn project_init_refuses_a_path_that_is_not_a_directory() {
    let env = TestEnv::isolated();
    let here = bare_dir(&env, "here");

    let assert = project(&env, &here, &["init", "./nope"]).failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr.contains("nope"),
        "the error names the path: {stderr}"
    );
}

#[test]
fn project_with_no_subcommand_prints_usage() {
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "repo");

    let assert = env.story(&dir).arg("project").assert().failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(stderr.contains("usage: story project"), "{stderr}");
}

#[test]
fn story_init_still_works_and_agrees_with_project_init() {
    // C2 adds the group without removing the old spelling; the removal is its
    // own commit so that a bisect can tell "the group is wrong" from "the
    // rename is wrong".
    let env = TestEnv::isolated();
    let old = bare_dir(&env, "old");
    let new = bare_dir(&env, "new");

    env.story(&old).args(["project", "init"]).assert().success();
    project(&env, &new, &["init"]).success();

    for dir in [&old, &new] {
        assert!(dir.join(".storyhook.toml").exists());
        assert!(dir.join("AGENTS.md").exists());
        env.story(dir).arg("summary").assert().success();
    }
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

#[test]
fn project_list_reports_every_project_with_its_checkout() {
    let env = TestEnv::isolated();
    let alpha = bare_dir(&env, "alpha");
    let beta = bare_dir(&env, "beta");
    project(&env, &alpha, &["init"]).success();
    project(&env, &beta, &["init"]).success();

    let assert = project(&env, &alpha, &["list"]).success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(stdout.contains("alpha"), "{stdout}");
    assert!(stdout.contains("beta"), "{stdout}");
    assert!(
        stdout.contains(alpha.to_str().unwrap()),
        "the checkout path is shown: {stdout}"
    );
}

#[test]
fn project_list_says_so_when_there_is_nothing_to_list() {
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "empty");

    let assert = project(&env, &dir, &["list"]).success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(!stdout.trim().is_empty(), "an empty catalog still answers");
}

#[test]
fn project_list_runs_outside_a_project() {
    // The catalog spans every project, so standing in one must not be a
    // precondition for reading it.
    let env = TestEnv::isolated();
    let inside = bare_dir(&env, "inside");
    let outside = bare_dir(&env, "outside");
    project(&env, &inside, &["init"]).success();

    project(&env, &outside, &["list"])
        .success()
        .stdout(predicates::str::contains("inside"));
}

#[test]
fn project_list_shows_a_project_whose_checkout_this_machine_does_not_have() {
    // The dashboard is about to serve these, and the CLI must be able to see
    // what the dashboard sees. A project with no checkout row is reachable
    // today by deleting the directory and running `story doctor --fix`.
    let env = TestEnv::isolated();
    let gone = bare_dir(&env, "gone");
    let here = bare_dir(&env, "here");
    project(&env, &gone, &["init"]).success();
    project(&env, &here, &["init"]).success();
    std::fs::remove_dir_all(&gone).expect("deleting the checkout");

    env.story(&here)
        .args(["doctor", "--fix"])
        .assert()
        .success();

    let assert = project(&env, &here, &["list"]).success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(
        stdout.contains("gone"),
        "a project with no checkout is still a project: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// The old spelling
// ---------------------------------------------------------------------------

#[test]
fn story_init_says_where_the_command_went() {
    // Not left to fall through to `unknown command`. Every shipped document,
    // this repo's own plugin skill, and every agent that has ever seen
    // storyhook say `story init`; telling all of them that no such command
    // exists is the least useful thing this binary could do.
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "repo");

    let out = env
        .story(&dir)
        .arg("init")
        .output()
        .expect("running the old spelling");

    assert_eq!(out.status.code(), Some(2), "a usage error");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("story project init"),
        "the error must name the new spelling: {stderr}"
    );
    assert!(
        !stderr.contains("unknown command"),
        "and must not pretend the command never existed: {stderr}"
    );
    assert!(
        !dir.join(".storyhook.toml").exists(),
        "a refused init writes nothing"
    );
}
