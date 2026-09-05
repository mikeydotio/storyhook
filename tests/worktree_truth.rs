//! The headline regression of the data-layer rearchitecture: **two checkouts of
//! one repository must share one truth.**
//!
//! Storyhook used to keep its state in `.storyhook/`, a *version-controlled*
//! directory, and that single decision was the root of SH-46's second-order
//! failure. A git worktree is a second checkout of the same repository, so it
//! got its own copy — including its own `next-id` counter and its own story
//! files. Two checkouts were therefore two independent databases that happened
//! to share a git history:
//!
//! - a story created in one checkout did not exist in the other, and
//! - two concurrent `story new` calls both minted the *same* id.
//!
//! Captured red output, `cargo test --test worktree_truth -- --ignored` at
//! commit `6320609`, before the flip:
//!
//! ```text
//! ---- two_worktrees_of_one_repo_mint_colliding_ids stdout ----
//! assertion `left != right` failed: two checkouts of one repository must not
//! mint the same story id; both `story new` calls returned SH-2.
//!   left: "SH-2"
//!  right: "SH-2"
//! ```
//!
//! # Why this file was rewritten (SH-121, C10 of the server-owned epic SH-112)
//!
//! The assertions below are the ones written against that failing behaviour.
//! **The fixture is what changed, and it had to.**
//!
//! At the flip, the fixture kept the shape of the defect and swapped what a
//! checkout commits: `.storyhook.toml` instead of the whole tracker. Each
//! worktree then carried a copy of the pointer file, and resolution — which
//! reads the pointer in the working directory first — answered from that copy.
//! So did every other checkout in the fixture. That is a green test proving
//! **two directories holding the same three lines of TOML agree**, which is a
//! fact about `cp`, and it stayed green through SH-119 deleting the
//! resolution index underneath it without ever executing the new path.
//! `invoker_seam.rs::two_checkouts_of_one_project_resolve_to_the_same_project`
//! already makes exactly that claim, in-process, for a tenth of the cost.
//!
//! Measured rather than reasoned about: a whole-suite trace of which step
//! answered every project resolution found **892 of 963 resolved by a pointer
//! file in the working directory itself, and 5 by a registered origin** — both
//! of this file's worktrees among the 892.
//!
//! # What the fixture is now
//!
//! A `git clone` of the project's origin, with its worktrees inside it. Nothing
//! in that tree carries a pointer file and nothing above it does either
//! ([`assert_selection_is_not_inherited`] is what says so, per worktree), so
//! **the registered origin is the only thing that can answer** — which is the
//! mechanism the epic states: *a second checkout of the same origin is the same
//! project, so linked worktrees resolve identically to their main tree by
//! construction, with no runtime git walk and no worktree bookkeeping.*
//!
//! The clone is also the honest shape. It is what a second machine, or a second
//! working copy, actually has: the pointer file is written by `story project
//! new` after the push that created the origin, so it never travels.
//!
//! `the_origin_is_what_answers_and_nothing_else_is` is the guard that keeps this
//! file from decaying back into the last one. It breaks the mechanism —
//! `story project unlink origin` — and requires the answer to disappear.

use storyhook_test_support::{
    ChildGuard, Project, STORY_COMMAND_DEADLINE, SecondCheckout, TestEnv,
    assert_selection_is_not_inherited,
};

/// One repository with a registered origin, and a second checkout of it
/// carrying two linked worktrees.
///
/// The seed story predates the clone, so any later divergence is provably drift
/// rather than fixtures that never agreed — and it is why the ids below collide
/// at `SH-2` rather than `SH-1`.
fn two_checkouts_of_one_repository<'a>(env: &'a TestEnv) -> (Project<'a>, SecondCheckout<'a>) {
    let project = env.project().with_local_origin().build();
    project.new_story("Created before the checkouts diverged");
    let second = project
        .second_checkout()
        .with_worktree("a")
        .with_worktree("b");
    (project, second)
}

