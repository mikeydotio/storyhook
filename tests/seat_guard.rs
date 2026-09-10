//! An uninstalled build is refused the default store's daemon seat (SH-634).
//!
//! The incident: a `PATH=target/debug:$PATH story list` from a checkout stood
//! down the installed daemon on the production store and seated a worktree
//! debug build in its place; the next installed `story` seated the installed
//! build back; four custodians in one hour. The guard
//! (`storyhook::daemon::seat_guard`) refuses the replacement *before* the
//! shutdown request, and refuses the bare invocation — nothing naming a store
//! — a seat at all.
//!
//! Two binaries, one build, as in `tests/migration_guard.rs`: the test binary
//! itself, which is uninstalled by construction (its lease sits inside the
//! directory `build.rs` stamped), and `installed_copy()`, the same bytes copied
//! out of that directory — what `make install` does and the only thing it
//! does. A daemon started by one is "another build" to the other, because
//! `DaemonInfo::is_this_binary` compares the executable path, so no portfile
//! is ever doctored here.
//!
//! | client | incumbent | store | outcome |
//! |---|---|---|---|
//! | test binary | the copy's daemon | default-shaped | refused, exit 2, daemon untouched — the incident |
//! | test binary, `daemon restart` | the copy's daemon | default-shaped | refused, daemon untouched |
//! | the copy | the test binary's daemon | default-shaped | replaced — the `make install` skew restart, the control |
//! | test binary, override set | the copy's daemon | default-shaped | replaced — today's behaviour, on purpose |
//! | test binary | the copy's daemon | `--store-path`, not the default | replaced — a named store belongs to whoever names it |
//! | test binary, in-process | none | `Environment::at`, nothing named | refused, nothing spawned — the bare invocation |
//!
//! **Why the refusal rows check what is left afterwards.** SH-411's lesson:
//! a gate one line too low — after `request_shutdown` — is indistinguishable
//! from a correct one in the exit code and the message. The only observable
//! that separates them is whether the incumbent is still serving, so every
//! refusal row asks it to.

use std::path::Path;

use storyhook::daemon::lifecycle::{self, DaemonInfo};
use storyhook::daemon::seat_guard::OVERRIDE_VAR;
use storyhook::env::Environment;
use storyhook_test_support::{
    TestEnv, installed_copy, path_without_tailscale, scratch_dir, story_binary,
};

/// Stops whatever daemon `env` is running, even if the test panics first.
struct DaemonGuard<'a>(&'a TestEnv);

impl Drop for DaemonGuard<'_> {
    fn drop(&mut self) {
        let _ = lifecycle::stop(&self.0.environment(), lifecycle::StopMode::Force);
    }
}

/// Whether `info` names the executable at `exe` — the question
/// `DaemonInfo::is_this_binary` asks of the *calling* process, asked here of a
/// path, because the caller is a test binary in `deps/`.
fn serves_from(info: &DaemonInfo, exe: &Path) -> bool {
    info.exe == exe
}

/// A daemon started by the installed copy, under `path_without_tailscale`
/// with the copy's own directory first — the installed machine's shape.
fn start_installed_daemon(env: &TestEnv, cwd: &Path) -> DaemonInfo {
    let (_no_tailscale, path) = path_without_tailscale(env);
    let mut path_entries = vec![
        installed_copy()
            .parent()
            .expect("the copy has a directory")
            .to_path_buf(),
    ];
    path_entries.extend(std::env::split_paths(&path));
    let out = env
        .raw_installed_story(cwd)
        .env(
            "PATH",
            std::env::join_paths(path_entries).expect("joining PATH"),
        )
        .args(["daemon", "start"])
        .output()
        .expect("running the installed copy");
    assert!(
        out.status.success(),
        "the installed copy must start a daemon: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let info = env
        .daemon()
        .expect("the copy's daemon publishes a portfile");
    assert!(
        serves_from(&info, installed_copy()),
        "positive control: the incumbent must be the installed copy's daemon, got {}",
        info.exe.display()
    );
    info
}

/// A daemon started by the test binary — the uninstalled shape.
fn start_uninstalled_daemon(env: &TestEnv, cwd: &Path) -> DaemonInfo {
    let (_no_tailscale, path) = path_without_tailscale(env);
    let out = env
        .raw_story(cwd)
        .env("PATH", &path)
        .args(["daemon", "start"])
        .output()
        .expect("running the test binary");
    assert!(
        out.status.success(),
        "the test binary must start a daemon on a fresh store: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let info = env.daemon().expect("a portfile");
    assert!(serves_from(&info, story_binary()));
    info
}

/// The assertions every refusal shares: exit 2, the store and the incumbent
/// named, every way out named, `story update` not, and the incumbent left
/// exactly as it was — same token, still live, still answering.
fn assert_refused_and_untouched(env: &TestEnv, incumbent: &DaemonInfo, out: &std::process::Output) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "expected a refusal, got success:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(
        out.status.code(),
        Some(2),
        "a refused seat is a usage failure, like the migration guard's: {stderr}"
    );
    for needle in [
        env.store_path().display().to_string(),
        format!("pid {}", incumbent.pid),
        incumbent.exe.display().to_string(),
        "Nothing was stopped".to_string(),
        "make scratch".to_string(),
        "--store-path".to_string(),
        "make install".to_string(),
        OVERRIDE_VAR.to_string(),
    ] {
        assert!(stderr.contains(&needle), "missing {needle:?} in: {stderr}");
    }
    assert!(
        !stderr.contains("story update"),
        "SH-405's dead end: {stderr}"
    );

    let after = env
        .daemon()
        .expect("the incumbent's portfile is still there");
    assert_eq!(
        after.token, incumbent.token,
        "the incumbent must not have been replaced (a fresh token means a fresh daemon)"
    );
    assert_eq!(after.exe, incumbent.exe);
    assert!(
        env.daemon_is_live(),
        "the incumbent must still hold the store"
    );
    assert!(
        lifecycle::hello(&after).is_ok(),
        "the incumbent must still answer — a refusal after the shutdown request would leave \
         a live lock and a dead daemon"
    );
    assert!(
        !env.environment().daemon_attempt().exists(),
        "a refusal about this client's own build must not be published for an installed \
         waiter to adopt"
    );
}

