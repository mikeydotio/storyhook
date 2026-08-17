//! SH-186: a wedged or absent `tailscale` must cost the dashboard, and every
//! client that autostarts it, nothing.
//!
//! # The defect this fences
//!
//! `bind_listeners` used to shell out to `tailscale status --json` (capped at
//! [`TAILNET_PROBE_TIMEOUT`]) *between* binding the loopback socket and
//! starting to serve it — so for as long as a wedged `tailscale` blocked that
//! call, the socket accepted connections and answered nothing, and the
//! portfile that publishes the daemon to every other client had not been
//! written yet either. Measured before this story's fix, with a `tailscale`
//! shim that sleeps two minutes: `story web start` took 3.28s of its 5s
//! `SPAWN_DEADLINE`, and the accepting-but-silent window on a direct
//! `story web --serve` was 2.96s.
//!
//! The fix takes the probe off that path entirely: `bind_listeners` binds
//! loopback and stops, and every tailnet bind — including the first —
//! happens on `serve::tailnet_reprobe`'s background thread (previously a
//! self-heal path only, SH-146).
//!
//! # Two ways these tests assert the mechanism, not a wall clock
//!
//! [`answer_after_accept_is_well_under_the_probe_budget`] and
//! [`a_wedged_tailscale_cli_leaves_no_accepting_but_silent_window`] derive
//! their bound from [`TAILNET_PROBE_TIMEOUT`] itself, so a change to that
//! budget cannot make them vacuous the way the original regression test's
//! 20s bound (roughly 6x the probe's own 3s) did.
//!
//! The other three ([`publication_does_not_wait_on_the_tailnet_probe`],
//! [`an_ordinary_command_autostarts_fast_under_a_wedged_tailscale`],
//! [`web_start_returns_fast_under_a_wedged_tailscale_and_never_overclaims`])
//! do not compare against `TAILNET_PROBE_TIMEOUT` at all (SH-394's council
//! decision — see `story show SH-394`; this project's own council
//! directories are gitignored and per-worktree, so the verdict is restated
//! here rather than pointed at alone, per SH-363). Each measures a
//! quantity that includes a whole process spawn, on a machine that runs
//! three-to-four concurrent worktree suites at once — and no fixed ceiling
//! can be simultaneously tight enough to reject a reintroduced synchronous
//! probe and loose enough to survive that load, because the two claims a
//! wall-clock bound makes ("the deadline was not spent" and "this machine is
//! fast today") cannot be separated by choosing a different number. They use
//! [`HeldTailscale`] instead: a shim that cannot proceed past its very first
//! instruction until the test releases it, so if the client-observable
//! outcome arrives at all while it is still held, nothing on the path that
//! produced it can have been waiting on a tailscale invocation to return.
//! That is a structural fact (event order), not a timing measurement (event
//! duration), and it is what this project's own doctrine already asks for —
//! "a timestamp is not an ordering key," prefer a structural signal over a
//! derived one.
//!
//! A held shim was chosen over a simpler "touch a marker, then check whether
//! it exists at the moment of the outcome" design (the council's literal
//! proposal) because that has a real race this codebase's own source rules
//! out on inspection: `tailnet_reprobe` (`src/daemon/serve.rs`) calls
//! `probe_and_bind_tailnet` on its *first* loop iteration with no initial
//! delay — the backoff schedule only applies after a *failed* attempt — so
//! the background thread invokes the shim almost immediately once spawned,
//! whether or not the implementation under test is correct. "Does the marker
//! exist by the time I check" is a race between two independently-scheduled
//! things, not a clean signal; a hold has no such race, because the shim
//! cannot make any progress — not even exit — until released, so there is no
//! window to lose a race in.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use storyhook::daemon::tailnet::TAILNET_PROBE_TIMEOUT;
use storyhook_test_support::{
    ChildGuard, TestEnv, http_status_line, port_of, reserve_port, scratch_dir,
};

/// A directory holding a `tailscale` that never answers — `sleep 120` wedges
/// every probe attempt for the life of the test, the same `tailscaled`
/// pathology [`TAILNET_PROBE_TIMEOUT`]'s doc comment describes.
fn wedged_tailscale_shim() -> tempfile::TempDir {
    let dir = scratch_dir();
    let path = dir.path().join("tailscale");
    std::fs::write(&path, "#!/bin/sh\nsleep 120\n").expect("writing the shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("making the shim executable");
    }
    dir
}

