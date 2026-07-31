//! Running commands through the daemon.
//!
//! The transport's job is to be invisible: a command run through
//! `/api/v1/invoke` must produce what the same command produces in this process,
//! byte for byte, on both streams and in its exit code. These tests check that
//! claim and the two places it could break — the recursion depth a hook carries,
//! and the version the two ends agree on.

use std::time::{Duration, Instant};

use storyhook::daemon::lifecycle;
use storyhook_test_support::{TestEnv, scratch_dir, story_binary};

/// A hook command that runs the `story` binary under test.
///
/// The absolute path is not a nicety: a bare `story` resolves through `PATH`,
/// which in a test run means the developer's *installed* storyhook — a
/// different build, which the daemon would then refuse and try to restart. The
/// hook has to run the binary the test is about.
fn hook_running_story(args: &str) -> String {
    format!(
        "[hooks.on_create]\ncommand = \"{} {args}\"\n",
        story_binary().display()
    )
}

/// Stops whatever daemon `env` is running, however the test ends.
struct DaemonGuard<'a>(&'a TestEnv);

impl Drop for DaemonGuard<'_> {
    fn drop(&mut self) {
        let _ = lifecycle::stop(&self.0.environment());
    }
}

/// Runs `story` through the daemon.
fn via_daemon(env: &TestEnv, cwd: &std::path::Path, args: &[&str]) -> std::process::Output {
    env.story(cwd)
        .env("STORYHOOK_INVOKER", "daemon")
        .args(args)
        .output()
        .expect("running story")
}

/// Runs `story` in its own process.
fn via_local(env: &TestEnv, cwd: &std::path::Path, args: &[&str]) -> std::process::Output {
    env.story(cwd)
        .env("STORYHOOK_INVOKER", "local")
        .args(args)
        .output()
        .expect("running story")
}

/// A project, initialized locally so the fixture does not depend on the thing
/// under test.
fn project(env: &TestEnv) -> tempfile::TempDir {
    let dir = scratch_dir();
    let out = via_local(env, dir.path(), &["init", "--no-agents-md"]);
    assert!(out.status.success(), "initializing the fixture project");
    dir
}

/// Writes hook configuration into the checkout's committed pointer file,
/// preserving the project identity `story init` put there.
fn write_hooks(dir: &std::path::Path, body: &str) {
    let pointer = dir.join(".storyhook.toml");
    let existing = std::fs::read_to_string(&pointer).expect("the project has a pointer file");
    std::fs::write(&pointer, format!("{existing}\n{body}")).expect("writing hooks");
}

#[test]
fn the_first_command_starts_a_daemon_and_the_answer_is_the_same_one() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let dir = project(&env);

    assert!(
        env.daemon().is_none(),
        "the fixture must not have started one yet"
    );

    let out = via_daemon(&env, dir.path(), &["new", "Through the daemon"]);
    assert!(out.status.success(), "{out:?}");
    assert!(
        env.daemon().is_some(),
        "the first command must auto-spawn a daemon"
    );

    // And the write really landed in the shared store, not somewhere the daemon
    // kept to itself.
    let listed = via_local(&env, dir.path(), &["list"]);
    assert!(String::from_utf8_lossy(&listed.stdout).contains("Through the daemon"));
}

/// **The byte-comparison.** The same command, run both ways, must produce
/// identical stdout, identical stderr, and the same exit code — for successes,
/// for failures, and in both rendering modes.
///
/// This is a check on the argument rather than the argument itself: rendering
/// never leaves the client, so identical output is structural. A failure here
/// means something rendered on the wrong side of the wire.
#[test]
fn every_answer_is_byte_identical_through_the_daemon_and_in_process() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let dir = project(&env);
    via_local(&env, dir.path(), &["new", "A story to look at"]);

    let cases: &[&[&str]] = &[
        &["show", "SH-1"],
        &["show", "SH-1", "--json"],
        &["list"],
        &["list", "--json"],
        &["summary"],
        &["summary", "--json"],
        &["graph"],
        &["context"],
        &["next"],
        &["version"],
        // Failures too: an error's rendering and its exit code cross the same
        // wire as a success, and a variant that arrived as prose would show up
        // here as a different message.
        &["show", "SH-404"],
        &["show", "SH-404", "--json"],
        &["move", "SH-1", "not-a-state"],
        &["move", "SH-1", "not-a-state", "--json"],
        &["new"],
    ];

    for args in cases {
        let daemon = via_daemon(&env, dir.path(), args);
        let local = via_local(&env, dir.path(), args);
        assert_eq!(
            String::from_utf8_lossy(&daemon.stdout),
            String::from_utf8_lossy(&local.stdout),
            "stdout differs for `story {}`",
            args.join(" ")
        );
        assert_eq!(
            String::from_utf8_lossy(&daemon.stderr),
            String::from_utf8_lossy(&local.stderr),
            "stderr differs for `story {}`",
            args.join(" ")
        );
        assert_eq!(
            daemon.status.code(),
            local.status.code(),
            "exit code differs for `story {}`",
            args.join(" ")
        );
    }
}

/// **Reentrancy terminates.** A hook that runs `story` reaches the daemon, and
/// the daemon must not fire the hook that spawned it.
///
/// Under a CLI this loop was bounded by a variable in the child's environment.
/// Under a daemon the child's environment says nothing about the daemon's state,
/// so depth has to travel in the envelope — and if it did not, this test would
/// not fail, it would never finish.
#[test]
fn a_hook_that_runs_story_terminates() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let dir = project(&env);

    // The sharpest shape available: the hook fires the very event that fires it.
    write_hooks(dir.path(), &hook_running_story("new 'spawned by the hook'"));

    let started = Instant::now();
    let out = via_daemon(&env, dir.path(), &["new", "The one a human asked for"]);
    assert!(out.status.success(), "{out:?}");
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "the hook chain did not terminate promptly"
    );

    // Two stories: the one asked for, and exactly one from its hook. A third
    // would mean the hook's own `story new` fired the hook again.
    let listed = via_local(&env, dir.path(), &["list", "--json"]);
    let json: serde_json::Value = serde_json::from_slice(&listed.stdout).expect("a JSON listing");
    let stories = json["stories"].as_array().expect("a stories array");
    assert_eq!(
        stories.len(),
        2,
        "expected the story and one hook-created story, got: {json}"
    );
}