// ---------------------------------------------------------------------------
// The refusals
// ---------------------------------------------------------------------------

/// The incident: the installed daemon is serving, and an uninstalled build
/// with its own directory first on `$PATH` runs an ordinary command.
#[test]
fn an_uninstalled_build_is_refused_the_seat_an_installed_daemon_holds() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let cwd = scratch_dir();
    let incumbent = start_installed_daemon(&env, cwd.path());

    // `env.raw_story` already puts the test binary's directory first on PATH —
    // the SH-630 invocation.
    let out = env
        .raw_story(cwd.path())
        .args(["project", "list"])
        .output()
        .expect("running the test binary");
    assert_refused_and_untouched(&env, &incumbent, &out);

    // An explicit start is the same seat.
    let out = env
        .raw_story(cwd.path())
        .args(["daemon", "start"])
        .output()
        .expect("running the test binary");
    assert_refused_and_untouched(&env, &incumbent, &out);

    // The installed copy is unaffected: the daemon it left is still its own.
    let out = env
        .raw_installed_story(cwd.path())
        .args(["project", "list"])
        .output()
        .expect("running the installed copy");
    assert!(
        out.status.success(),
        "the installed copy must still be served by its own daemon: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let still = env.daemon().expect("portfile");
    assert_eq!(still.token, incumbent.token, "and by the same daemon");
}

/// `story daemon restart` is the other seat: it stops the incumbent and
/// spawns the caller's binary on the same port. Refused before the stop.
#[test]
fn an_uninstalled_build_is_refused_a_restart_of_an_installed_daemon() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let cwd = scratch_dir();
    let incumbent = start_installed_daemon(&env, cwd.path());

    let out = env
        .raw_story(cwd.path())
        .args(["daemon", "restart"])
        .output()
        .expect("running the test binary");
    assert_refused_and_untouched(&env, &incumbent, &out);
}

/// The bare invocation — nothing naming a store — refused with nothing
/// running. Reached in-process because a test build refuses an `XdgDefault`
/// origin at `Environment::from_process` before any guard can run, while
/// `Environment::at` builds exactly that origin for a fixture.
#[test]
fn the_bare_invocation_is_refused_a_seat_with_no_daemon_running() {
    let home = scratch_dir();
    let env = Environment::at(home.path());
    assert_eq!(
        env.store().origin(),
        storyhook::env::StoreOrigin::XdgDefault,
        "positive control: nothing named this store"
    );

    let error = lifecycle::ensure(&env).expect_err("an uninstalled build must be refused");
    let text = error.to_string();
    for needle in [
        env.store_path().display().to_string(),
        "No daemon was started".to_string(),
        "make scratch".to_string(),
        "--store-path".to_string(),
        OVERRIDE_VAR.to_string(),
    ] {
        assert!(text.contains(&needle), "missing {needle:?} in: {text}");
    }
    assert!(!text.contains("story update"));
    assert!(
        matches!(error, storyhook::error::AppError::Usage(_)),
        "{error}"
    );

    assert!(
        !env.daemon_file().exists(),
        "no daemon may have been started: a portfile was published"
    );
    assert!(!lifecycle::is_live(&env), "no daemon may hold the pidfile");
    assert!(
        !env.daemon_attempt().exists(),
        "a refusal is not an attempt for a waiter to adopt"
    );
}

// ---------------------------------------------------------------------------
// The permits
// ---------------------------------------------------------------------------

