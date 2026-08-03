//! `story project delete` — the one command that destroys work.
//!
//! It removes the project, every story, every event, every checkout
//! registration and every registered origin. There is no undo, so most of what
//! is here is about the gate rather than the deletion: what the warning says,
//! what happens when nobody can answer it, and what survives when the answer is
//! no.
//!
//! The confirmation is deliberately a **typed slug** rather than `[y/N]`. A
//! single keystroke is the right weight for "reopen this deleted story" and the
//! wrong weight for "erase four thousand events".
//!
//! # What changed when `deinit` became `delete`
//!
//! Two things, and every assertion below is about one of them.
//!
//! It **touches no filesystem**. `deinit` deleted the `.storyhook.toml` and the
//! `AGENTS.md` it had generated in every recorded checkout — reaching into
//! directories the caller had not named, justified by the plan having listed
//! them first. `delete` leaves them alone. A stale pointer file is a tidiness
//! complaint with a clear diagnosis; a destroyed repository file is work gone,
//! and the two are not the same size of mistake. The plan still *names* the
//! checkouts, and now says the files in them are left behind.
//!
//! It takes **no positional**. `deinit` accepted a path or a slug and guessed
//! which; `delete` is named by the ordinary SH-116 selector — the working
//! directory, `--project <slug>`, or `STORYHOOK_PROJECT` — and by nothing else.
//! A fourth way to name a project is the thing SH-116 exists to have removed.

use std::path::Path;

use storyhook_test_support::TestEnv;