/// Two checkouts of one repository must never mint the same story id.
///
/// The name records the *defect* (that is what a future reader greps for); the
/// assertion states the world we want. See this file's header for the captured
/// failure it was written against.
#[test]
fn two_worktrees_of_one_repo_mint_colliding_ids() {
    let env = TestEnv::shared();
    let (_project, second) = two_checkouts_of_one_repository(env);

    // Both processes must be *spawned* before either is waited on, or the
    // second one reads a counter the first has already advanced and the race
    // this test exists to lose never happens.
    let mut a_command = env.raw_story(second.worktree_path("a"));
    a_command.args(["new", "Minted in worktree a", "--json"]);
    let mut a =
        ChildGuard::spawn_with_output(&mut a_command).expect("spawning `story new` in worktree a");
    let mut b_command = env.raw_story(second.worktree_path("b"));
    b_command.args(["new", "Minted in worktree b", "--json"]);
    let mut b =
        ChildGuard::spawn_with_output(&mut b_command).expect("spawning `story new` in worktree b");

    let id_a = minted_id(
        a.wait_with_output_within(STORY_COMMAND_DEADLINE, || {
            "`story new` in worktree a did not finish".to_string()
        }),
        "a",
    );
    let id_b = minted_id(
        b.wait_with_output_within(STORY_COMMAND_DEADLINE, || {
            "`story new` in worktree b did not finish".to_string()
        }),
        "b",
    );

    assert_ne!(
        id_a, id_b,
        "two checkouts of one repository must not mint the same story id; both `story new` \
         calls returned {id_a}"
    );
}

/// A story created in one checkout must be readable from the other — they are
/// one project, not two.
///
/// The companion to the id collision: even without a race, the two checkouts
/// disagree about what exists.
#[test]
fn a_story_created_in_one_checkout_is_visible_from_the_other() {
    let env = TestEnv::shared();
    let (_project, second) = two_checkouts_of_one_repository(env);

    let created = env
        .story(second.worktree_path("a"))
        .args(["new", "Created in worktree a", "--json"])
        .output()
        .expect("running `story new` in worktree a");
    let id = minted_id(created, "a");

    let seen = env
        .story(second.worktree_path("b"))
        .args(["show", &id])
        .output()
        .expect("running `story show` in worktree b");
    assert!(
        seen.status.success(),
        "a story created in worktree `a` must be visible from worktree `b` — they are one \
         project. `story show {id}` in b exited with code {} and said: {}",
        seen.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&seen.stderr).trim()
    );
}

/// The case SH-121 names: a linked worktree and its main tree, across two
/// repositories on disk that share nothing but an origin URL.
///
/// Stronger than the pair above, and the reason it is separate: worktrees `a`
/// and `b` are two directories inside one clone, and could conceivably agree
/// through something local to it. The clone and the original are *different
/// repositories*, cloned before either knew about the other's stories, and the
/// only fact they hold in common is `remote.origin.url`.
#[test]
fn a_linked_worktree_answers_for_its_main_trees_project() {
    let env = TestEnv::shared();
    let (project, second) = two_checkouts_of_one_repository(env);

    let created = env
        .story(second.worktree_path("a"))
        .args(["new", "Created in a worktree of the clone", "--json"])
        .output()
        .expect("running `story new` in the clone's worktree");
    let id = minted_id(created, "a");

    let seen = project
        .story()
        .args(["show", &id])
        .output()
        .expect("running `story show` in the original checkout");
    assert!(
        seen.status.success(),
        "the original checkout must see a story its clone's worktree created — one origin is \
         one project. `story show {id}` exited {} and said: {}",
        seen.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&seen.stderr).trim()
    );
}

/// **AC-1, encoded rather than performed by hand.** Break the origin
/// registration and the answer must go away.
///
/// Without this, every assertion above could be satisfied by some mechanism
/// nobody meant — a path index quietly reintroduced, a pointer file a fixture
/// started committing — and the file would be back to proving nothing. The
/// registration is the *only* thing removed, and the refusal it leaves behind
/// is the one an unresolvable directory always gets.
///
/// Isolated rather than shared: unlinking an origin is a fact about the store
/// that every sibling test in this binary would also see.
#[test]
fn the_origin_is_what_answers_and_nothing_else_is() {
    let env = TestEnv::isolated();
    let (project, second) = two_checkouts_of_one_repository(&env);
    let worktree = second.worktree_path("a");

    // The precondition, asserted rather than assumed: it has to answer *before*
    // the break, or the assertion after it passes for free.
    env.story(worktree).args(["list"]).assert().success();
    assert_selection_is_not_inherited(worktree);

    project
        .story()
        .args(["project", "unlink", "origin"])
        .assert()
        .success();

    let out = env
        .story(worktree)
        .args(["list"])
        .output()
        .expect("running story");
    assert_eq!(
        out.status.code(),
        Some(3),
        "with the origin unregistered the worktree must refuse, not answer. stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--project"),
        "and the refusal must still name a way out: {stderr}"
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
