//! `story project set-prefix` — SH-109's item 3, the command that makes a
//! story-id prefix rename safe.
//!
//! Structured like `tests/project_delete.rs`: most of what is here is about
//! the confirmation gate, since the rewrite itself is proven at the store
//! level in `tests/service_project_set_prefix.rs`. The confirmation is a
//! **typed prefix**, not `[y/N]` — the same weight `delete` and `purge` use
//! for a one-way door, and this is one: every id already written down
//! anywhere under the old prefix stops resolving the moment it commits.

use std::path::Path;

use assert_cmd::assert::OutputAssertExt;
use storyhook_test_support::TestEnv;

/// A project with `stories` stories in it, at `<home>/<name>`.
fn project_with(env: &TestEnv, name: &str, prefix: &str, stories: usize) -> std::path::PathBuf {
    let dir = env.home().join(name);
    std::fs::create_dir_all(&dir).expect("creating the repository");
    env.story(&dir)
        .args(["project", "new", "--prefix", prefix])
        .assert()
        .success();
    for index in 1..=stories {
        env.story(&dir)
            .args(["new", &format!("Story {index}")])
            .assert()
            .success();
    }
    dir
}

/// Runs `story project set-prefix …` with stdin closed — no terminal, which
/// is what every test process has anyway.
fn set_prefix(env: &TestEnv, cwd: &Path, args: &[&str]) -> std::process::Output {
    let mut cmd = env.story(cwd);
    cmd.args(["project", "set-prefix"]);
    cmd.args(args);
    cmd.output().expect("running story project set-prefix")
}