/// A project with `stories` stories in it, at `<home>/<name>`.
fn project_with(env: &TestEnv, name: &str, stories: usize) -> std::path::PathBuf {
    let dir = env.home().join(name);
    std::fs::create_dir_all(&dir).expect("creating the repository");
    env.story(&dir)
        .args(["project", "new", "--prefix", "SH"])
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

/// Runs `story project delete …` with stdin closed — no terminal, which is what
/// every test process has anyway.
fn delete(env: &TestEnv, cwd: &Path, args: &[&str]) -> std::process::Output {
    let mut cmd = env.story(cwd);
    cmd.args(["project", "delete"]);
    cmd.args(args);
    cmd.output().expect("running story project delete")
}

/// Runs `story --project <slug> project delete …` from `cwd`.
fn delete_by_slug(env: &TestEnv, cwd: &Path, slug: &str, args: &[&str]) -> std::process::Output {
    let mut cmd = env.story(cwd);
    cmd.args(["--project", slug, "project", "delete"]);
    cmd.args(args);
    cmd.output().expect("running story project delete")
}

/// Everything `story project list` printed, run from `cwd`.
fn listing(env: &TestEnv, cwd: &Path) -> String {
    let out = env
        .story(cwd)
        .args(["project", "list"])
        .output()
        .expect("listing projects");
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Whether the store still knows a project by this slug.
///
/// Matches the listing's row shape rather than searching the whole output for
/// the slug. A substring search passes for the wrong reason here: the empty
/// catalog's own message contains "repo", so a test whose fixture is called
/// `repo` would see its project as still present precisely when it had
/// successfully deleted it.
fn store_has_project(env: &TestEnv, cwd: &Path, slug: &str) -> bool {
    listing(env, cwd)
        .lines()
        .any(|line| line.trim_start().starts_with(&format!("{slug} — ")))
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

#[test]
fn without_force_and_without_a_terminal_it_refuses_and_names_the_flag() {
    let env = TestEnv::isolated();
    let dir = project_with(&env, "repo", 2);

    let out = delete(&env, &dir, &[]);

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
    assert!(
        store_has_project(&env, &dir, "repo"),
        "a refused delete destroys nothing"
    );
}

#[test]
fn the_refusal_says_what_would_have_been_destroyed() {
    // A flag named without the stakes is a flag people pass reflexively.
    let env = TestEnv::isolated();
    let dir = project_with(&env, "repo", 3);

    let out = delete(&env, &dir, &[]);
    let stderr = stderr(&out);

    assert!(stderr.contains('3'), "the story count is stated: {stderr}");
    assert!(stderr.contains("repo"), "the project is named: {stderr}");
}

#[test]
fn json_without_force_refuses_rather_than_prompting_into_the_stream() {
    // `--json` promises one self-describing document on stdout. A prompt
    // written there would corrupt it for every scripted caller.
    let env = TestEnv::isolated();
    let dir = project_with(&env, "repo", 1);

    let out = delete(&env, &dir, &["--json"]);

    assert_eq!(out.status.code(), Some(2));
    let stdout = stdout(&out);
    assert!(
        stdout.trim().is_empty() || serde_json::from_str::<serde_json::Value>(&stdout).is_ok(),
        "stdout must stay machine-readable: {stdout}"
    );
    assert!(store_has_project(&env, &dir, "repo"));
}

/// The two-step completes in **one** confirmation cycle, under a wall clock.
///
/// This is what a `forced()` that forgot `Delete` fails, and it is why the
/// bound is here rather than left to the harness. Without the arm the client
/// re-sends an unforced request, the daemon answers with the same plan, and the
/// pair loops forever: no error, no compile failure, no failing assertion —
/// just a command that never returns. A timeout naming this test is a diagnosis;
/// a wedged gate is not.
#[test]
fn the_two_step_round_trip_completes_in_one_cycle() {
    let env = TestEnv::isolated();
    let dir = project_with(&env, "repo", 1);

    let started = std::time::Instant::now();
    let refused = delete(&env, &dir, &[]);
    assert_eq!(refused.status.code(), Some(2));
    let forced = delete(&env, &dir, &["--force"]);
    let elapsed = started.elapsed();

    assert_eq!(forced.status.code(), Some(0), "{}", stderr(&forced));
    assert!(
        elapsed < std::time::Duration::from_secs(60),
        "the refusal and the forced retry together took {elapsed:?}; a confirmation that \
         re-asks forever looks exactly like this"
    );
    assert!(!store_has_project(&env, &dir, "repo"));
}

/// `main` reads standard input *before* dispatching, for the commands that
/// consume it. Delete must not be one: draining stdin first would leave the
/// confirmation prompt reading EOF, and every delete would silently cancel.
#[test]
fn delete_must_not_read_stdin_before_it_prompts() {
    use storyhook::cli::{Invocation, ProjectAction};

    for force in [false, true] {
        assert!(
            !storyhook::invoke::reads_stdin(&Invocation::Project {
                action: ProjectAction::Delete { force },
            }),
            "the prompt needs the terminal that `reads_stdin` would consume"
        );
    }
}

// ---------------------------------------------------------------------------
// The deletion
// ---------------------------------------------------------------------------

#[test]
fn force_destroys_the_project_its_stories_and_its_events() {
    let env = TestEnv::isolated();
    let dir = project_with(&env, "repo", 3);
    let elsewhere = env.home().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();

    let out = delete(&env, &dir, &["--force"]);

    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(!store_has_project(&env, &elsewhere, "repo"));
    // The directory stops resolving: no project here any more.
    env.story(&dir).arg("summary").assert().failure();
}

#[test]
fn a_neighbouring_project_is_untouched() {
    let env = TestEnv::isolated();
    let doomed = project_with(&env, "doomed", 2);
    let kept = project_with(&env, "kept", 2);

    delete(&env, &doomed, &["--force"]);

    assert!(store_has_project(&env, &kept, "kept"));
    // Both projects took the default prefix, so both minted `SH-1`. That is the
    // shape a scope bug would show up in: a delete that reached across projects
    // by story number rather than by project would take this one with it.
    for number in 1..=2 {
        env.story(&kept)
            .args(["show", &format!("SH-{number}")])
            .assert()
            .success()
            .stdout(predicates::str::contains(format!("Story {number}")));
    }
}

#[test]
fn a_second_delete_of_the_same_project_fails_cleanly() {
    // Running it twice is not a no-op — there is nothing left to delete — but
    // it must fail cleanly rather than half-succeed or panic.
    let env = TestEnv::isolated();
    let doomed = project_with(&env, "repo", 1);
    let here = project_with(&env, "here", 0);

    assert_eq!(
        delete_by_slug(&env, &here, "repo", &["--force"])
            .status
            .code(),
        Some(0)
    );
    let second = delete_by_slug(&env, &here, "repo", &["--force"]);

    assert_ne!(second.status.code(), Some(0));
    assert!(
        !stderr(&second).is_empty(),
        "the failure must say something"
    );
    let _ = doomed;
}

// ---------------------------------------------------------------------------
// The filesystem, which this verb no longer touches — D20(b) and D20(d)
// ---------------------------------------------------------------------------

/// D20(b). The two files `project new` generated survive the project they
/// belonged to.
///
/// `deinit` removed both. It is the single behaviour change this commit is
/// about, and this is the test that says so: nothing under the checkout is the
/// tracker's to destroy, because the tracker's data does not live there.
#[test]
fn the_checkouts_files_survive_the_delete() {
    let env = TestEnv::isolated();
    let dir = project_with(&env, "repo", 1);
    let pointer = dir.join(".storyhook.toml");
    let agents = dir.join("AGENTS.md");
    assert!(
        pointer.exists() && agents.exists(),
        "the fixture writes both"
    );
    let agents_before = std::fs::read_to_string(&agents).unwrap();

    let out = delete(&env, &dir, &["--force"]);

    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        !store_has_project(&env, &dir, "repo"),
        "the project is gone"
    );
    assert!(
        pointer.exists(),
        "`.storyhook.toml` is repository content, not tracker state"
    );
    assert_eq!(
        std::fs::read_to_string(&agents).unwrap(),
        agents_before,
        "and `AGENTS.md` is untouched, edited or not"
    );
}

/// D20(d). Deleting by selector from somewhere else leaves that checkout's
/// files alone.
///
/// The half of the old behaviour that reached furthest: `deinit` cleared the
/// repository files of *every* recorded checkout, including ones the caller had
/// never named. This is that case, now asserting the opposite.
#[test]
fn deleting_from_an_unrelated_directory_leaves_the_checkout_alone() {
    let env = TestEnv::isolated();
    let doomed = project_with(&env, "doomed", 1);
    let here = project_with(&env, "here", 0);

    let out = delete_by_slug(&env, &here, "doomed", &["--force"]);

    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(!store_has_project(&env, &here, "doomed"));
    assert!(
        doomed.join(".storyhook.toml").exists(),
        "a directory the caller never named must not be written to"
    );
    assert!(doomed.join("AGENTS.md").exists());
}

/// The plan still names every recorded checkout, and now says why they are in
/// the list.
///
/// Naming them is not decoration: they are the directories left carrying a
/// pointer file that names a project which no longer exists, and the person
/// confirming is the only one who can decide what to do about that.
#[test]
fn the_plan_names_the_checkouts_and_says_their_files_are_left_behind() {
    let env = TestEnv::isolated();
    let dir = project_with(&env, "repo", 1);

    let refused = delete(&env, &dir, &[]);
    let stderr = stderr(&refused);

    let canonical = dir.canonicalize().unwrap();
    assert!(
        stderr.contains(&canonical.display().to_string()),
        "the checkout must be named: {stderr}"
    );
    assert!(
        stderr.contains("left"),
        "and the warning must say the files in it are left behind: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// What the row takes with it — D20(c) and D20(e)
// ---------------------------------------------------------------------------

/// D20(c). After a delete the origin is **free**, and another project may claim
/// it.
///
/// The only test in the suite that would notice `ON DELETE CASCADE` on
/// `project_remotes` regressing. A leaked row is not a leak that fades: the
/// unique index on `normalized` makes the identity permanently unclaimable, and
/// the refusal names a project that no longer exists.
#[test]
fn the_deleted_projects_origin_becomes_claimable_again() {
    const URL: &str = "git@github.com:acme/widgets.git";
    let env = TestEnv::isolated();
    let doomed = project_with(&env, "doomed", 0);
    let heir = project_with(&env, "heir", 0);

    env.story(&doomed)
        .args(["project", "link", "origin", URL])
        .assert()
        .success();
    // The claim is exclusive while the project holds it.
    let refused = env
        .story(&heir)
        .args(["project", "link", "origin", URL])
        .output()
        .expect("running link origin");
    assert_ne!(refused.status.code(), Some(0), "an origin has one holder");

    delete_by_slug(&env, &heir, "doomed", &["--force"]);

    let out = env
        .story(&heir)
        .args(["project", "link", "origin", URL])
        .output()
        .expect("running link origin");
    assert_eq!(
        out.status.code(),
        Some(0),
        "the freed origin must be claimable; stderr={}",
        stderr(&out)
    );
    assert!(listing(&env, &heir).contains(URL));
}

/// D20(e). The linked checkout goes with the row.
///
/// Asked of `story project list`, which is `checkout_path`'s only production
/// reader, rather than of the database — so this asserts what a user would see.
#[test]
fn the_linked_checkout_goes_with_the_row() {
    let env = TestEnv::isolated();
    let doomed = project_with(&env, "doomed", 0);
    let here = project_with(&env, "here", 0);
    let elsewhere = env.home().join("some-worktree");
    std::fs::create_dir_all(&elsewhere).unwrap();

    env.story(&doomed)
        .args(["project", "link", "checkout", elsewhere.to_str().unwrap()])
        .assert()
        .success();
    let canonical = elsewhere.canonicalize().unwrap();
    assert!(
        listing(&env, &here).contains(&canonical.display().to_string()),
        "the fixture must have linked it"
    );

    delete_by_slug(&env, &here, "doomed", &["--force"]);

    assert!(
        !listing(&env, &here).contains(&canonical.display().to_string()),
        "no surviving project may still report the deleted one's checkout"
    );
    assert!(
        elsewhere.is_dir(),
        "and the directory itself is not the tracker's to remove"
    );
}

// ---------------------------------------------------------------------------
// Naming the project, and failing to
// ---------------------------------------------------------------------------

#[test]
fn delete_reaches_a_project_whose_checkout_is_gone() {
    // The project this most needs to reach is one whose directory has been
    // deleted. There is no working directory left to stand in, and it is
    // exactly the project a user wants rid of.
    let env = TestEnv::isolated();
    let gone = project_with(&env, "gone", 1);
    let here = project_with(&env, "here", 1);
    std::fs::remove_dir_all(&gone).unwrap();
    env.story(&here)
        .args(["doctor", "--fix"])
        .assert()
        .success();
    assert!(store_has_project(&env, &here, "gone"));

    let out = delete_by_slug(&env, &here, "gone", &["--force"]);

    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(!store_has_project(&env, &here, "gone"));
    assert!(store_has_project(&env, &here, "here"));
}

#[test]
fn delete_in_a_directory_with_no_project_refuses_the_way_every_scoped_verb_does() {
    let env = TestEnv::isolated();
    let bare = env.home().join("bare");
    std::fs::create_dir_all(&bare).unwrap();

    let out = delete(&env, &bare, &["--force"]);

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

/// A bare word after `delete` is refused rather than guessed at.
///
/// `deinit` accepted one and had to decide whether it was a path or a slug.
/// That guess is the fourth way of naming a project SH-116 exists to have
/// removed, and the refusal names the flag that replaces it.
#[test]
fn delete_takes_no_positional() {
    let env = TestEnv::isolated();
    let dir = project_with(&env, "repo", 1);

    let out = delete(&env, &dir, &["repo", "--force"]);

    assert_eq!(
        out.status.code(),
        Some(2),
        "a positional is a usage error; stderr={}",
        stderr(&out)
    );
    assert!(
        store_has_project(&env, &dir, "repo"),
        "and nothing is destroyed on the way to saying so"
    );
}
