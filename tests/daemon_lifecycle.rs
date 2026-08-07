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
        let _ = lifecycle::stop(&self.0.environment(), lifecycle::StopMode::Force);
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

/// `story daemon token` (SH-50) — how an operator gets a copy of the bearer
/// token into the dashboard's own token prompt without reading the 0600
/// portfile by hand. Prints exactly what the portfile holds, and what it
/// prints actually gates `/api/v1/*`.
#[test]
fn daemon_token_prints_the_portfiles_token_and_it_actually_works() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let dir = scratch_dir();
    let info = start(&env);

    env.story(dir.path())
        .args(["daemon", "token"])
        .assert()
        .success()
        .stdout(predicates::str::contains(&info.token));

    assert_eq!(hello_status(&info, &info.token), 200);
}

/// Refuses rather than silently starting a daemon: `token` is a question
/// about a daemon presumably already serving the dashboard the token is for,
/// so a caller with none running gets told to start one, not handed a token
/// for a daemon it never asked for.
#[test]
fn daemon_token_refuses_when_nothing_is_running() {
    let env = TestEnv::isolated();
    let dir = scratch_dir();

    env.story(dir.path())
        .args(["daemon", "token"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("not running"));

    assert!(
        env.daemon().is_none(),
        "`daemon token` must not have started a daemon as a side effect"
    );
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

/// An unforced `daemon stop` waits for in-flight work to finish rather than
/// abandoning it — the whole point of the default not being `--force`.
///
/// The fixture holds a request open with a 2s hook; `daemon stop` must not
/// return before that hook does, and the daemon must actually be gone once
/// it does return (drained, not merely told to drain).
#[test]
fn an_unforced_stop_waits_for_in_flight_work_to_finish() {
    use std::io::Write;

    let env = TestEnv::isolated();
    let project = env.project().prefix("PB").build();
    let _guard = DaemonGuard(&env);

    std::fs::create_dir_all(project.path().join(".storyhook")).expect("the hooks directory");
    let mut hooks = std::fs::File::create(project.path().join(".storyhook/hooks.toml"))
        .expect("writing hooks.toml");
    hooks
        .write_all(
            b"[settings]\ntimeout_seconds = 60\n\n\
              [on_comment]\ncommand = \"sleep 2\"\ntimeout_seconds = 60\n",
        )
        .expect("writing the hook");
    drop(hooks);
    env.story(project.path())
        .args(["new", "a story"])
        .assert()
        .success();

    let mut slow = env.raw_story(project.path());
    let mut child = slow
        .args(["comment", "PB-1", "trip the hook"])
        .spawn()
        .expect("spawning the slow command");

    let environment = env.environment();
    wait_for(
        "the daemon to publish the slow comment as in flight",
        || {
            lifecycle::read_inflight(&environment)
                .iter()
                .any(|r| r.command == "comment")
        },
    );

    let started = Instant::now();
    env.story(project.path())
        .args(["daemon", "stop"])
        .assert()
        .success();
    let waited = started.elapsed();

    assert!(
        waited >= Duration::from_secs(2),
        "an unforced stop must wait for the 2s hook to finish, not abandon it: took {waited:?}"
    );
    assert!(
        !lifecycle::is_live(&environment),
        "the daemon must actually be gone once stop returns"
    );
    child.wait().expect("the slow command finishes");
}

/// `daemon stop --force` does not wait for a hook that outlives its grace
/// period — it signals the daemon directly once that period elapses.
///
/// The fixture's hook sleeps far longer than `FORCE_GRACE`, so a `--force`
/// stop that waited for it would time this test out; one that behaves
/// correctly returns within a few seconds of the grace period regardless.
#[test]
fn a_forced_stop_kills_a_daemon_that_is_still_draining() {
    use std::io::Write;

    let env = TestEnv::isolated();
    let project = env.project().prefix("PB").build();
    let _guard = DaemonGuard(&env);

    std::fs::create_dir_all(project.path().join(".storyhook")).expect("the hooks directory");
    let mut hooks = std::fs::File::create(project.path().join(".storyhook/hooks.toml"))
        .expect("writing hooks.toml");
    hooks
        .write_all(
            b"[settings]\ntimeout_seconds = 60\n\n\
              [on_comment]\ncommand = \"sleep 60\"\ntimeout_seconds = 60\n",
        )
        .expect("writing the hook");
    drop(hooks);
    env.story(project.path())
        .args(["new", "a story"])
        .assert()
        .success();

    let mut slow = env.raw_story(project.path());
    let mut child = slow
        .args(["comment", "PB-1", "trip the hook"])
        .spawn()
        .expect("spawning the slow command");

    let environment = env.environment();
    wait_for(
        "the daemon to publish the slow comment as in flight",
        || {
            lifecycle::read_inflight(&environment)
                .iter()
                .any(|r| r.command == "comment")
        },
    );

    let started = Instant::now();
    env.story(project.path())
        .args(["daemon", "stop", "--force"])
        .assert()
        .success();
    let waited = started.elapsed();

    assert!(
        waited < Duration::from_secs(10),
        "a forced stop must not wait for a 60s hook: took {waited:?}"
    );
    assert!(
        !lifecycle::is_live(&environment),
        "the daemon must actually be gone once a forced stop returns"
    );
    // The killed daemon's socket closes underneath it — this client fails
    // fast rather than waiting out its own deadline. Its exact error is not
    // this test's concern, only that it is reaped rather than left running.
    let _ = child.wait();

    // What --force abandoned is not just gone — it is ledgered, and `story
    // doctor abandoned` is how a human reviews and clears it.
    let listing = env
        .story(project.path())
        .args(["doctor", "abandoned"])
        .output()
        .expect("listing abandoned commands");
    let listing_text = String::from_utf8_lossy(&listing.stdout);
    assert!(
        listing_text.contains("comment"),
        "the killed comment must appear in the abandoned ledger: {listing_text}"
    );

    env.story(project.path())
        .args(["doctor", "abandoned", "clear", "--all"])
        .assert()
        .success();
    let cleared = env
        .story(project.path())
        .args(["doctor", "abandoned"])
        .output()
        .expect("listing again after clearing");
    assert!(
        String::from_utf8_lossy(&cleared.stdout).contains("no abandoned"),
        "clearing --all must actually empty the ledger"
    );
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
        tailnet: None,
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

/// A queue behind the spawn lock is one *attempt* deep, not one attempt deep
/// per client.
///
/// # The defect this replaces
///
/// Every client that could not use the daemon took the spawn lock and ran the
/// whole slow path — even when the client ahead of it had just run the same
/// path against the same broken world and reached an answer. Measured before the
/// fix, four concurrent clients against a wedged daemon returned at 10.16s,
/// 20.25s, 30.33s and 40.42s: strictly linear, and every one of them was told
/// what the first had already been told. `tests/concurrency_soak.rs` runs eight
/// (SH-143).
///
/// # The fixture
///
/// A daemon that cannot be replaced and will not answer: a portfile naming a
/// version this build is not — so `usable` is false and every client takes the
/// slow path — beside a pidfile lock held by *this test*, so `is_live` stays
/// true, the incumbent never stands down, and the child that gets spawned always
/// fails to claim the pidfile. The port belongs to nothing, so the shutdown
/// request is refused rather than hanging; the wedge under test here is the
/// queue, not the socket.
///
/// # The assertion calibrates itself
///
/// One client is timed first, and the wave of four must land inside twice that.
/// A hard-coded second count would be a claim about this machine; a multiple of
/// a measurement taken moments earlier on the same machine is a claim about the
/// shape, which is what changed. Before the fix the wave took four attempts and
/// fails this by a wide margin.
#[test]
fn four_clients_behind_a_wedged_daemon_share_one_attempt() {
    use std::sync::mpsc;

    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let _incumbent = wedge_the_daemon(&env);

    let run_one = || {
        let dir = scratch_dir();
        let started = Instant::now();
        let out = env
            .raw_story(dir.path())
            .args(["summary"])
            .output()
            .expect("running a client");
        (started.elapsed(), out)
    };

    // The first execution of a freshly built binary is not a measurement of
    // anything this test is about: macOS validates a new Mach-O on its first
    // exec, and that cost landed entirely on whichever client ran first —
    // measured at 32.5s against the 5.1s an attempt actually takes. Paid here,
    // outside the clock, so the comparison below is between two attempts rather
    // than between a cold start and a warm one.
    env.raw_story(scratch_dir().path())
        .arg("--version")
        .output()
        .expect("warming the binary");

    // One alone, to price a single attempt on this machine.
    let (alone, out) = run_one();
    assert!(
        !out.status.success(),
        "the fixture must make the attempt fail, or this measures nothing"
    );

    // Scoped threads rather than `spawn`, because `TestEnv` is deliberately not
    // `Clone` — one environment is one store, and handing copies of it around is
    // the mistake the type is shaped to prevent.
    let (tx, rx) = mpsc::channel();
    let wave = Instant::now();
    let successes: Vec<bool> = std::thread::scope(|scope| {
        for _ in 0..4 {
            let tx = tx.clone();
            let env = &env;
            scope.spawn(move || {
                let dir = scratch_dir();
                let out = env
                    .raw_story(dir.path())
                    .args(["summary"])
                    .output()
                    .expect("running a client");
                let _ = tx.send(out.status.success());
            });
        }
        drop(tx);
        rx.iter().collect()
    });
    let together = wave.elapsed();

    assert_eq!(successes.len(), 4, "every client must have reported");
    assert!(
        successes.iter().all(|ok| !ok),
        "the fixture must fail every client, or the timing proves nothing"
    );
    assert!(
        together < alone * 2,
        "four clients must share one attempt, not queue four of them: one alone \
         took {alone:?} and four together took {together:?}"
    );
}

/// A daemon that cannot be replaced and will not answer.
///
/// Three things together, and all three are needed: a portfile naming a version
/// this build is not, so `usable` is false and every client takes the slow path;
/// a pidfile lock held by the **test process**, so `is_live` stays true, the
/// incumbent never stands down, and the child that gets spawned always fails to
/// claim it; and a port nothing listens on, so the stand-down request is refused
/// at once rather than waiting out a control deadline — the wedge under test
/// here is the queue, not the socket.
///
/// The returned handle is the lock. Dropping it releases the incumbent, so a
/// caller holds it for as long as it wants the fixture to bite.
fn wedge_the_daemon(env: &TestEnv) -> std::fs::File {
    use fs4::FileExt;

    let environment = env.environment();
    std::fs::create_dir_all(environment.daemon_state_dir()).expect("the daemon directory");

    let pidfile = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(environment.daemon_pidfile())
        .expect("opening the pidfile");
    pidfile
        .try_lock_exclusive()
        .expect("this test must be the incumbent");

    let dead_port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("binding");
        listener.local_addr().expect("an address").port()
    };
    let wedged = DaemonInfo {
        pid: std::process::id(),
        port: dead_port,
        version: "0.0.0-not-this-build".to_string(),
        protocol: 1,
        exe: std::path::PathBuf::from("/nowhere/story"),
        exe_mtime: 0,
        started_at: "2026-01-01T00:00:00Z".to_string(),
        token: "t".to_string(),
        store_path: environment.store_path().to_path_buf(),
        tailnet: None,
    };
    std::fs::write(
        environment.daemon_file(),
        serde_json::to_string(&wedged).expect("serializing the portfile"),
    )
    .expect("writing the portfile");
    assert!(
        !wedged.is_this_binary(),
        "the fixture must make every client take the slow path"
    );
    pidfile
}

/// The refusal that used to be thrown away.
///
/// `spawn_locked` asked the incumbent to stand down and discarded the answer
/// with `let _ =`. What the user got was the *consequence* — "a storyhook daemon
/// is already running. Run `story daemon stop` first" — which is a remedy that
/// will itself hang against a daemon in this state, and which says nothing about
/// why the stand-down did not work. A daemon replies to a shutdown request
/// *before* it exits, so a request that did not come back is one the incumbent
/// never accepted, and that is the fact worth reporting (SH-143).
#[test]
fn a_refused_stand_down_is_named_above_the_failure_it_causes() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let _incumbent = wedge_the_daemon(&env);

    let dir = scratch_dir();
    let out = env
        .raw_story(dir.path())
        .args(["summary"])
        .output()
        .expect("running a client");
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(!out.status.success(), "the fixture must fail the command");
    assert!(
        stderr.contains("did not stand down"),
        "the cause must be reported, not swallowed: {stderr}"
    );
    assert!(
        stderr.contains("already running"),
        "and the consequence must survive alongside it: {stderr}"
    );
}

/// The fix must not be paid for out of the common case.
///
/// Every `story` command takes this path since SH-114, and the overwhelming
/// majority of them find a daemon already running or start one in a fifth of a
/// second — six concurrent clients against a startable daemon measured at
/// 0.20–0.22s each before any of this changed. Polling for the lock instead of
/// blocking on it, and consulting a verdict file on the way past, must stay
/// invisible there. The bound is generous against a loaded four-thread suite;
/// what it would catch is a design in which a follower waits out a deadline
/// rather than returning as soon as the daemon exists.
#[test]
fn concurrent_clients_against_a_startable_daemon_stay_fast() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);

    // Cold-start validation of a freshly built binary is not what this measures.
    env.raw_story(scratch_dir().path())
        .arg("--version")
        .output()
        .expect("warming the binary");

    let started = Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..6 {
            let env = &env;
            scope.spawn(move || {
                let dir = scratch_dir();
                // A store-wide query, so this measures reaching the daemon
                // rather than resolving a project from a scratch directory.
                let out = env
                    .raw_story(dir.path())
                    .args(["project", "list"])
                    .output()
                    .expect("running a client");
                assert!(
                    out.status.success(),
                    "a healthy client must succeed: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
            });
        }
    });
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(10),
        "six clients racing to start one daemon must not queue behind a deadline: \
         took {elapsed:?}"
    );
    assert!(
        env.daemon_is_live(),
        "and they must have produced exactly one daemon between them"
    );
}

