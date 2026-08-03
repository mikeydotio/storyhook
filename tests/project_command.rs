//! `story project new`'s filesystem side, `story project list`, and the three
//! redirects.
//!
//! The lifecycle verbs are a group because all of them are about the *project*
//! rather than about a story. `tests/project_new.rs` owns the grammar — which
//! flags are required, what a bare word does, what `--attach` and `--no-attach`
//! mean. What is here is the rest: what a created project records, what
//! `project list` shows, and what the retired spellings say now.
//!
//! # The redirects, and why they are tested rather than deleted
//!
//! `story init`, `story project init`, `story project deinit` and `story
//! relink` are gone as commands and kept as signposts. Five years of documents,
//! this repo's own plugin skill and every agent that has ever seen storyhook
//! say some of those words; the least useful thing this binary could do is tell
//! them no such command exists. Each redirect is asserted to name its
//! replacement *and* not to say "unknown command" — the second half is what
//! stops somebody deleting the arm as dead code.

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
// what a created project records
// ---------------------------------------------------------------------------

/// Everything `story project list` printed for the project rooted at `dir`.
fn listing(env: &TestEnv, dir: &Path) -> String {
    let out = env
        .story(dir)
        .args(["project", "list"])
        .output()
        .expect("running `story project list`");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Attaching a checkout records it in `projects.checkout_path` as well as in
/// the resolution index. Two facts about one directory, and SH-119 collapses
/// them — a test that watched only one would not notice the wrong one going.
#[test]
fn creating_a_project_records_the_directory_as_its_checkout() {
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "repo");

    project(&env, &dir, &["new", "--prefix", "SH", "--no-agents-md"]).success();

    let canonical = dir.canonicalize().expect("canonicalizing the checkout");
    let listed = listing(&env, &dir);
    assert!(
        listed.contains(&format!("checkout  {}", canonical.display())),
        "`project list` must report the checkout that was attached:\n{listed}"
    );
}

/// The gap is filled, never the occupant evicted.
///
/// A checkout says where a project's repo-side work runs, and moving it is
/// `story project link checkout`'s job — the verb that reports what it
/// displaced. Attaching a second clone must not perform that move silently, or
/// somebody finds out weeks later that dispatch has been running in the wrong
/// tree.
#[test]
fn a_second_clone_does_not_steal_the_first_ones_checkout() {
    let env = TestEnv::isolated();
    let first = bare_dir(&env, "first");
    let second = bare_dir(&env, "second");
    project(&env, &first, &["new", "--prefix", "SH", "--no-agents-md"]).success();

    // A clone is a directory carrying the same committed pointer, which is the
    // route `new` adopts an existing project through.
    std::fs::copy(
        first.join(".storyhook.toml"),
        second.join(".storyhook.toml"),
    )
    .expect("copying the pointer file");
    project(&env, &second, &["new", "--prefix", "SH", "--no-agents-md"]).success();

    let listed = listing(&env, &first);
    let kept = first.canonicalize().expect("canonicalizing the first");
    let stolen = second.canonicalize().expect("canonicalizing the second");
    assert!(
        listed.contains(&format!("checkout  {}", kept.display())),
        "the first checkout must survive the second clone:\n{listed}"
    );
    assert!(
        !listed.contains(&format!("checkout  {}", stolen.display())),
        "the second clone must not have taken the checkout slot:\n{listed}"
    );
}

/// A prefix is fixed at creation. Running `new` again in a checkout that
/// already belongs to a project re-registers it and leaves the prefix alone —
/// so a second `--prefix` is ignored rather than silently re-minting every
/// future id under a different name.
#[test]
fn creating_again_leaves_the_prefix_alone() {
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "repo");
    project(&env, &dir, &["new", "--prefix", "ZZ"]).success();

    project(&env, &dir, &["new", "--prefix", "QQ"]).success();

    env.story(&dir)
        .args(["new", "First"])
        .assert()
        .success()
        .stdout(predicates::str::contains("ZZ-1"));
}

#[test]
fn project_new_records_a_display_name_when_asked() {
    // `--name` had exactly one home before this: `web register --name`. The
    // catalog *is* the projects table, so a flag that is accepted and dropped
    // would be worse than one that does not exist.
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "repo");

    project(
        &env,
        &dir,
        &["new", "--prefix", "SH", "--name", "Nicely Named"],
    )
    .success();

    project(&env, &dir, &["list"])
        .success()
        .stdout(predicates::str::contains("Nicely Named"));
}

