//! SH-147: does `bind_preferred`'s port-fallback path probe the tailnet
//! identity twice, eating `2 * TAILNET_PROBE_TIMEOUT` out of a 5s
//! `SPAWN_DEADLINE`?
//!
//! # Why this is a measurement, not a fix
//!
//! The story was filed speculatively, during a council vote, against line
//! numbers `bind_preferred` no longer has, and says outright: "Not
//! reproduced -- file to measure." `bind_listeners` binds loopback *before*
//! it ever calls the tailnet probe, and returns `Err` the moment that bind
//! fails — which is exactly the branch `bind_preferred` takes to decide
//! whether to retry on port 0. So the retry only ever happens on a path that
//! never reached the probe the first time; a successful call reaches the
//! probe and then cannot fail, so it never retries either. Structurally,
//! `bind_preferred` cannot call the probe more than once per invocation, and
//! a `git log -p` back to this function's introduction (`c4365c3`) shows
//! loopback-bind-before-probe has been true since the first version of
//! `bind_listeners` — there is no earlier ordering this ever regressed from.
//!
//! # SH-186 strengthened the premise from "at most once" to "never"
//!
//! SH-186 removed the tailnet probe from `bind_listeners`/`bind_preferred`'s
//! synchronous path entirely — a fresh, race-free unit test,
//! `daemon::serve::tests::bind_listeners_never_itself_produces_a_tailnet_bind`,
//! pins that directly by asserting `bind_listeners`'s own return value rather
//! than counting shim invocations. What used to be structural-but-latent
//! ("the probe is reached at most once, because a successful loopback bind
//! never retries") is now trivially true by construction: there is no call
//! to the probe on this path at all to double.
//!
//! What remains worth measuring end-to-end, and what this file still pins:
//! the *one* probe a fresh daemon does eventually make — on
//! `serve::tailnet_reprobe`'s background thread, first attempt included —
//! never happens more than once per daemon lifetime, even across the
//! port-fallback scenario SH-147 was filed against. That thread is one-shot
//! by design (`tailnet_reprobe` returns the instant a bind succeeds), and
//! this test is its regression guard.
//!
//! # Why the assertion polls rather than reads once
//!
//! The probe that satisfies the invariant above no longer runs on
//! `web start`'s own synchronous path — it runs asynchronously, and its
//! first attempt fires immediately rather than after a delay, so there is a
//! genuine race between "the client observes the daemon as healthy" and
//! "the background thread's first probe has already run." Reading the
//! counter once, immediately after `web start` returns, flakes between 0 and
//! 1 depending on which side of that race won.
//!
//! The first fix attempted here read the counter until it stopped changing
//! for a quiet window — and was itself racy under load: under
//! `--test-threads=4` contention, a counter that legitimately had not been
//! written to *yet* was indistinguishable from one that would never be
//! written to again, so the quiet window elapsed while the probe was still
//! merely slow to start, and the assertion read a confirmed 0. The fix is
//! [`probes_after_bind_confirmed`]: wait for the daemon's own portfile to
//! report the bind, which is authoritative rather than a proxy — the
//! callback that writes it runs synchronously inside `tailnet_reprobe`,
//! strictly before that one-shot loop returns and stops probing forever, so
//! observing the bind proves the count is already final.

use std::ffi::OsString;
use std::net::{IpAddr, TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use storyhook_test_support::{DaemonGuard, TestEnv, reserve_port, slug_at};

/// A real, bindable, non-loopback IP on this machine — the same
/// routing-lookup trick `tailnet_rebind.rs` uses, and for the same reason:
/// since SH-186 a probe that reports an identity but never actually *binds*
/// makes `tailnet_reprobe` retry forever rather than stop, so this file's
/// "exactly one probe" invariant needs the shim's identity to be one the
/// bind can genuinely succeed against. No live Tailscale required.
fn a_bindable_non_loopback_ip() -> IpAddr {
    let socket = UdpSocket::bind("0.0.0.0:0").expect("binding an ephemeral UDP socket");
    socket
        .connect("8.8.8.8:80")
        .expect("a route to pick a source address from, even with no real connectivity behind it");
    socket.local_addr().expect("the socket's own address").ip()
}

/// A directory holding a `tailscale` that counts every `status --json`
/// invocation into `counter_path` (one appended byte each) and always answers
/// with a fixed identity — fast and unconditional, so nothing here depends
/// on the 3s probe deadline. `ip` must be one this machine can actually bind
/// ([`a_bindable_non_loopback_ip`]) or `tailnet_reprobe` retries forever
/// instead of stopping after one attempt.
fn counting_tailscale_shim(counter_path: &Path, ip: &str, fqdn: &str) -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix("storyhook-tailscale-counting-shim-")
        .tempdir_in("/private/tmp")
        .expect("a scratch directory for the shim");
    let script = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = \"status\" ]; then\n\
         \x20 printf 'x' >> '{counter}'\n\
         \x20 printf '%s' '{{\"Self\":{{\"DNSName\":\"{fqdn}.\",\"TailscaleIPs\":[\"{ip}\"]}}}}'\n\
         \x20 exit 0\n\
         fi\n\
         exit 1\n",
        counter = counter_path.display(),
    );
    let path = dir.path().join("tailscale");
    std::fs::write(&path, script).expect("writing the shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("making the shim executable");
    }
    dir
}