/// The daemon says what it is serving, and stops saying it when it is done.
///
/// The half of SH-144 that lives in the daemon. Without this file a client has
/// no observable at all: the daemon writes no bytes until its handler returns,
/// and it serves one request at a time, so a second request cannot ask either.
///
/// The fixture is a project event hook that sleeps, which is the only way to
/// hold a real request open long enough to look at it — and it is the same
/// shape that measured the daemon's serial dispatch in the first place.
#[test]
fn a_running_command_is_published_and_retracted() {
    use std::io::Write;

    let env = TestEnv::isolated();
    let project = env.project().prefix("PB").build();
    let _guard = DaemonGuard(&env);

    std::fs::create_dir_all(project.path().join(".storyhook")).expect("the project directory");
    let mut hooks = std::fs::File::create(project.path().join(".storyhook/hooks.toml"))
        .expect("writing hooks.toml");
    hooks
        .write_all(b"[settings]\ntimeout_seconds = 60\n\n[on_comment]\ncommand = \"sleep 5\"\ntimeout_seconds = 60\n")
        .expect("writing the hook");
    drop(hooks);

    env.story(project.path())
        .args(["new", "a story"])
        .assert()
        .success();

    // Fire a command that will sit inside its hook, and look at the record
    // while it does.
    let mut slow = env.raw_story(project.path());
    let mut child = slow
        .args(["comment", "PB-1", "trip the hook"])
        .spawn()
        .expect("spawning the slow command");

    let environment = env.environment();
    let mut seen = None;
    for _ in 0..100 {
        if let Some(record) = lifecycle::read_inflight(&environment).into_iter().next() {
            seen = Some(record);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    let record = seen.expect("the daemon must publish the command it is running");
    assert_eq!(
        record.command, "comment",
        "the record must name the command, which is what chooses its deadline"
    );
    assert!(
        !record.request_id.is_empty(),
        "the record must carry the request id a client recognises its own work by"
    );
    assert!(
        record.pid != 0,
        "the record must carry the pid, because it is the only remedy that works"
    );

    child.wait().expect("the slow command finishes");

    // And it is retracted, which is a signal in its own right: a client's clock
    // resets on the record disappearing exactly as on it changing.
    wait_for("the record to be retracted", || {
        lifecycle::read_inflight(&environment).is_empty()
    });
}

/// A record a `kill -9`'d daemon left behind does not poison the next one.
///
/// Nothing writes `daemon.current.json` on an abnormal exit's way out — there
/// is no way out to run code on — so a killed daemon can leave the file
/// naming a command that finished long before this test's daemon ever
/// started. Without a harvest at startup, the next client to wait on this new
/// daemon would read that frozen record, wait out its deadline, and be told a
/// command it never ran "may or may not have run".
#[test]
fn a_record_a_killed_daemon_left_behind_does_not_survive_the_next_ones_start() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let environment = env.environment();

    std::fs::create_dir_all(environment.daemon_state_dir()).expect("the daemon's state directory");
    lifecycle::publish_inflight(
        &environment,
        &[lifecycle::CurrentRequest {
            request_id: "stale-from-a-killed-daemon".to_string(),
            command: "github-sync".to_string(),
            project: Some("stale-project".to_string()),
            pid: 1,
            started_at: "2020-01-01T00:00:00Z".to_string(),
            served_deadline_secs: lifecycle::SYNC_SERVED_DEADLINE.as_secs(),
        }],
    );
    assert!(
        !lifecycle::read_inflight(&environment).is_empty(),
        "the fixture must actually plant a stale record before starting a daemon"
    );

    start(&env);

    wait_for(
        "the stale record to be cleared by the new daemon's own start",
        || lifecycle::read_inflight(&environment).is_empty(),
    );
}

/// **The daemon log is 0600, the same as every other daemon file that
/// matters.**
///
/// It carries the daemon's whole stderr for its whole life, and since SH-153
/// that can include a GitHub token surfaced in a diagnostic — an
/// `eprintln!` this process never audited for what it prints, because nothing
/// in it expects to be handling a secret. `publish_inflight` and the pidfile
/// were already 0600; the log was `File::create`'s 0644 until this test
/// pinned the fix.
#[cfg(unix)]
#[test]
fn the_daemon_log_is_not_world_or_group_readable() {
    use std::os::unix::fs::PermissionsExt;

    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    start(&env);

    let log = env.environment().daemon_log();
    let mode = std::fs::metadata(&log)
        .expect("the daemon must have created its log")
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o777,
        0o600,
        "the daemon log must be 0600, not {:o}",
        mode & 0o777
    );
}
