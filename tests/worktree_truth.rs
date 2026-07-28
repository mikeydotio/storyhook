//! The headline regression of the data-layer rearchitecture: **two checkouts of
//! one repository must share one truth.**
//!
//! Storyhook keeps its state in `.storyhook/`, a *version-controlled* directory
//! (`src/app.rs`: "It must be committed to git so that project state travels
//! with the repository"). That single decision is the root of SH-46's
//! second-order failure. A git worktree is a second checkout of the same
//! repository, so it gets its own copy of `.storyhook/` — including its own
//! `next-id` counter and its own story files. Two checkouts are therefore two
//! independent databases that happen to share a git history:
//!
//! - a story created in one checkout does not exist in the other, and
//! - two concurrent `story new` calls both mint the *same* id.
//!
//! `ensure_project` (`src/storage.rs:318`) does not walk up ancestors — the
//! project root is exactly `env::current_dir()` — so nothing rescues this at
//! read time. `plugin/claude-code/tests/test-dispatch-cwd.sh` works around it
//! in the dispatch layer by anchoring every `story` call at the *main* repo;
//! that fix cannot reach a human (or agent) who simply runs `story new` while
//! standing in a worktree.
//!
//! Both tests below assert the world the rearchitecture is *for*: one global
//! store, one id space, one truth per project regardless of checkout. They are
//! `#[ignore]`d because they are red today and the quality gate must stay
//! green. **Removing the two `#[ignore]` attributes is W4's exit criterion.**
//!
//! Captured red output, `cargo test --workspace --test worktree_truth -- --ignored`
//! at commit `6320609`:
//!
//! ```text
//! ---- two_worktrees_of_one_repo_mint_colliding_ids stdout ----
//! assertion `left != right` failed: two checkouts of one repository must not
//! mint the same story id; both `story new` calls returned SH-2. Each checkout
//! carries its own committed copy of .storyhook/next-id, so each incremented a
//! private counter.
//!   left: "SH-2"
//!  right: "SH-2"
//!
//! ---- a_story_created_in_one_checkout_is_visible_from_the_other stdout ----
//! assertion failed: a story created in worktree `a` must be visible from
//! worktree `b` — they are one project. `story show SH-2` in b exited with
//! code 3 and said: error: story `SH-2` not found
//! ```
//!
//! The two ids collide at `SH-2` rather than `SH-1` because the fixture seeds
//! one story before splitting, which proves the checkouts start from a *shared*
//! counter and then diverge — the collision is drift, not a fixture that never
//! agreed in the first place.

use std::path::Path;
use std::process::Command;

use storyhook_test_support::{Project, TestEnv};

/// Builds one repository with two linked worktrees, each holding its own
/// committed copy of `.storyhook/` — the exact shape a real storyhook repo has
/// when `story.sh dispatch` opens a second working session.
///
/// [`storyhook_test_support::ProjectBuilder`] leaves `.storyhook/` untracked, so
/// this commits it and fast-forwards both worktrees onto that commit. That step
/// is what makes the fixture faithful: without it the worktrees resolve *no*
/// tracker at all (a different failure — exit 3, "not initialized") rather than
/// a silently divergent one.
fn two_checkouts_of_one_repo() -> Project<'static> {
    let env = TestEnv::shared();
    let project = env.project().git().worktree("a").worktree("b").build();

    // A story that predates the split. Both checkouts inherit it, so any later
    // divergence is provably drift rather than fixtures that never agreed.
    project.new_story("Created before the checkouts diverged");

    git(env, project.path(), &["add", ".storyhook"]);
    git(env, project.path(), &["commit", "-qm", "track the tracker"]);
    for name in ["a", "b"] {
        git(
            env,
            project.worktree_path(name),
            &["merge", "-q", "--ff-only", "main"],
        );
    }

    for name in ["a", "b"] {
        assert!(
            project
                .worktree_path(name)
                .join(".storyhook/next-id")
                .exists(),
            "fixture: worktree `{name}` must carry its own copy of .storyhook/ — \
             without it this file tests the wrong failure"
        );
    }
    project
}

/// Runs `git <args>` in `cwd` under `env`, asserting success.
fn git(env: &TestEnv, cwd: &Path, args: &[&str]) {
    let mut cmd = Command::new("git");
    cmd.current_dir(cwd);
    env.apply(&mut cmd);
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    let out = cmd.args(args).output().expect("running git");
    assert!(
        out.status.success(),
        "`git {}` in {} failed: {}",
        args.join(" "),
        cwd.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Two checkouts of one repository must never mint the same story id.
///
/// The name records the *defect* (that is what a future reader greps for); the
/// assertion states the world we want. Red today: see this file's header for
/// the captured failure.
#[test]
#[ignore = "SH-46: two checkouts are separate databases; goes green at the W4 flip"]
fn two_worktrees_of_one_repo_mint_colliding_ids() {
    let env = TestEnv::shared();
    let project = two_checkouts_of_one_repo();

    // Both processes must be *spawned* before either is waited on, or the
    // second one reads a counter the first has already advanced and the race
    // this test exists to lose never happens.
    let a = env
        .raw_story(project.worktree_path("a"))
        .args(["new", "Minted in worktree a", "--json"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawning `story new` in worktree a");
    let b = env
        .raw_story(project.worktree_path("b"))
        .args(["new", "Minted in worktree b", "--json"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawning `story new` in worktree b");

    let id_a = minted_id(a.wait_with_output().expect("waiting on worktree a"), "a");
    let id_b = minted_id(b.wait_with_output().expect("waiting on worktree b"), "b");

    assert_ne!(
        id_a, id_b,
        "two checkouts of one repository must not mint the same story id; both \
         `story new` calls returned {id_a}. Each checkout carries its own committed \
         copy of .storyhook/next-id, so each incremented a private counter."
    );
}

/// A story created in one checkout must be readable from the other — they are
/// one project, not two.
///
/// The companion to the id collision: even without a race, the two checkouts
/// disagree about what exists.
#[test]
#[ignore = "SH-46: two checkouts are separate databases; goes green at the W4 flip"]
fn a_story_created_in_one_checkout_is_visible_from_the_other() {
    let env = TestEnv::shared();
    let project = two_checkouts_of_one_repo();

    let created = env
        .story(project.worktree_path("a"))
        .args(["new", "Created in worktree a", "--json"])
        .output()
        .expect("running `story new` in worktree a");
    let id = minted_id(created, "a");

    let seen = env
        .story(project.worktree_path("b"))
        .args(["show", &id])
        .output()
        .expect("running `story show` in worktree b");
    assert!(
        seen.status.success(),
        "a story created in worktree `a` must be visible from worktree `b` — they \
         are one project. `story show {id}` in b exited with code {} and said: {}",
        seen.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&seen.stderr).trim()
    );
}

/// Extracts the id from a `story new --json` run, failing loudly with the
/// child's own diagnostics if it did not succeed.
fn minted_id(out: std::process::Output, checkout: &str) -> String {
    assert!(
        out.status.success(),
        "`story new --json` in worktree `{checkout}` failed ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "`story new --json` in worktree `{checkout}` did not print JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    value["story"]["story"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no id in `story new --json` output from worktree `{checkout}`"))
        .to_string()
}
