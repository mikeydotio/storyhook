//! The two prompts, over a real terminal (SH-117, D19/D20).
//!
//! # What is new here
//!
//! Every other harness in this repository pipes stdin, and a pipe is not a
//! terminal — so `story project new`'s questionnaire and the typed-slug
//! confirmation that guards project destruction have never been *executed* by a
//! test, only refused by one. `storyhook_test_support::Pty` puts a real
//! `openpty` between the test and the binary, so `IsTerminal` answers true and
//! both branches run.
//!
//! The confirmation in particular has guarded every project destruction in this
//! repository's history — `deinit`'s, and now `delete`'s — without a single
//! test ever having typed a token into it. Two of the five tests here are about
//! that.
//!
//! # Why this file cannot wedge the gate
//!
//! `make test` is the only gate this repository has. Every `expect` carries its
//! own deadline and fails with the transcript; [`watchdog`] bounds the whole
//! file and aborts rather than stalling; each child carries
//! `daemon_containment()`, enforced by `Pty::spawn`, because killing a child
//! does *not* kill a daemon it started; and
//! `scripts/check-no-orphan-servers.sh postlude` is the backstop.
//!
//! **Never widen an `expect` to stabilize a test here.** An `expect` that
//! matches anything asserts nothing and will pass forever. If a case cannot be
//! made deterministic, delete the case.

use std::path::Path;
use std::time::Duration;

use storyhook_test_support::{Pty, TestEnv, Watchdog, watchdog};

/// The wall-clock bound on this whole file.
///
/// Measured: five conversations totalling about 2.5 seconds warm. The margin is
/// for the sporadic multi-second stall recorded on
/// [`EXPECT_TIMEOUT`](storyhook_test_support::EXPECT_TIMEOUT), not for the
/// conversations themselves — anything near this is a wedge.
const FILE_BUDGET: Duration = Duration::from_secs(180);

fn armed() -> Watchdog {
    watchdog("tests/pty_interactive.rs", FILE_BUDGET)
}

/// A directory inside the environment's scratch space, created but not
/// initialized.
fn bare_dir(env: &TestEnv, name: &str) -> std::path::PathBuf {
    let dir = env.home().join(name);
    std::fs::create_dir_all(&dir).expect("creating a bare directory");
    dir
}

/// `story …` under a pty, in `cwd`, with this environment applied.
///
/// `raw_story` rather than a hand-built `Command`, because it is what applies
/// `daemon_containment()` — and `Pty::spawn` refuses a child without it.
fn pty(label: &str, env: &TestEnv, cwd: &Path, args: &[&str]) -> Pty {
    let mut command = env.raw_story(cwd);
    command.args(args);
    Pty::spawn(label, command)
}

/// Everything `story project list` printed, run without a terminal.
fn listing(env: &TestEnv, cwd: &Path) -> String {
    let out = env
        .story(cwd)
        .args(["project", "list"])
        .output()
        .expect("running `story project list`");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// ---------------------------------------------------------------------------
// the questionnaire
// ---------------------------------------------------------------------------

/// D2 end to end: a bare `story project new` at a terminal asks, and creates
/// exactly what the summary described.
#[test]
fn a_bare_project_new_at_a_terminal_asks_and_creates_what_it_described() {
    let _watchdog = armed();
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "asked");

    let mut pty = pty("questionnaire", &env, &dir, &["project", "new"]);
    pty.expect("Project name");
    pty.send_line("Widgets");
    pty.expect("Story-id prefix");
    pty.send_line("WID");
    pty.expect("Attach this checkout");
    pty.send_line("");
    pty.expect("Generate AGENTS.md?");
    pty.send_line("n");
    // The summary, before the last question rather than after it.
    pty.expect("prefix     WID");
    pty.expect("Create this project?");
    pty.send_line("y");
    let status = pty.wait();
    assert!(status.success(), "{}", pty.transcript());

    // What the summary said is what exists.
    let listed = listing(&env, &dir);
    assert!(listed.contains("widgets"), "{listed}");
    assert!(
        !dir.join("AGENTS.md").exists(),
        "`n` at the AGENTS.md question must be honoured"
    );
    env.story(&dir)
        .args(["new", "The first story"])
        .assert()
        .success()
        .stdout(predicates::str::contains("WID-1"));
}

/// A cancellation is a decision, so it exits 0 — and, far more importantly,
/// nothing was created.
#[test]
fn declining_the_last_question_creates_nothing_and_exits_zero() {
    let _watchdog = armed();
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "declined");

    let mut pty = pty("cancel", &env, &dir, &["project", "new"]);
    pty.expect("Project name");
    pty.send_line("");
    pty.expect("Story-id prefix");
    pty.send_line("");
    pty.expect("Attach this checkout");
    pty.send_line("");
    pty.expect("Generate AGENTS.md?");
    pty.send_line("");
    pty.expect("Create this project?");
    pty.send_line("n");
    pty.expect("cancelled; nothing was created");
    let status = pty.wait();

    assert_eq!(status.code(), Some(0), "{}", pty.transcript());
    assert!(
        listing(&env, &dir).contains("No projects yet"),
        "a declined questionnaire must leave the store untouched"
    );
    assert!(!dir.join(".storyhook.toml").exists());
    assert!(!dir.join("AGENTS.md").exists());
}