/// A `tailscale` that cannot proceed past its own first instruction until
/// [`HeldTailscale::release`] is called — not merely slow, held. See this
/// file's own header for why a hold, rather than a timed sleep plus a marker
/// check, is what the three load-sensitive tests below need.
///
/// The poll interval (50ms) is a wake-up cadence, not a claim about speed:
/// nothing here is timed, so a slower poll would only ever cost this fixture
/// wall-clock, never correctness.
struct HeldTailscale {
    dir: tempfile::TempDir,
    go: PathBuf,
}

impl HeldTailscale {
    fn spawn() -> Self {
        let dir = scratch_dir();
        let path = dir.path().join("tailscale");
        let go = dir.path().join("go");
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nwhile [ ! -e {} ]; do sleep 0.05; done\nexit 1\n",
                go.display()
            ),
        )
        .expect("writing the shim");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("making the shim executable");
        }
        Self { dir, go }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Lets a held invocation, if there is one, finally exit — cleanly, with
    /// a non-zero status and no output, so a probe that runs after release
    /// reads unambiguously as "failed" rather than as something a caller
    /// might try to parse. Idempotent, and also run on drop, so a test that
    /// panics before calling this explicitly still does not leave a poller
    /// running past the end of the process.
    fn release(&self) {
        let _ = std::fs::write(&self.go, "");
    }
}

impl Drop for HeldTailscale {
    fn drop(&mut self) {
        self.release();
    }
}

/// `PATH` with `shim` ahead of everything the harness already puts there —
/// the same construction `tailnet_advertise.rs`, `tailnet_rebind.rs` and
/// `tailnet_probe_budget.rs` use.
fn path_with_shim(env: &TestEnv, shim: &Path) -> OsString {
    let mut entries: Vec<PathBuf> = vec![shim.to_path_buf()];
    entries.extend(std::env::split_paths(&env.path_with_binary()));
    std::env::join_paths(entries).expect("joining PATH")
}

/// How long the loopback socket may accept a connection and answer nothing
/// before that is a bug rather than scheduling noise.
///
/// Deliberately far under [`TAILNET_PROBE_TIMEOUT`] rather than close to it:
/// the whole point of moving the probe off this path is that nothing on it
/// waits on the probe's budget at all. A regression that reintroduced a
/// synchronous probe here would still pass a bound merely *less than*
/// `TAILNET_PROBE_TIMEOUT` most of the time (the probe can fail fast on a
/// missing binary); this bound is small enough that only "no probe on this
/// path" can satisfy it.
const ANSWER_AFTER_ACCEPT: Duration = Duration::from_secs(1);

/// A daemon that binds loopback and then blocks for the whole tailnet probe
/// before it can answer anything is exactly the defect this story fixes —
/// `ANSWER_AFTER_ACCEPT` must be small enough that only "no probe on the
/// pre-serve path at all" can satisfy it.
#[test]
fn answer_after_accept_is_well_under_the_probe_budget() {
    assert!(
        ANSWER_AFTER_ACCEPT < TAILNET_PROBE_TIMEOUT,
        "a bound that isn't strictly tighter than the probe's own budget \
         cannot tell \"no probe on this path\" apart from \"a probe that \
         happened to fail fast\""
    );
}

/// The regression test for SH-186 itself: a wedged `tailscale` must not
/// leave the loopback socket accepting connections and answering nothing.
///
/// Unlike the story's original regression test, this measures the silent
/// window directly — from the instant a TCP connect succeeds to the instant
/// a response arrives — rather than tolerating up to 20s for *some* 200 to
/// eventually show up. Red before this story's fix (measured 2.96s of dead
/// air); green after, because nothing on the path from bind to first-request
/// touches `tailscale` at all.
#[test]
fn a_wedged_tailscale_cli_leaves_no_accepting_but_silent_window() {
    let env = TestEnv::isolated();
    let shim = wedged_tailscale_shim();
    let path = path_with_shim(&env, shim.path());
    let port = reserve_port();

    let mut command = env.raw_story(std::env::temp_dir());
    command
        .args(["web", "--serve", "--port", &port.to_string()])
        .env("PATH", &path);
    let child = command.spawn().expect("spawning the dashboard");
    let guard = ChildGuard::new(child);

    let connect_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        assert!(
            Instant::now() < connect_deadline,
            "the dashboard never bound {port} at all, wedged tailscale or not"
        );
        std::thread::sleep(Duration::from_millis(25));
    }

    // The socket accepts; from this instant, an answer must arrive well
    // inside the probe's own budget, not merely inside a wall clock generous
    // enough to hide a 3s stall behind it.
    let status = http_status_line(port, ANSWER_AFTER_ACCEPT);
    assert!(
        status.as_deref().is_some_and(|line| line.contains("200")),
        "the dashboard accepted a connection but did not answer within \
         {ANSWER_AFTER_ACCEPT:?} of a wedged `tailscale`; got: {status:?}"
    );

    // The port belongs to the daemon this test armed, not a stranger that
    // happened to win a stale port — the same identity check `port_of`
    // performs for the client-facing tests below.
    let info = env
        .daemon()
        .expect("the dashboard must have published a portfile by now");
    assert_eq!(
        info.pid,
        guard.pid(),
        "the daemon answering on {port} is not the one this test spawned"
    );
}