/// `PATH` with `shim` ahead of everything the harness already puts there —
/// the same construction `tailnet_advertise.rs` and `tailnet_rebind.rs` use.
fn path_with_shim(env: &TestEnv, shim: &Path) -> OsString {
    let mut entries: Vec<PathBuf> = vec![shim.to_path_buf()];
    entries.extend(std::env::split_paths(&env.path_with_binary()));
    std::env::join_paths(entries).expect("joining PATH")
}

/// How long this test waits for the daemon to confirm a tailnet bind before
/// giving up. Generous relative to what it is bounding — the shim answers
/// instantly — because a false "it's done" reading here would silently hide
/// a probe that simply hadn't run yet under load.
const SETTLE_DEADLINE: Duration = Duration::from_secs(5);

/// Waits for the daemon to report a confirmed tailnet bind, then reads the
/// probe counter — race-free, unlike watching the counter file for a quiet
/// period (tried first, and wrong: under `--test-threads=4` contention a
/// counter that legitimately has not been written to *yet* is
/// indistinguishable from one that will never be written to again, so a
/// window-based "it stopped changing" reading raced the probe and read 0).
///
/// `info.tailnet.is_some()` is authoritative rather than a proxy: `serve.rs`'s
/// `on_late_tailnet_bind` callback — which writes this portfile field — runs
/// synchronously inside `tailnet_reprobe`, strictly *before* that one-shot
/// loop returns and stops probing forever. Observing the bind therefore
/// proves the probe count is already final; nothing waits for it to "settle"
/// because there is nothing left that could still change it.
fn probes_after_bind_confirmed(env: &TestEnv, counter_path: &Path) -> usize {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        if let Some(info) = env.daemon()
            && info.tailnet.is_some()
        {
            return std::fs::read(counter_path)
                .map(|bytes| bytes.len())
                .unwrap_or(0);
        }
        assert!(
            Instant::now() < deadline,
            "the daemon never reported a tailnet bind within {SETTLE_DEADLINE:?} — the \
             counting shim always succeeds immediately, so this means tailnet_reprobe never \
             ran or never bound at all, not merely that it was slow"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_daemon_never_probes_the_tailnet_more_than_once_across_a_port_fallback() {
    let env = TestEnv::isolated();
    let project = env.project().build();
    let _slug = slug_at(&env, project.path());

    // Building the fixture above already started a daemon on its own
    // ambient PATH and port — the same fixture-ordering trap SH-146's test
    // found. Force a fresh spawn so the shimmed PATH and the occupied
    // `--port` below are both actually observed.
    env.stop_daemon();

    // Occupy the preferred port with a plain listener held for the rest of
    // this test, so `bind_preferred`'s first `bind_listeners` call fails on
    // its loopback bind and must fall back to port 0 — the exact scenario
    // SH-147 was filed against.
    let occupied = reserve_port();
    let _hog = TcpListener::bind(("127.0.0.1", occupied))
        .expect("occupying the preferred port so bind_preferred must fall back");

    let counter = project.path().join("tailscale-probe-count");
    let ip = a_bindable_non_loopback_ip();
    let shim = counting_tailscale_shim(
        &counter,
        &ip.to_string(),
        "sh147-probe-budget.tail00000.ts.net",
    );
    let path = path_with_shim(&env, shim.path());
    let _daemon_guard = DaemonGuard::new(&env, project.path());

    env.story(project.path())
        .env("PATH", &path)
        .args(["web", "start", "--port", &occupied.to_string()])
        .assert()
        .success();

    let info = env
        .daemon()
        .expect("the daemon must have started, on the fallback port");
    assert_ne!(
        info.port, occupied,
        "the daemon must have fallen back off the occupied preferred port"
    );

    let probes = probes_after_bind_confirmed(&env, &counter);
    assert_eq!(
        probes, 1,
        "one spawn probed the tailnet identity {probes} times through a port fallback; \
         tailnet_reprobe must stop the instant it binds successfully, or a daemon on this \
         path could spend an unbounded number of `tailscale status --json` calls rather \
         than the one this test expects"
    );
}