/// The prefix is the one answer that cannot be undone, so a bad one is caught
/// while it is still free to fix.
#[test]
fn an_invalid_prefix_is_re_asked_at_the_terminal() {
    let _watchdog = armed();
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "typo");

    let mut pty = pty("retry", &env, &dir, &["project", "new"]);
    pty.expect("Project name");
    pty.send_line("Widgets");
    pty.expect("Story-id prefix");
    pty.send_line("hello world");
    pty.expect("invalid story-id prefix");
    pty.send_line("WID");
    pty.expect("Attach this checkout");
    pty.send_line("");
    pty.expect("Generate AGENTS.md?");
    pty.send_line("n");
    pty.expect("Create this project?");
    pty.send_line("y");
    assert!(pty.wait().success(), "{}", pty.transcript());

    env.story(&dir)
        .args(["new", "The first story"])
        .assert()
        .success()
        .stdout(predicates::str::contains("WID-1"));
}

// ---------------------------------------------------------------------------
// the typed-slug confirmation
// ---------------------------------------------------------------------------

/// Creates a project in `dir` and returns its slug, without a terminal.
fn project_at(env: &TestEnv, dir: &Path) -> String {
    env.story(dir)
        .args(["project", "new", "--prefix", "SH", "--no-agents-md"])
        .assert()
        .success();
    let listed = listing(env, dir);
    let canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    listed
        .lines()
        .find(|line| line.contains(&*canonical.to_string_lossy()))
        .and_then(|line| line.split_whitespace().next())
        .unwrap_or_else(|| {
            panic!(
                "no `project list` row for {}:\n{listed}",
                canonical.display()
            )
        })
        .to_string()
}

/// **The gap the council's brief declared unreachable.** The typed-slug gate
/// has protected every project destruction in this repository's history and
/// has never once been typed into by a test.
#[test]
fn the_typed_slug_confirmation_destroys_on_the_correct_token() {
    let _watchdog = armed();
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "doomed");
    let slug = project_at(&env, &dir);

    let mut pty = pty("confirm-yes", &env, &dir, &["project", "delete"]);
    pty.expect(&format!("Type `{slug}` to confirm"));
    pty.send_line(&slug);
    assert!(pty.wait().success(), "{}", pty.transcript());

    assert!(
        !listing(&env, &dir).contains(&slug),
        "the correct token must destroy the project"
    );
}

/// The other half, and the one that matters more: a wrong token destroys
/// nothing. A gate that accepts anything is worse than no gate, because it
/// reads as protection.
#[test]
fn a_wrong_token_cancels_and_destroys_nothing() {
    let _watchdog = armed();
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "spared");
    let slug = project_at(&env, &dir);
    env.story(&dir)
        .args(["new", "A story that must survive"])
        .assert()
        .success();

    let mut pty = pty("confirm-no", &env, &dir, &["project", "delete"]);
    pty.expect(&format!("Type `{slug}` to confirm"));
    pty.send_line("not-the-slug");
    pty.expect("cancelled");
    let status = pty.wait();

    assert_eq!(status.code(), Some(0), "{}", pty.transcript());
    assert!(
        listing(&env, &dir).contains(&slug),
        "a wrong token must destroy nothing"
    );
    env.story(&dir)
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicates::str::contains("A story that must survive"));
}

// ---------------------------------------------------------------------------
// the undelete confirmation (SH-154)
// ---------------------------------------------------------------------------

/// Deletes `SH-1` in a fresh project at `dir` and returns the project's env.
fn project_with_a_deleted_story(env: &TestEnv, dir: &Path) {
    env.story(dir)
        .args(["project", "new", "--prefix", "SH", "--no-agents-md"])
        .assert()
        .success();
    env.story(dir)
        .args(["new", "Deleted by mistake"])
        .assert()
        .success();
    env.story(dir)
        .args(["delete", "SH-1", "created in error"])
        .assert()
        .success();
}

/// `--json show SH-1`'s stdout, without a terminal.
fn show(env: &TestEnv, dir: &Path) -> String {
    let out = env
        .story(dir)
        .args(["--json", "show", "SH-1"])
        .output()
        .expect("running `story show`");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// **The gap `confirm_undelete` left unreachable.** It prompted from inside
/// the service layer, which runs in the daemon and never has a terminal — so
/// `story reopen <deleted-id>` hard-refused unconditionally, even run
/// interactively at a real pty. No test could ever answer "yes" to it,
/// because the question never actually reached one. This is the first.
#[test]
fn the_undelete_confirmation_restores_the_story_on_yes() {
    let _watchdog = armed();
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "undelete-yes");
    project_with_a_deleted_story(&env, &dir);

    let mut pty = pty("undelete-yes", &env, &dir, &["reopen", "SH-1"]);
    pty.expect("SH-1 — Deleted by mistake");
    pty.expect("deleted: created in error");
    pty.expect("Reopen (undelete) this deleted story?");
    pty.send_line("y");
    assert!(pty.wait().success(), "{}", pty.transcript());

    let after = show(&env, &dir);
    assert!(after.contains("\"superstate\": \"OPEN\""), "{after}");
    assert!(!after.contains("\"deleted\": true"), "{after}");
}

/// The other half: declining leaves the story exactly as deleted as it was.
#[test]
fn declining_the_undelete_confirmation_leaves_the_story_deleted() {
    let _watchdog = armed();
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "undelete-no");
    project_with_a_deleted_story(&env, &dir);

    let mut pty = pty("undelete-no", &env, &dir, &["reopen", "SH-1"]);
    pty.expect("Reopen (undelete) this deleted story?");
    pty.send_line("n");
    pty.expect("cancelled");
    let status = pty.wait();

    assert_eq!(status.code(), Some(0), "{}", pty.transcript());
    let after = show(&env, &dir);
    assert!(after.contains("\"deleted\": true"), "{after}");
    assert!(after.contains("\"superstate\": \"CLOSED\""), "{after}");
}
