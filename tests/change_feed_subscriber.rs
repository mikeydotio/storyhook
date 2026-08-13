//! [`storyhook::daemon::subscribe::Subscriber`] against a real, spawnable
//! daemon (SH-150).
//!
//! `src/daemon/subscribe.rs`'s own unit tests cover connecting, receiving a
//! change, and a dead connection surfacing as an error. What they cannot
//! cover end to end is a **real daemon restart** — stopping one process and
//! auto-spawning another exactly as an ordinary `story` command does — which
//! needs `CARGO_BIN_EXE_story`, a real binary only an integration test has.

use std::time::{Duration, Instant};

use storyhook::daemon::subscribe::Subscriber;
use storyhook_test_support::{TestEnv, scratch_dir};

/// Stops whatever daemon `env` is running, even if the test panics first.
struct DaemonGuard<'a>(&'a TestEnv);

impl Drop for DaemonGuard<'_> {
    fn drop(&mut self) {
        self.0.stop_daemon();
    }
}

fn wait_for(what: &str, deadline: Duration, ready: impl Fn() -> bool) {
    let start = Instant::now();
    while start.elapsed() < deadline {
        if ready() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out after {deadline:?} waiting for {what}");
}

/// A subscriber survives its daemon being stopped and a fresh one taking its
/// place: the reconnect it makes on its own finds the replacement rather than
/// retrying a port nothing answers on any more, and that connection is fully
/// live -- a write made after the restart is reported, not just the one-time
/// `Resync` the reconnect itself produces.
#[test]
fn a_subscriber_survives_its_daemon_restarting() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let project = env.project().prefix("PB").build();

    // Any command spawns the daemon this environment will use from here on.
    env.story(project.path()).args(["list"]).assert().success();
    let daemon = env.daemon().expect("a command must have spawned one");

    let mut subscriber = Subscriber::new(env.environment(), daemon, Duration::from_secs(20));
    subscriber
        .connect()
        .expect("connecting to the first daemon's change feed");

    env.stop_daemon();

    // A fresh daemon, auto-spawned by an ordinary command exactly as
    // production does it -- not started by hand, so this proves the same
    // path a real `story tui` session would take.
    env.story(project.path()).args(["list"]).assert().success();

    // The old connection is still open at the protocol level -- the daemon
    // publishes `Change::Reload` ahead of going down, which `Subscriber`
    // follows immediately, and only then does the socket actually close --
    // so "resynced" is deliberately driven from a fresh call each time
    // rather than asserted on the first `Some` alone: either path (the
    // `Reload`-triggered reconnect or the eventual close-triggered one)
    // ends at the same place, a live connection to the new daemon.
    let mut resynced = false;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline && !resynced {
        if subscriber.poll(Duration::from_millis(500)).is_some() {
            resynced = true;
        }
    }
    assert!(
        resynced,
        "the subscriber never reported a change after its daemon restarted"
    );

    // The replacement connection is not merely open -- it is subscribed.
    // Prove it the same way `src/daemon/subscribe.rs`'s own tests do: a
    // write made now must be reported.
    env.story(project.path())
        .args(["new", "a story after the restart"])
        .assert()
        .success();

    let mut reported = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && !reported {
        if subscriber.poll(Duration::from_millis(500)).is_some() {
            reported = true;
        }
    }
    assert!(
        reported,
        "a write made through the replacement daemon must reach the subscriber"
    );
}

/// A subscriber whose seed names a daemon that never starts must not hang,
/// and must never spawn one on its own -- `Subscriber::daemon_info` reads
/// the portfile and checks liveness, and never calls `lifecycle::ensure`
/// (which spawns), so there is no path from a dead seed to a daemon this
/// test never asked for.
#[test]
fn a_subscriber_seeded_with_a_dead_daemon_never_spawns_one() {
    let env = TestEnv::isolated();
    // Deliberately never spawned: the story CLI never runs, so a daemon
    // appearing at all would mean something here spawned one.
    let dir = scratch_dir();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a throwaway listener");
    let port = listener.local_addr().expect("the bound address").port();
    drop(listener);

    let daemon = storyhook::daemon::lifecycle::DaemonInfo {
        pid: std::process::id(),
        port,
        version: env!("CARGO_PKG_VERSION").to_string(),
        protocol: 1,
        exe: std::env::current_exe().expect("this test binary"),
        exe_mtime: 0,
        started_at: "2026-01-01T00:00:00Z".to_string(),
        token: "unused".to_string(),
        store_path: dir.path().join("store.db"),
        tailnet: None,
        cookie_name: "storyhook_unused".to_string(),
    };

    let environment = storyhook::env::Environment::at(dir.path());
    let mut subscriber = Subscriber::new(environment, daemon, Duration::from_secs(20));
    assert!(
        subscriber.connect().is_err(),
        "nothing listens on the throwaway port"
    );
    // A few bounded polls, not an unbounded loop: this is the same
    // "must not hang" assertion `event.rs`'s test makes, and if it were ever
    // going to spawn something it would have by now.
    for _ in 0..3 {
        subscriber.poll(Duration::from_millis(200));
    }

    wait_for(
        "no orphan daemon to appear",
        Duration::from_millis(500),
        || env.daemon().is_none(),
    );
}