fn set_prefix_by_slug(
    env: &TestEnv,
    cwd: &Path,
    slug: &str,
    args: &[&str],
) -> std::process::Output {
    let mut cmd = env.story(cwd);
    cmd.args(["--project", slug, "project", "set-prefix"]);
    cmd.args(args);
    cmd.output().expect("running story project set-prefix")
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn show(env: &TestEnv, cwd: &Path, id: &str) -> std::process::Output {
    env.story(cwd)
        .args(["show", id])
        .output()
        .expect("running story show")
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

#[test]
fn without_force_and_without_a_terminal_it_refuses_and_names_the_flag() {
    let env = TestEnv::isolated();
    let dir = project_with(&env, "repo", "SH", 2);

    let out = set_prefix(&env, &dir, &["AGE"]);

    assert_eq!(
        out.status.code(),
        Some(2),
        "a refusal is a usage error; stdout={} stderr={}",
        stdout(&out),
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("--force"),
        "the refusal must name the way past it: {}",
        stderr(&out)
    );
    // Refused, so the old prefix's ids still resolve and the new one's do not.
    show(&env, &dir, "SH-1").assert().success();
}

#[test]
fn the_refusal_says_what_would_change() {
    let env = TestEnv::isolated();
    let dir = project_with(&env, "repo", "SH", 2);

    let out = set_prefix(&env, &dir, &["AGE"]);
    let stderr = stderr(&out);

    assert!(stderr.contains("SH"), "the old prefix is named: {stderr}");
    assert!(stderr.contains("AGE"), "the new prefix is named: {stderr}");
    assert!(stderr.contains('2'), "the story count is stated: {stderr}");
}

#[test]
fn json_without_force_refuses_rather_than_prompting_into_the_stream() {
    let env = TestEnv::isolated();
    let dir = project_with(&env, "repo", "SH", 1);

    let out = set_prefix(&env, &dir, &["AGE", "--json"]);

    assert_eq!(out.status.code(), Some(2));
    let stdout = stdout(&out);
    assert!(
        stdout.trim().is_empty() || serde_json::from_str::<serde_json::Value>(&stdout).is_ok(),
        "stdout must stay machine-readable: {stdout}"
    );
}

/// The same wedge `tests/project_delete.rs` guards against: a `forced()` that
/// forgot `SetPrefix` would loop the two-step forever with no error and no
/// failing assertion, only a command that never returns.
#[test]
fn the_two_step_round_trip_completes_in_one_cycle() {
    let env = TestEnv::isolated();
    let dir = project_with(&env, "repo", "SH", 1);

    let started = std::time::Instant::now();
    let refused = set_prefix(&env, &dir, &["AGE"]);
    assert_eq!(refused.status.code(), Some(2));
    let forced = set_prefix(&env, &dir, &["AGE", "--force"]);
    let elapsed = started.elapsed();

    assert_eq!(forced.status.code(), Some(0), "{}", stderr(&forced));
    assert!(
        elapsed < std::time::Duration::from_secs(60),
        "the refusal and the forced retry together took {elapsed:?}; a confirmation that \
         re-asks forever looks exactly like this"
    );
    show(&env, &dir, "AGE-1").assert().success();
}

#[test]
fn set_prefix_must_not_read_stdin_before_it_prompts() {
    use storyhook::cli::{Invocation, ProjectAction};

    for force in [false, true] {
        assert!(
            !storyhook::invoke::reads_stdin(&Invocation::Project {
                action: ProjectAction::SetPrefix {
                    new_prefix: "AGE".to_string(),
                    force,
                },
            }),
            "the prompt needs the terminal that `reads_stdin` would consume"
        );
    }
}

// ---------------------------------------------------------------------------
// The rewrite
// ---------------------------------------------------------------------------

#[test]
fn force_rewrites_the_prefix_and_every_relationship() {
    let env = TestEnv::isolated();
    let dir = project_with(&env, "repo", "SH", 2);
    env.story(&dir)
        .args(["relate", "SH-1", "blocks", "SH-2"])
        .assert()
        .success();

    let out = set_prefix(&env, &dir, &["AGE", "--force"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));

    // The old ids are gone.
    show(&env, &dir, "SH-1").assert().failure();
    show(&env, &dir, "SH-2").assert().failure();

    // The new ones resolve, and both directions of the relationship read
    // under the new prefix.
    show(&env, &dir, "AGE-1")
        .assert()
        .success()
        .stdout(predicates::str::contains("AGE-2"));
    show(&env, &dir, "AGE-2")
        .assert()
        .success()
        .stdout(predicates::str::contains("AGE-1"));

    // And the rename itself did not corrupt anything an ordinary write would
    // trip over — the exact failure mode SH-109 was filed about.
    env.story(&dir)
        .args(["comment", "AGE-1", "still writable after the rename"])
        .assert()
        .success();
}

#[test]
fn a_neighbouring_project_is_untouched() {
    let env = TestEnv::isolated();
    let renamed = project_with(&env, "renamed", "SH", 1);
    let kept = project_with(&env, "kept", "KP", 1);

    set_prefix(&env, &renamed, &["AGE", "--force"]);

    show(&env, &kept, "KP-1").assert().success();
}

#[test]
fn set_prefix_refuses_a_prefix_already_used_by_another_project() {
    let env = TestEnv::isolated();
    let one = project_with(&env, "one", "ONE", 0);
    project_with(&env, "two", "TWO", 0);

    let out = set_prefix(&env, &one, &["TWO", "--force"]);

    assert_ne!(out.status.code(), Some(0));
    let stderr = stderr(&out);
    assert!(stderr.contains("TWO"), "{stderr}");
    assert!(
        stderr.contains("two"),
        "names the project that holds it: {stderr}"
    );
}

#[test]
fn set_prefix_refuses_a_no_op_rename() {
    let env = TestEnv::isolated();
    let dir = project_with(&env, "repo", "SH", 1);

    let out = set_prefix(&env, &dir, &["SH", "--force"]);

    assert_ne!(out.status.code(), Some(0));
    assert!(stderr(&out).contains("already has"), "{}", stderr(&out));
}

#[test]
fn set_prefix_refuses_an_invalid_prefix() {
    let env = TestEnv::isolated();
    let dir = project_with(&env, "repo", "SH", 1);

    let out = set_prefix(&env, &dir, &["not valid", "--force"]);

    assert_ne!(out.status.code(), Some(0));
    show(&env, &dir, "SH-1").assert().success();
}

// ---------------------------------------------------------------------------
// Naming the project, and failing to
// ---------------------------------------------------------------------------

#[test]
fn set_prefix_reaches_a_project_by_slug_from_elsewhere() {
    let env = TestEnv::isolated();
    let there = project_with(&env, "there", "SH", 1);
    let here = project_with(&env, "here", "HR", 0);

    let out = set_prefix_by_slug(&env, &here, "there", &["AGE", "--force"]);

    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    show(&env, &there, "AGE-1").assert().success();
}

#[test]
fn set_prefix_in_a_directory_with_no_project_refuses_the_way_every_scoped_verb_does() {
    let env = TestEnv::isolated();
    let bare = env.home().join("bare");
    std::fs::create_dir_all(&bare).unwrap();

    let out = set_prefix(&env, &bare, &["AGE", "--force"]);

    assert_eq!(
        out.status.code(),
        Some(3),
        "the same not-found every scoped verb answers with; stderr={}",
        stderr(&out)
    );
    let stderr = stderr(&out);
    assert!(
        stderr.contains("--project"),
        "the refusal must name the way to select one: {stderr}"
    );
}

#[test]
fn set_prefix_requires_exactly_one_positional() {
    let env = TestEnv::isolated();
    let dir = project_with(&env, "repo", "SH", 1);

    let missing = set_prefix(&env, &dir, &[]);
    assert_eq!(
        missing.status.code(),
        Some(2),
        "no positional is a usage error; stderr={}",
        stderr(&missing)
    );

    let extra = set_prefix(&env, &dir, &["AGE", "EXTRA", "--force"]);
    assert_eq!(
        extra.status.code(),
        Some(2),
        "a second positional is a usage error; stderr={}",
        stderr(&extra)
    );

    show(&env, &dir, "SH-1").assert().success();
}