/// Publication — the portfile every other client discovers this daemon
/// through — must not wait on the tailnet probe at all. Red before this
/// story's fix: `crates/storyhook-test-support::server::PORTFILE_DEADLINE`'s
/// own doc measured a 3.36s wait to publish against an identical wedge.
///
/// A *held* tailscale (this file's header explains why, over a timed one)
/// can never answer, so a portfile arriving at all proves nothing on the
/// path that produced it was waiting on a tailscale invocation to return.
/// `port_of`'s own [`PORTFILE_DEADLINE`](storyhook_test_support::server) —
/// unrelated to this file's timing and not touched by it — is what turns a
/// genuine hang (the broken case, since a held shim can never satisfy a
/// synchronous wait) into a named failure rather than a stuck `cargo test`.
#[test]
fn publication_does_not_wait_on_the_tailnet_probe() {
    let held = HeldTailscale::spawn();
    let env = TestEnv::isolated();
    let path = path_with_shim(&env, held.path());

    let mut command = env.raw_story(std::env::temp_dir());
    command.args(["web", "--serve"]).env("PATH", &path);
    let child = command.spawn().expect("spawning the dashboard");
    let guard = ChildGuard::new(child);

    let _port = port_of(&env, guard.pid());
    held.release();
}

/// Not just the dashboard: *every* command that autostarts a daemon
/// (`lifecycle::ensure`, on every command's path since SH-114) must be
/// unaffected by a wedged `tailscale`. Red before this story's fix — the
/// client-side `await_healthy` cannot return until a portfile lands, which
/// sat behind the same probe.
///
/// A held tailscale can never answer, so this command succeeding at all
/// proves the autostart path was not waiting on one — see this file's
/// header. `lifecycle::ensure`'s own `SPAWN_DEADLINE` is what bounds the
/// broken case (the client gives up waiting on the newly-spawned daemon and
/// `.assert().success()` fails loudly, rather than this test hanging).
#[test]
fn an_ordinary_command_autostarts_fast_under_a_wedged_tailscale() {
    let held = HeldTailscale::spawn();
    let env = TestEnv::isolated();
    let path = path_with_shim(&env, held.path());
    let dir = scratch_dir();

    env.stop_daemon();

    env.story(dir.path())
        .env("PATH", &path)
        .args(["project", "list"])
        .assert()
        .success();
    held.release();

    env.story(dir.path())
        .args(["web", "stop"])
        .assert()
        .success();
}

/// `story web start` itself, client-facing: it must return fast under a
/// wedged `tailscale`, and it must never print a tailnet URL it has not
/// actually confirmed — a loopback URL plus a note that the tailnet is
/// still resolving is the honest answer at this instant, per SH-186's
/// council decision, which that story carries as a comment.
///
/// "Returns" rather than "returns fast": a held tailscale can never answer,
/// so this command succeeding at all — bounded by `lifecycle::ensure`'s own
/// `SPAWN_DEADLINE`, the same anti-hang backstop the sibling test above
/// relies on — proves the mechanism, which is the property this test
/// actually names in its title. See this file's header for why.
#[test]
fn web_start_returns_fast_under_a_wedged_tailscale_and_never_overclaims() {
    let held = HeldTailscale::spawn();
    let env = TestEnv::isolated();
    let path = path_with_shim(&env, held.path());
    let dir = scratch_dir();

    env.stop_daemon();

    let output = env
        .story(dir.path())
        .env("PATH", &path)
        .args(["web", "start"])
        .output()
        .expect("running `story web start`");
    held.release();
    assert!(output.status.success(), "{output:?}");

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        stdout.contains("http://127.0.0.1:"),
        "must advertise loopback, the only address certain to be answering \
         at this instant; got: {stdout}"
    );
    assert!(
        !stdout.contains(".ts.net"),
        "must never print a tailnet URL it has not confirmed — the daemon \
         cannot have bound one yet under a probe that is still wedged; got: \
         {stdout}"
    );

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        stderr.contains("resolving") && stderr.to_lowercase().contains("tailnet"),
        "the printed loopback URL can lag reality once the probe is off the \
         critical path, so the CLI must say so rather than let a stale URL \
         read as a confirmed answer; got stderr: {stderr}"
    );

    env.story(dir.path())
        .args(["web", "stop"])
        .assert()
        .success();
}
