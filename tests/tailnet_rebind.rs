//! SH-146: a daemon that missed its tailnet bind at startup must self-heal
//! once `tailscale` catches up, without a restart.
//!
//! # Why a two-phase shim
//!
//! `tests/tailnet_advertise.rs`'s shim always answers the same way, which is
//! right for pinning the *initial*-bind defect (SH-110) but cannot exercise a
//! bind that only succeeds *after* the daemon has already started and given
//! up once. This file's shim starts by failing — standing in for `tailscaled`
//! not being up yet at login — then, once a marker file appears, starts
//! answering with a real identity.
//!
//! # Why the identity's IP is real
//!
//! `tailnet_advertise.rs` deliberately reports an *unbindable* CGNAT address
//! to prove the daemon serves loopback only when the bind itself fails. This
//! test needs the opposite: a late bind that actually *succeeds*, so it needs
//! an address this machine can actually claim. [`a_bindable_non_loopback_ip`]
//! finds one the same way an outbound connection would pick a source address
//! — a route lookup, not a real network access — so the test needs no real
//! tailnet and never skips.

use std::ffi::OsString;
use std::net::{IpAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use storyhook_test_support::{TestEnv, reserve_port, slug_at, wait_for_addr, wait_for_server};

/// A real, bindable, non-loopback IP on this machine — found by asking the
/// kernel which source address it would use to reach `8.8.8.8`. A UDP
/// `connect` only performs a routing-table lookup; it sends nothing, so this
/// works offline as long as the machine has any configured route.
fn a_bindable_non_loopback_ip() -> IpAddr {
    let socket = UdpSocket::bind("0.0.0.0:0").expect("binding an ephemeral UDP socket");
    socket
        .connect("8.8.8.8:80")
        .expect("a route to pick a source address from, even with no real connectivity behind it");
    socket.local_addr().expect("the socket's own address").ip()
}

/// A directory holding a `tailscale` that fails until `ready_marker` exists
/// as a file, then reports a fixed identity at `ip`/`fqdn` forever after.
fn flaky_tailscale_shim(ready_marker: &Path, ip: IpAddr, fqdn: &str) -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix("storyhook-tailscale-flaky-shim-")
        .tempdir_in("/private/tmp")
        .expect("a scratch directory for the shim");
    let script = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = \"status\" ]; then\n\
         \x20 if [ -f '{marker}' ]; then\n\
         \x20\x20 printf '%s' '{{\"Self\":{{\"DNSName\":\"{fqdn}.\",\"TailscaleIPs\":[\"{ip}\"]}}}}'\n\
         \x20\x20 exit 0\n\
         \x20 fi\n\
         \x20 exit 1\n\
         fi\n\
         exit 1\n",
        marker = ready_marker.display(),
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
/// the same construction `tailnet_advertise.rs` uses.
fn path_with_shim(env: &TestEnv, shim: &Path) -> OsString {
    let mut entries: Vec<PathBuf> = vec![shim.to_path_buf()];
    entries.extend(std::env::split_paths(&env.path_with_binary()));
    std::env::join_paths(entries).expect("joining PATH")
}

/// How long the test gives the background retry to notice the shim flipped.
/// The daemon is started with `STORYHOOK_TAILNET_REPROBE_*_MS` overrides far
/// below the production defaults (2s initial, 60s cap), so this bound is
/// about the retry *happening*, not about waiting out production timing.
const REBIND_DEADLINE: Duration = Duration::from_secs(10);

#[test]
fn a_daemon_that_missed_its_tailnet_bind_self_heals_without_a_restart() {
    let env = TestEnv::isolated();
    let project = env.project().build();
    let slug = slug_at(&env, project.path());
    let story_id = project.new_story("Move me once the tailnet self-heals");

    // Building the fixture above already talked to a daemon — every `story`
    // command reaches the store through one — so one is already running,
    // holding whatever ephemeral port and real `tailscale` identity it found
    // at ITS startup. `web start`'s fast path would just hand that back
    // untouched (a request to start something already started is not a
    // request to restart it), silently ignoring both the `--port` below and
    // the shimmed `PATH`. Standing it down first forces a fresh spawn that
    // actually observes both.
    env.stop_daemon();

    let port = reserve_port();
    let marker = project.path().join("tailscale-ready");
    let ip = a_bindable_non_loopback_ip();
    let fqdn = "sh146-rebind-test.tail00000.ts.net";
    let shim = flaky_tailscale_shim(&marker, ip, fqdn);
    let path = path_with_shim(&env, shim.path());
    let _daemon_guard = storyhook_test_support::DaemonGuard::new(&env, project.path());

    env.story(project.path())
        .env("PATH", &path)
        .env("STORYHOOK_TAILNET_REPROBE_INITIAL_MS", "50")
        .env("STORYHOOK_TAILNET_REPROBE_CAP_MS", "200")
        .args(["web", "start", "--port", &port.to_string()])
        .assert()
        .success();
    wait_for_server(port);

    // The premise: at startup the daemon really did miss the tailnet bind —
    // the marker does not exist yet, so the shim is still failing.
    let before = env
        .daemon()
        .expect("the daemon we just started must have a portfile");
    assert!(
        before.tailnet.is_none(),
        "the daemon must start loopback-only for this to be a regression test of the \
         self-heal, not of the initial bind; got {:?}",
        before.tailnet
    );

    // `tailscaled` "finishes starting" — flip the shim.
    std::fs::write(&marker, "").expect("writing the ready marker");

    // The daemon must notice on its own, without a restart, within the fast
    // retry cadence configured above.
    let started = Instant::now();
    let healed = loop {
        if let Some(bind) = env.daemon().and_then(|info| info.tailnet) {
            break bind;
        }
        assert!(
            started.elapsed() < REBIND_DEADLINE,
            "the daemon never self-healed its tailnet bind after tailscale became reachable \
             (SH-146) within {REBIND_DEADLINE:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(
        healed.ip(),
        ip,
        "the self-healed bind must be the identity the shim reported"
    );
    assert_eq!(healed.magic_dns(), Some(fqdn));

    // The new listener is genuinely accepting, not just reported.
    wait_for_addr(&format!("{ip}:{port}"));

    // And it is trusted for a mutation, exactly like a bind made at startup
    // would be — the late-bind counterpart of `web_test.rs`'s
    // `web_serve_tailnet_ip_is_auto_trusted_for_mutations`, which pins the
    // startup case. No `STORYHOOK_WEB_TRUSTED_HOSTS` is set: the daemon
    // itself decided to trust this host the moment it bound it.
    let url = format!("http://{ip}:{port}/api/repos/{slug}/story/{story_id}/move");
    let resp = ureq::post(&url)
        .header("X-Storyhook", "1")
        .header("X-Storyhook-Token", &before.token)
        .content_type("application/json")
        .send(r#"{"state":"in-progress"}"#)
        .unwrap_or_else(|e| {
            panic!("expected the late-bound tailnet interface to be auto-trusted: {e}")
        });
    assert_eq!(resp.status(), 200);
}
