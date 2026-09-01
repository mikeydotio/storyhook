use assert_cmd::Command;
use std::fs;
use storyhook_test_support::{TestEnv, scratch_dir};

/// Every `story` this file runs is the one THIS build produced, in the shared
/// test environment's private `HOME`, XDG directories and store — so nothing
/// here can reach the developer's own storyhook state, with or without a
/// wrapper script supplying one.
fn story(dir: &std::path::Path) -> Command {
    TestEnv::shared().story(dir)
}

/// Appends `body` — one or more `[hooks.*]` tables — to the checkout's
/// committed pointer file.
///
/// This is where event-hook configuration lives after the flip: a table in
/// `.storyhook.toml` rather than a file inside a directory that is about to
/// stop existing.
///
/// **Appended, not written over.** It used to replace the whole file with a
/// fabricated identity, which worked only because the store also kept an index
/// of directories to fall back on; with that index gone (SH-119) a pointer
/// naming a project the store does not have makes the checkout unresolvable,
/// which is the correct refusal and useless as a hook fixture. Appending is
/// also what a user does: `story project new` writes the identity, and the
/// `[hooks]` tables are hand-added underneath it.
fn write_hooks(dir: &std::path::Path, body: &str) {
    let path = dir.join(".storyhook.toml");
    let identity = fs::read_to_string(&path)
        .expect("`story project new` must have written the pointer file this appends to");
    fs::write(&path, format!("{identity}\n{body}")).unwrap();
}

/// Writes the *legacy* `.storyhook/hooks.toml`, creating its directory.
///
/// Only the two tests whose subject is the legacy fallback use this. The
/// directory has to be created explicitly because `story init` no longer makes
/// one once the store is the default.
fn write_legacy_hooks(dir: &std::path::Path, body: &str) {
    fs::create_dir_all(dir.join(".storyhook")).unwrap();
    fs::write(dir.join(".storyhook/hooks.toml"), body).unwrap();
}

fn init_project(dir: &std::path::Path) {
    story(dir)
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
}

#[test]
fn hook_fires_on_create() {
    let dir = scratch_dir();
    let dir = dir.path();
    // Init git repo
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(dir)
        .output()
        .unwrap();

    init_project(dir);

    let output_file = dir.join("hook_output.json");
    write_hooks(
        dir,
        &format!(
            "[hooks.on_create]\ncommand = \"cat > {}\"\n",
            output_file.display()
        ),
    );

    story(dir).args(["new", "Test story"]).assert().success();

    assert!(output_file.exists(), "hook output file should exist");
    let content = fs::read_to_string(&output_file).unwrap();
    let payload: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(payload["event_type"], "create");
    assert_eq!(payload["story_title"], "Test story");
}

#[test]
fn hook_failure_does_not_prevent_operation() {
    let dir = scratch_dir();
    let dir = dir.path();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(dir)
        .output()
        .unwrap();

    init_project(dir);

    write_hooks(dir, "[hooks.on_create]\ncommand = \"exit 1\"\n");

    // Operation should still succeed even though hook fails
    story(dir).args(["new", "Test story"]).assert().success();
}

#[test]
fn no_hooks_flag_suppresses_hooks() {
    let dir = scratch_dir();
    let dir = dir.path();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(dir)
        .output()
        .unwrap();

    init_project(dir);

    let output_file = dir.join("hook_output.json");
    write_hooks(
        dir,
        &format!(
            "[hooks.on_create]\ncommand = \"cat > {}\"\n",
            output_file.display()
        ),
    );

    story(dir)
        .args(["--no-hooks", "new", "Test story"])
        .assert()
        .success();

    assert!(
        !output_file.exists(),
        "hook should not have fired with --no-hooks"
    );
}

#[test]
fn hooks_list_shows_configured() {
    let dir = scratch_dir();
    let dir = dir.path();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .unwrap();

    init_project(dir);

    write_hooks(
        dir,
        "[hooks.on_create]\ncommand = \"echo created\"\n\
         [hooks.on_close]\ncommand = \"echo closed\"\n",
    );

    let output = story(dir).args(["hooks", "list"]).output().unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("on_create"));
    assert!(stdout.contains("on_close"));
}

/// The four engine event hooks (SH-472) are ordinary `[hooks]` slots and
/// reach `story hooks list` the same way every other event does — the
/// "wiring reaches the CLI" half of this project's usual two-mechanism
/// coverage; `tests/engine_reconcile.rs` proves the other half, that
/// `EngineService` actually fires them.
#[test]
fn hooks_list_shows_a_configured_engine_hook() {
    let dir = scratch_dir();
    let dir = dir.path();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .unwrap();

    init_project(dir);

    write_hooks(
        dir,
        "[hooks.on_engine_run_halted]\ncommand = \"echo halted\"\n",
    );

    let output = story(dir).args(["hooks", "list"]).output().unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("on_engine_run_halted"));
    assert!(stdout.contains("echo halted"));
}

#[test]
fn hooks_list_no_config() {
    let dir = scratch_dir();
    let dir = dir.path();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .unwrap();

    init_project(dir);

    let output = story(dir).args(["hooks", "list"]).output().unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("no hooks configured"));
}

#[test]
fn no_hooks_toml_operations_work() {
    let dir = scratch_dir();
    let dir = dir.path();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(dir)
        .output()
        .unwrap();

    init_project(dir);

    // No hooks.toml — operations should work fine
    story(dir).args(["new", "Test story"]).assert().success();
}