/// `--no-hooks` still reaches the daemon and still suppresses hooks, because it
/// travels in the envelope rather than being applied before the hop.
#[test]
fn no_hooks_crosses_the_wire() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let dir = project(&env);
    write_hooks(dir.path(), &hook_running_story("new 'spawned by the hook'"));

    let out = via_daemon(&env, dir.path(), &["new", "Quietly", "--no-hooks"]);
    assert!(out.status.success(), "{out:?}");

    let listed = via_local(&env, dir.path(), &["list", "--json"]);
    let json: serde_json::Value = serde_json::from_slice(&listed.stdout).expect("a JSON listing");
    assert_eq!(
        json["stories"].as_array().expect("stories").len(),
        1,
        "`--no-hooks` must suppress the hook on the far side of the wire too"
    );
}

/// `--local` runs without a daemon, and does not start one. Git hooks and CI
/// depend on that: a daemon spawned inside `prepare-commit-msg` is hostile.
#[test]
fn local_runs_without_a_daemon_and_starts_none() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let dir = project(&env);

    let out = via_local(&env, dir.path(), &["new", "No daemon needed"]);
    assert!(out.status.success(), "{out:?}");
    assert!(
        env.daemon().is_none(),
        "`--local` must not leave a daemon behind"
    );
    assert!(!lifecycle::is_live(&env.environment()));
}

/// The flag spelling and the environment spelling are the same mode.
#[test]
fn the_local_flag_and_the_local_variable_agree() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let dir = project(&env);

    let out = env
        .story(dir.path())
        .env("STORYHOOK_INVOKER", "daemon")
        .args(["--local", "new", "Flagged local"])
        .output()
        .expect("running story");
    assert!(out.status.success(), "{out:?}");
    assert!(
        env.daemon().is_none(),
        "`--local` must win over STORYHOOK_INVOKER=daemon"
    );
}

/// A backend nobody implements is refused by name, rather than silently
/// becoming one that is.
#[test]
fn an_unknown_backend_is_refused_loudly() {
    let env = TestEnv::isolated();
    let dir = scratch_dir();
    env.story(dir.path())
        .env("STORYHOOK_INVOKER", "legacy")
        .args(["list"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("is not a storyhook backend"))
        .stderr(predicates::str::contains("`local`"));
}

/// A command through the daemon fires the project's hooks, exactly as one in
/// this process does. The daemon runs the same services; the hop must not be
/// the reason a user's automation stops happening.
#[test]
fn hooks_still_fire_through_the_daemon() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let dir = project(&env);
    let marker = dir.path().join("hook-ran");
    write_hooks(
        dir.path(),
        &format!(
            "[hooks.on_create]\ncommand = \"cat > {}\"\n",
            marker.display()
        ),
    );

    let out = via_daemon(&env, dir.path(), &["new", "Fire the hook"]);
    assert!(out.status.success(), "{out:?}");

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !marker.exists() {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        marker.exists(),
        "the daemon must fire the project's hooks; nothing was written to {}",
        marker.display()
    );
}

/// A relative `PATH` names a directory relative to the **client's** working
/// directory, not the daemon's.
///
/// This is the one place where "the transport is invisible" needs an argument
/// rather than an assertion of sameness. The daemon's own working directory is
/// an accident of how it was spawned; if `story project init ./sub` resolved
/// there, a project would still be created and the command would still
/// succeed — just somewhere nobody named. The failure has no symptom at the
/// call site, which is exactly why it needs a test.
#[test]
fn a_relative_path_is_resolved_against_the_clients_directory_over_the_daemon() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let here = scratch_dir();
    let sub = here.path().join("sub");
    std::fs::create_dir_all(&sub).expect("creating the target");

    // Start the daemon from somewhere else entirely, so that resolving against
    // its cwd would land outside `here` and be visible.
    let elsewhere = scratch_dir();
    let started = via_daemon(&env, elsewhere.path(), &["project", "list"]);
    assert!(started.status.success(), "{started:?}");

    let out = via_daemon(&env, here.path(), &["project", "init", "./sub"]);
    assert!(out.status.success(), "{out:?}");

    assert!(
        sub.join(".storyhook.toml").exists(),
        "the project must be created under the directory the client ran in"
    );
    assert!(
        !elsewhere.path().join(".storyhook.toml").exists(),
        "nothing may be created relative to the daemon's own working directory"
    );
}

/// `story project init` produces the same bytes over both transports.
#[test]
fn project_init_answers_identically_through_both_invokers() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let local_dir = scratch_dir();
    let daemon_dir = scratch_dir();

    let local = via_local(
        &env,
        local_dir.path(),
        &["project", "init", "--no-agents-md"],
    );
    let daemon = via_daemon(
        &env,
        daemon_dir.path(),
        &["project", "init", "--no-agents-md"],
    );

    assert_eq!(local.status.code(), daemon.status.code());
    assert_eq!(
        String::from_utf8_lossy(&local.stdout),
        String::from_utf8_lossy(&daemon.stdout)
    );
    assert_eq!(
        String::from_utf8_lossy(&local.stderr),
        String::from_utf8_lossy(&daemon.stderr)
    );
}
