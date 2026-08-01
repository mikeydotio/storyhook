//! Starting the daemon, finding it, and being sure it is the right one.
//!
//! These run the real thing: a detached `story daemon --serve` process, its
//! portfile, its pidfile lock, and HTTP against the port it published. The unit
//! tests in `daemon::lifecycle` cover the pieces; this covers the claim that the
//! pieces add up to a daemon.
//!
//! Every test gets its own environment, so every test gets its own daemon, and
//! `STORYHOOK_DAEMON_ADDR=127.0.0.1:0` means none of them can bind the port a
//! developer's own dashboard is on.

use std::time::{Duration, Instant};

use storyhook::daemon::lifecycle::{self, DaemonInfo};
use storyhook_test_support::{TestEnv, scratch_dir, story_binary};

/// Whether `info` describes a daemon running the `story` binary this build
/// produced.
///
/// `DaemonInfo::is_this_binary` asks the same question of the *calling*
/// process, which is exactly right in production — the caller is `story` — and
/// exactly wrong here, where the caller is a test binary in `deps/`. So the
/// comparison is made against the binary the harness runs instead.
fn is_the_binary_under_test(info: &DaemonInfo) -> bool {
    let expected_mtime = std::fs::metadata(story_binary())
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);
    info.exe == story_binary() && Some(info.exe_mtime) == expected_mtime
}

/// Stops whatever daemon `env` is running, even if the test panics first.
///
/// A daemon is a detached process, so nothing else reaps it: the parent-pid
/// contract catches a killed test *binary*, and this catches a failed test
/// inside a binary that keeps going.
struct DaemonGuard<'a>(&'a TestEnv);

impl Drop for DaemonGuard<'_> {
    fn drop(&mut self) {
        let _ = lifecycle::stop(&self.0.environment());
    }
}

/// Starts a daemon in `env` and returns what it published about itself.
fn start(env: &TestEnv) -> DaemonInfo {
    let dir = scratch_dir();
    env.story(dir.path())
        .args(["daemon", "start"])
        .assert()
        .success();
    env.daemon()
        .expect("a started daemon must publish a portfile")
}

/// Blocks until `ready`, or fails the test.
fn wait_for(what: &str, ready: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if ready() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out waiting for {what}");
}

/// A `GET /api/v1/hello` with the given token, returning the status.
fn hello_status(info: &DaemonInfo, token: &str) -> u16 {
    let url = format!("http://127.0.0.1:{}/api/v1/hello", info.port);
    match ureq::get(&url).header("X-Storyhook-Token", token).call() {
        Ok(response) => response.status().as_u16(),
        Err(ureq::Error::StatusCode(code)) => code,
        Err(other) => panic!("the daemon did not answer: {other}"),
    }
}

#[test]
fn starting_publishes_a_portfile_the_daemon_actually_answers_on() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let info = start(&env);

    assert!(info.port > 0, "the portfile must name the bound port");
    assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
    assert!(is_the_binary_under_test(&info));
    assert_eq!(hello_status(&info, &info.token), 200);
}

/// The port is written *after* the bind, so it is the port the kernel gave —
/// not the one that was asked for. The harness asks for 0, so a portfile
/// carrying 0 would mean the file was written from the request rather than from
/// the socket.
#[test]
fn the_published_port_is_the_one_that_was_bound() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let info = start(&env);

    assert_ne!(
        info.port, 0,
        "a portfile written before the bind would carry the requested port"
    );
    let reachable = std::net::TcpStream::connect(("127.0.0.1", info.port));
    assert!(
        reachable.is_ok(),
        "nothing is listening on the published port"
    );
}

#[test]
fn the_token_is_required_and_is_not_guessable_from_the_portfile_alone() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let info = start(&env);

    assert_eq!(hello_status(&info, ""), 401);
    assert_eq!(hello_status(&info, "not-the-token"), 401);
    assert_eq!(hello_status(&info, &info.token), 200);
}

/// Liveness is the held lock. A portfile is a claim; the lock is the fact.
#[test]
fn a_running_daemon_holds_its_pidfile_and_a_stopped_one_does_not() {
    let env = TestEnv::isolated();
    let guard = DaemonGuard(&env);
    let _info = start(&env);
    assert!(lifecycle::is_live(&env.environment()));

    drop(guard);
    wait_for("the daemon to release its pidfile", || {
        !lifecycle::is_live(&env.environment())
    });
}

#[test]
fn starting_twice_yields_one_daemon() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let first = start(&env);
    let second = start(&env);

    assert_eq!(
        first.pid, second.pid,
        "the second start must find the first daemon rather than add one"
    );
    assert_eq!(first.port, second.port);
}

#[test]
fn stopping_reports_what_it_stopped_and_clears_the_portfile() {
    let env = TestEnv::isolated();
    let info = start(&env);
    let dir = scratch_dir();

    env.story(dir.path())
        .args(["daemon", "stop"])
        .assert()
        .success()
        .stdout(predicates::str::contains(format!("PID {}", info.pid)));

    assert!(!lifecycle::is_live(&env.environment()));
    assert!(
        env.daemon().is_none(),
        "a stopped daemon's portfile must not go on describing it"
    );
}

#[test]
fn stopping_nothing_says_so_and_succeeds() {
    let env = TestEnv::isolated();
    let dir = scratch_dir();
    env.story(dir.path())
        .args(["daemon", "stop"])
        .assert()
        .success()
        .stdout(predicates::str::contains("not running"));
}