/// The `[hooks]` table in the committed pointer file is read in place of
/// `.storyhook/hooks.toml`.
///
/// Where event-hook configuration lives is the other half of the question the
/// flip answers: it is *user-authored config about this repository*, not story
/// data, so it stays in the repository — it just stops living inside a
/// directory that is about to stop existing.
#[test]
fn hooks_can_be_configured_in_the_pointer_file() {
    let dir = scratch_dir();
    let dir = dir.path();
    init_project(dir);

    let output_file = dir.join("hook_output.json");
    write_hooks(
        dir,
        &format!(
            "[hooks.on_create]\ncommand = \"cat > {}\"\n",
            output_file.display()
        ),
    );

    story(dir)
        .args(["new", "Configured in the pointer"])
        .assert()
        .success();

    assert!(
        output_file.exists(),
        "a hook declared in .storyhook.toml's [hooks] table must fire"
    );
    let payload: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&output_file).unwrap()).unwrap();
    assert_eq!(payload["story_title"], "Configured in the pointer");
}

#[test]
fn the_pointers_hooks_table_wins_over_the_legacy_file() {
    let dir = scratch_dir();
    let dir = dir.path();
    init_project(dir);

    let legacy = dir.join("legacy.json");
    let pointed = dir.join("pointed.json");
    write_legacy_hooks(
        dir,
        &format!("[on_create]\ncommand = \"cat > {}\"\n", legacy.display()),
    );
    write_hooks(
        dir,
        &format!(
            "[hooks.on_create]\ncommand = \"cat > {}\"\n",
            pointed.display()
        ),
    );

    story(dir)
        .args(["new", "Two homes, one answer"])
        .assert()
        .success();

    assert!(pointed.exists(), "the pointer's table must be the one read");
    assert!(
        !legacy.exists(),
        "a repository that has moved its hooks must not fire the old ones as well"
    );
}

#[test]
fn a_pointer_with_no_hooks_table_leaves_the_legacy_file_in_charge() {
    let dir = scratch_dir();
    let dir = dir.path();
    init_project(dir);

    let legacy = dir.join("legacy.json");
    write_legacy_hooks(
        dir,
        &format!("[on_create]\ncommand = \"cat > {}\"\n", legacy.display()),
    );
    // No `[hooks]` table: the pointer file `story project new` wrote carries
    // identity and nothing else, which is exactly the premise here.

    story(dir)
        .args(["new", "Still on the old config"])
        .assert()
        .success();

    assert!(
        legacy.exists(),
        "the two storage models coexist until the daemon wave"
    );
}

/// A failing hook's own words reach the daemon log, even while a descendant of
/// it still holds the stderr it was given.
///
/// # Why this test exists, and why it reads a log file
///
/// It is the went-too-far guard for SH-141. The wedge it protects against was
/// closed by giving the hook files instead of pipes, and the cheapest wrong way
/// to have closed it was to stop capturing hook stderr at all — which would pass
/// every timing assertion in `tests/hook_bounds.rs` while making every failing
/// hook silent. Nothing anywhere asserted that message before this test:
/// `hook_failure_does_not_prevent_operation` above checks `.success()` and never
/// looks at what was said.
///
/// The log rather than the terminal, because that is where the message actually
/// goes and it always has. Every `fire_hook` call site is under `src/service/`,
/// and since SH-114 the daemon is the only route to the store, so the
/// `eprintln!` is the *daemon's* stderr — which `daemon::lifecycle` points at
/// `daemon.log`. A user running `story new` sees nothing on their own terminal
/// whether the hook succeeded or failed. Asserting on the CLI's stderr would
/// therefore pass with the capture deleted, proving nothing.
///
/// The grandchild is what makes it a regression test rather than a coverage
/// test: without it, a single blocking read returns the bytes immediately and
/// the old code passes too.
///
/// The daemon is deliberately *not* stood down before the read. CLAUDE.md's rule
/// about stopping it first concerns bytes in the **store**, which the daemon
/// answers from its own page cache; its log is an ordinary append the daemon has
/// already flushed, since Rust's `stderr` is unbuffered.
#[test]
fn a_failing_hooks_reason_reaches_the_daemon_log() {
    let env = storyhook_test_support::TestEnv::isolated();
    let dir = scratch_dir();
    let dir = dir.path();

    env.story(dir)
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    // `sleep` inherits the stderr `sh` was given and outlives it, so the
    // end-of-file the old code waited for never arrives inside this test's life.
    write_hooks(
        dir,
        "[hooks.on_create]\ncommand = \"echo THE-HOOK-IS-UNHAPPY >&2; sleep 300 & exit 3\"\n",
    );

    let out = env
        .story(dir)
        .args(["new", "A story whose hook fails"])
        .output()
        .expect("running story");
    assert!(
        out.status.success(),
        "a failing hook must not fail the command"
    );

    let log = env.environment().daemon_log();
    let contents = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        contents.contains("THE-HOOK-IS-UNHAPPY"),
        "the hook's own words are missing from {}.\n\
         A hook that fails must still be able to say why: that is the whole \
         reason storyhook captures its stderr, and capturing it is what SH-141 \
         had to keep while removing the wedge.\n\
         log contents:\n{contents}",
        log.display()
    );
    assert!(
        contents.contains("create hook failed"),
        "the diagnostic must name the event and the failure, not just echo the \
         hook's bytes.\nlog contents:\n{contents}"
    );
}