/// The control, and the `make install` / `story update` skew restart: an
/// installed binary finds a daemon of another build and replaces it.
#[test]
fn an_installed_binary_replaces_a_daemon_of_another_build() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let cwd = scratch_dir();
    let first = start_uninstalled_daemon(&env, cwd.path());

    let out = env
        .raw_installed_story(cwd.path())
        .args(["daemon", "start"])
        .output()
        .expect("running the installed copy");
    assert!(
        out.status.success(),
        "an installed binary must replace a daemon from another build: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let replacement = env.daemon().expect("a portfile after the replacement");
    assert!(
        serves_from(&replacement, installed_copy()),
        "the replacement must be the installed copy's daemon, got {}",
        replacement.exe.display()
    );
    assert_ne!(
        replacement.token, first.token,
        "the incumbent must actually have been replaced"
    );
    assert!(env.daemon_is_live());
}

/// The override is the sanctioned "this build, this store, on purpose":
/// today's behaviour, chosen rather than fallen into.
#[test]
fn the_override_lets_an_uninstalled_build_take_the_seat_on_purpose() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let cwd = scratch_dir();
    let incumbent = start_installed_daemon(&env, cwd.path());

    let out = env
        .raw_story(cwd.path())
        .env(OVERRIDE_VAR, "1")
        .args(["daemon", "start"])
        .output()
        .expect("running the test binary");
    assert!(
        out.status.success(),
        "the override must permit the replacement: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let replacement = env.daemon().expect("a portfile");
    assert!(serves_from(&replacement, story_binary()));
    assert_ne!(replacement.token, incumbent.token);
}

/// A store named with `--store-path` somewhere other than the default belongs
/// to whoever names it — a scratch store, a second tracker — and is never
/// guarded, incumbent or not.
#[test]
fn a_store_named_away_from_the_default_is_not_guarded() {
    let env = TestEnv::isolated();
    let cwd = scratch_dir();
    let store_dir = scratch_dir();
    let store = store_dir.path().join("elsewhere.db");
    let store_arg = store.display().to_string();
    let named = env.environment().with_store(
        storyhook::env::StoreLocation::resolve(
            Some(&store),
            &storyhook::env::StoreVars::default(),
            env.home(),
        )
        .expect("resolving the named store"),
    );
    struct Stop<'a>(&'a Environment);
    impl Drop for Stop<'_> {
        fn drop(&mut self) {
            let _ = lifecycle::stop(self.0, lifecycle::StopMode::Force);
        }
    }
    let _stop = Stop(&named);

    let out = env
        .raw_installed_story(cwd.path())
        .args(["--store-path", &store_arg, "daemon", "start"])
        .output()
        .expect("running the installed copy");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let incumbent = lifecycle::read_info(&named).expect("the copy's daemon on the named store");
    assert!(serves_from(&incumbent, installed_copy()));

    let out = env
        .raw_story(cwd.path())
        .args(["--store-path", &store_arg, "daemon", "start"])
        .output()
        .expect("running the test binary");
    assert!(
        out.status.success(),
        "a named, non-default store is the caller's own: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let replacement = lifecycle::read_info(&named).expect("a portfile");
    assert!(serves_from(&replacement, story_binary()));
    assert_ne!(replacement.token, incumbent.token);
}

// ---------------------------------------------------------------------------
// `story daemon status` tells the truth about what the next command will do
// ---------------------------------------------------------------------------

/// The status line used to promise "the next command will restart it" for any
/// daemon of another build. From an uninstalled build that promise is now
/// false, and the one place a stale incumbent is named to a person must say
/// what will actually happen — and how to get through.
#[test]
fn daemon_status_says_the_next_command_will_be_refused_rather_than_restart() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let cwd = scratch_dir();
    let incumbent = start_installed_daemon(&env, cwd.path());

    let out = env
        .raw_story(cwd.path())
        .args(["daemon", "status"])
        .output()
        .expect("running the test binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("will be refused"),
        "status from an uninstalled build must not promise a restart: {stdout}"
    );
    assert!(
        !stdout.contains("the next command will restart it"),
        "the old promise is a lie here: {stdout}"
    );
    for needle in ["make scratch", "--store-path", OVERRIDE_VAR] {
        assert!(stdout.contains(needle), "missing {needle:?} in: {stdout}");
    }
    // Reading status is not a seat: nothing changed.
    let still = env.daemon().expect("portfile");
    assert_eq!(still.token, incumbent.token);
    assert!(env.daemon_is_live());
}

/// The control: an installed binary looking at a daemon of another build is
/// still told the truth it was always told.
#[test]
fn daemon_status_from_an_installed_binary_still_promises_the_restart() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let cwd = scratch_dir();
    start_uninstalled_daemon(&env, cwd.path());

    let out = env
        .raw_installed_story(cwd.path())
        .args(["daemon", "status"])
        .output()
        .expect("running the installed copy");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("the next command will restart it"),
        "an installed binary replaces a stale daemon, and status says so: {stdout}"
    );
    assert!(!stdout.contains("will be refused"), "{stdout}");
}