/// The backups are reported by `daemon status` rather than by `doctor`: a
/// project's integrity and a machine's copies of the database are different
/// questions, and only one of them has an exit code that means something.
#[test]
fn status_reports_the_backups() {
    let env = TestEnv::isolated();
    let dir = scratch_dir();

    env.story(dir.path())
        .args(["daemon", "status"])
        .assert()
        .success()
        .stdout(predicates::str::contains("backups:"));

    let _guard = DaemonGuard(&env);
    start(&env);
    env.story(dir.path())
        .args(["daemon", "status"])
        .assert()
        .success()
        .stdout(predicates::str::contains("backups: 1"));
}

#[test]
fn status_describes_a_running_daemon_and_an_absent_one() {
    let env = TestEnv::isolated();
    let dir = scratch_dir();

    env.story(dir.path())
        .args(["daemon", "status"])
        .assert()
        .success()
        .stdout(predicates::str::contains("not running"));

    let _guard = DaemonGuard(&env);
    let info = start(&env);
    env.story(dir.path())
        .args(["daemon", "status"])
        .assert()
        .success()
        .stdout(predicates::str::contains(format!("PID {}", info.pid)))
        .stdout(predicates::str::contains(info.port.to_string()));
}

/// A daemon started outside any project must still start: it serves every
/// project on the machine, so standing in one of them is not a precondition.
#[test]
fn the_daemon_starts_outside_a_project() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let nowhere = scratch_dir();
    env.story(nowhere.path())
        .args(["daemon", "start"])
        .assert()
        .success();
    assert!(env.daemon().is_some());
}

/// **The suicide contract.** A daemon whose named parent goes away exits, so a
/// test binary that is killed cannot leave one behind to answer the next run's
/// requests out of a store that no longer exists.
#[test]
fn a_daemon_does_not_outlive_the_process_that_named_itself_its_parent() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let dir = scratch_dir();

    // A real process to stand in for a test binary, so the pid is one that
    // genuinely exists and then genuinely stops existing.
    let mut parent = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawning a stand-in parent");
    let parent_pid = parent.id();

    env.story(dir.path())
        .env("STORYHOOK_PARENT_PID", parent_pid.to_string())
        .args(["daemon", "start"])
        .assert()
        .success();
    assert!(lifecycle::is_live(&env.environment()));

    parent.kill().expect("killing the stand-in parent");
    parent.wait().expect("reaping the stand-in parent");

    wait_for("the orphaned daemon to exit", || {
        !lifecycle::is_live(&env.environment())
    });
}

/// **Version skew.** A daemon serving a different build must not be used, and
/// the check is on the executable's mtime as well as the version — a developer
/// rebuilding the same version is the common case, and the one that produced a
/// dashboard serving 42-hour-old code.
#[test]
fn a_daemon_from_another_build_is_replaced_rather_than_reused() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let dir = scratch_dir();
    let first = start(&env);

    // Rewrite the portfile to describe a daemon from a different build, leaving
    // the running one in place. The next command must notice, stop it, and put
    // one of its own there.
    let mut stale = first.clone();
    stale.exe_mtime -= 1;
    let path = env.environment().daemon_file();
    std::fs::write(&path, serde_json::to_string_pretty(&stale).unwrap())
        .expect("rewriting the portfile");
    assert!(!is_the_binary_under_test(&stale));

    env.story(dir.path())
        .args(["daemon", "start"])
        .assert()
        .success();

    let replacement = env.daemon().expect("a portfile after the restart");
    assert!(
        is_the_binary_under_test(&replacement),
        "a daemon serving another build must be replaced, never reused"
    );
    assert_ne!(
        replacement.pid, first.pid,
        "the stale daemon must actually have been stopped"
    );
}

/// A portfile left behind by a daemon that crashed describes nothing. The lock
/// is what says whether something is running, so a start must succeed over it.
#[test]
fn a_portfile_without_a_daemon_does_not_stop_one_starting() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let dir = scratch_dir();

    let orphan = DaemonInfo {
        pid: 999_999,
        port: 1,
        version: env!("CARGO_PKG_VERSION").to_string(),
        protocol: lifecycle::PROTOCOL,
        exe: std::env::current_exe().unwrap(),
        exe_mtime: 0,
        started_at: "2026-01-01T00:00:00Z".to_string(),
        token: "stale".to_string(),
        store_path: env.environment().store_path().to_path_buf(),
    };
    let environment = env.environment();
    std::fs::create_dir_all(environment.daemon_state_dir()).unwrap();
    std::fs::write(
        environment.daemon_file(),
        serde_json::to_string_pretty(&orphan).unwrap(),
    )
    .unwrap();

    env.story(dir.path())
        .args(["daemon", "start"])
        .assert()
        .success();
    let info = env.daemon().expect("a portfile");
    assert_ne!(info.pid, orphan.pid);
    assert_eq!(hello_status(&info, &info.token), 200);
}

/// `story web start|stop|status` still work, still print what they printed, and
/// say on stderr where they moved. Scripts read stdout; humans read stderr.
#[test]
fn the_web_aliases_keep_their_output_and_announce_themselves_on_stderr() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let dir = scratch_dir();

    env.story(dir.path())
        .args(["web", "status"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Web UI is not running"))
        .stderr(predicates::str::contains("story daemon status"));

    env.story(dir.path())
        .args(["web", "start"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Web UI started at"))
        .stderr(predicates::str::contains("story daemon start"));

    let info = env.daemon().expect("the alias must start a real daemon");
    env.story(dir.path())
        .args(["web", "status"])
        .assert()
        .success()
        .stdout(predicates::str::contains(format!("PID {}", info.pid)));

    env.story(dir.path())
        .args(["web", "stop"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Web UI stopped"))
        .stderr(predicates::str::contains("story daemon stop"));
}