#[test]
fn project_new_can_skip_the_agent_instructions() {
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "repo");

    project(&env, &dir, &["new", "--prefix", "SH", "--no-agents-md"]).success();

    assert!(!dir.join("AGENTS.md").exists());
}

#[test]
fn project_new_refuses_an_attach_target_that_is_not_a_directory() {
    let env = TestEnv::isolated();
    let here = bare_dir(&env, "here");

    let assert = project(
        &env,
        &here,
        &["new", "--prefix", "SH", "--attach", "./nope"],
    )
    .failure();
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

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

#[test]
fn project_list_reports_every_project_with_its_checkout() {
    let env = TestEnv::isolated();
    let alpha = bare_dir(&env, "alpha");
    let beta = bare_dir(&env, "beta");
    project(&env, &alpha, &["new", "--prefix", "AL"]).success();
    project(&env, &beta, &["new", "--prefix", "BE"]).success();

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
    project(&env, &inside, &["new", "--prefix", "IN"]).success();

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
    project(&env, &gone, &["new", "--prefix", "GO"]).success();
    project(&env, &here, &["new", "--prefix", "HE"]).success();
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
// The retired spellings
// ---------------------------------------------------------------------------

/// Every redirect: the exact tokens someone types, what each must name, and the
/// one thing none of them may say.
///
/// A table rather than four near-identical tests, because the property is the
/// same for all of them and the fourth would have been the one somebody forgot
/// to write. `unknown command` is asserted *against* on purpose — it is what
/// each of these produces the moment its arm is deleted as dead code, and it is
/// the failure this whole design is about avoiding.
#[test]
fn every_retired_verb_names_the_command_that_replaced_it() {
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "repo");

    for (typed, replacement) in [
        (vec!["init"], "story project new"),
        (vec!["project", "init"], "story project new"),
        (vec!["project", "deinit"], "story project delete"),
        (vec!["relink"], "story project link checkout"),
    ] {
        let out = env
            .story(&dir)
            .args(&typed)
            .output()
            .expect("running a retired spelling");
        let stderr = String::from_utf8_lossy(&out.stderr);
        let typed = typed.join(" ");

        assert_eq!(
            out.status.code(),
            Some(2),
            "`story {typed}` must be a usage error; stderr={stderr}"
        );
        assert!(
            stderr.contains(replacement),
            "`story {typed}` must name `{replacement}`: {stderr}"
        );
        assert!(
            !stderr.contains("unknown command"),
            "`story {typed}` must not pretend the command never existed: {stderr}"
        );
    }

    assert!(
        !dir.join(".storyhook.toml").exists(),
        "and no redirect creates anything on its way to saying so"
    );
}

/// A redirect fires for the shape people actually type, not only the bare one.
///
/// SH-62's gate runs ahead of every parser and fails closed, so a retired verb
/// with no entry in the flag table answers "unknown flag `--prefix`" and the
/// redirect never runs. That is why both entries are still in `VERB_FLAGS`, and
/// this is the test that says so — it goes red the moment somebody tidies them
/// away.
#[test]
fn a_retired_verb_redirects_even_when_its_old_flags_are_passed() {
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "repo");

    for (typed, replacement) in [
        (
            vec!["project", "init", "--prefix", "AB"],
            "story project new",
        ),
        (vec!["project", "deinit", "--force"], "story project delete"),
    ] {
        let out = env
            .story(&dir)
            .args(&typed)
            .output()
            .expect("running a retired spelling with its old flags");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(replacement),
            "`story {}` must reach the redirect rather than a flag complaint: {stderr}",
            typed.join(" ")
        );
    }
}

/// `story relink` is not merely renamed, and the redirect says so.
///
/// `link checkout` records a path against a project named the ordinary way and
/// reads nothing in the directory. Somebody who types the old command needs the
/// selector, because the old grammar's first positional was the project.
#[test]
fn the_relink_redirect_names_the_selector_the_new_grammar_needs() {
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "repo");

    let out = env
        .story(&dir)
        .arg("relink")
        .output()
        .expect("running the old spelling");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--project"), "{stderr}");
}
