//! Running commands through the daemon.
//!
//! The transport's job is to be invisible, and this file used to check that by
//! running each command twice — once over `/api/v1/invoke`, once in this process
//! — and comparing the bytes. There is one transport now, so there is nothing to
//! compare against, and the byte contract is held instead by
//! `tests/golden_cli.rs`, whose ~130 invocations are frozen against snapshots
//! taken while the other transport still existed. That is the stronger form of
//! the same claim: frozen bytes rather than two things agreeing with each other.
//!
//! What is left here is what a snapshot cannot hold — the things that are true
//! of the *hop* rather than of the rendering:
//!
//! - a command starts a daemon when none is running, and the write lands in the
//!   store rather than somewhere the daemon kept to itself;
//! - a hook that runs `story` terminates, because the recursion depth travels in
//!   the envelope rather than in a child's environment;
//! - `--no-hooks` crosses the wire;
//! - a relative path names a directory relative to the *client's* working
//!   directory, not the daemon's;
//! - and the two destructive verbs' confirmation, which happens in the client
//!   and whose second request has to be recognized on the far side.

use std::time::{Duration, Instant};

use storyhook::store::{ReadOps, Store, StoryQuery};
use storyhook_test_support::{TestEnv, project_id_at, scratch_dir, story_binary};

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
        self.0.stop_daemon();
    }
}

/// Runs `story`, which is to say: through the daemon, because there is nowhere
/// else for it to go.
fn via_daemon(env: &TestEnv, cwd: &std::path::Path, args: &[&str]) -> std::process::Output {
    env.story(cwd).args(args).output().expect("running story")
}

/// A project to run commands in.
fn project(env: &TestEnv) -> tempfile::TempDir {
    let dir = scratch_dir();
    let out = via_daemon(
        env,
        dir.path(),
        &["project", "new", "--prefix", "SH", "--no-agents-md"],
    );
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

    // The fixture's own `project init` started one, so the question is asked
    // from a known state rather than from an assumption about one.
    env.stop_daemon();
    assert!(
        !env.daemon_is_live(),
        "the fixture must not be holding the store when the next command runs"
    );

    let out = via_daemon(&env, dir.path(), &["new", "Through the daemon"]);
    assert!(out.status.success(), "{out:?}");
    assert!(
        env.daemon_is_live(),
        "a command with no daemon running must start one"
    );

    // And the write really landed in the shared store, not somewhere the daemon
    // kept to itself. Read by a process that opens the database directly, with
    // the daemon stood down — asking the daemon whether it wrote something is a
    // question its own page cache can answer either way.
    env.stop_daemon();
    let store = env.open_store();
    let id = project_id_at(&store, dir.path()).expect("the project resolves");
    let titles: Vec<String> = store
        .read(|tx| {
            Ok(tx
                .stories(id, &StoryQuery::all())?
                .into_iter()
                .map(|row| row.snapshot.title)
                .collect())
        })
        .expect("reading the store");
    assert_eq!(titles, vec!["Through the daemon".to_string()]);
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
    let listed = via_daemon(&env, dir.path(), &["list", "--json"]);
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

    let listed = via_daemon(&env, dir.path(), &["list", "--json"]);
    let json: serde_json::Value = serde_json::from_slice(&listed.stdout).expect("a JSON listing");
    assert_eq!(
        json["stories"].as_array().expect("stories").len(),
        1,
        "`--no-hooks` must suppress the hook on the far side of the wire too"
    );
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

    let out = via_daemon(
        &env,
        here.path(),
        &["project", "new", "--prefix", "SH", "--attach", "./sub"],
    );
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

/// Delete's two-step — the plan, then the forced deletion — over the wire.
///
/// This is the case the seam most needs to cover, because the confirmation
/// happens in the *client* while the deletion happens in the daemon. Its
/// byte-for-byte agreement with an in-process run used to be the assertion;
/// with one transport there is nothing to agree with, and what is left is the
/// half a snapshot cannot hold: a refusal that carries the plan, a second
/// request the far side recognizes as forced, and a project resolved from a
/// working directory only the client knows about.
#[test]
fn delete_refuses_and_then_deletes_over_the_wire() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let dir = project(&env);

    // Unforced: a refusal naming --force, with nothing deleted.
    let refusal = via_daemon(&env, dir.path(), &["project", "delete"]);
    assert_eq!(refusal.status.code(), Some(2), "{refusal:?}");
    assert!(String::from_utf8_lossy(&refusal.stderr).contains("--force"));

    // Forced: the daemon resolved the project from the *client's* working
    // directory, which is the part that has no equivalent inside one process —
    // the daemon's own cwd is wherever it was spawned and knows nothing of this
    // checkout.
    let forced = via_daemon(&env, dir.path(), &["project", "delete", "--force"]);
    assert_eq!(forced.status.code(), Some(0), "{forced:?}");
    let listing = via_daemon(&env, dir.path(), &["project", "list"]);
    assert!(
        !String::from_utf8_lossy(&listing.stdout).contains(&dir.path().display().to_string()),
        "the project the client stood in is the one that went"
    );
    assert!(
        dir.path().join(".storyhook.toml").exists(),
        "and the daemon writes nothing into the client's repository"
    );
}

/// So is the purge's, and for a reason the delete case cannot cover.
///
/// `ConfirmationPlan` is an enum since SH-130, so the plan a purge answers with
/// has to survive the JSON hop that `/api/v1/invoke` gives it *and* be
/// recognised by `InvokeRequest::forced()`, which is what turns the client's
/// answer back into a second request. A `forced()` that only knew about `delete`
/// would loop forever here rather than fail: the daemon would keep answering
/// with the plan and the client would keep asking.
#[test]
fn purge_refuses_and_then_deletes_over_the_wire() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let dir = project(&env);

    assert!(
        via_daemon(&env, dir.path(), &["new", "Created in error"])
            .status
            .success()
    );
    assert!(
        via_daemon(&env, dir.path(), &["delete", "SH-1", "created in error"])
            .status
            .success()
    );

    // Unforced: a refusal naming --force, with the plan intact and nothing
    // purged. The title is what proves the plan crossed as data rather than as
    // a bare "are you sure".
    let refusal = via_daemon(&env, dir.path(), &["purge", "SH-1"]);
    assert_eq!(refusal.status.code(), Some(2), "{refusal:?}");
    let stderr = String::from_utf8_lossy(&refusal.stderr);
    assert!(stderr.contains("--force"), "{stderr}");
    assert!(
        stderr.contains("Created in error"),
        "the plan crossed the wire intact: {stderr}"
    );
    assert!(
        via_daemon(&env, dir.path(), &["show", "SH-1"])
            .status
            .success()
    );

    // Forced: the second request is recognized, and the story is gone.
    let forced = via_daemon(&env, dir.path(), &["purge", "SH-1", "--force"]);
    assert_eq!(forced.status.code(), Some(0), "{forced:?}");
    assert!(
        !via_daemon(&env, dir.path(), &["show", "SH-1"])
            .status
            .success(),
        "the story is gone"
    );
}
