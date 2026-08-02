//! What the dashboard tells you to visit must be something it is listening on.
//!
//! # Why these tests fake `tailscale` instead of asking for one
//!
//! Every other tailnet test in this suite skips on a machine without Tailscale,
//! which means the whole surface can go quietly to zero on a CI image, on a new
//! laptop, or after a `tailscale logout` — still reporting green. These are the
//! family's subject-independent members: they bring their own `tailscale` on
//! `PATH` and therefore **never skip**, on any machine.
//!
//! # What they pin
//!
//! SH-110. `story web status` and `story web address` used to name a host by
//! probing *this machine* (`reachable_host`, deleted with that story), not by
//! reading what the daemon *bound*. The two disagree in both directions, and
//! both were observed:
//!
//! - the daemon bound the tailnet fine and the client, whose own probe missed
//!   its three-second deadline under load, advertised loopback;
//! - the daemon's probe missed the deadline so it served loopback only, and the
//!   client — probing successfully — advertised a MagicDNS URL that refused
//!   connections, and `story web address` copied it to the system clipboard.
//!
//! The second is the direction these tests mechanize, because it is the one a
//! shim can produce without a real tailnet: the shim names an IPv4 this machine
//! does not have, so `TcpListener::bind` fails and the daemon is honestly
//! loopback-only, while the shim keeps answering the client with a MagicDNS
//! name. Before the fix the client printed that name. Now it cannot: there is
//! no probe left in a client, and the daemon publishes what it bound.
//!
//! The bind fails immediately with `EADDRNOTAVAIL` rather than by timing out,
//! so these tests cost milliseconds rather than the probe's three seconds. The
//! *first* direction cannot be faked — it needs an address this machine can
//! actually bind — so it stays with the real-tailnet tests in `web_test.rs`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use storyhook_test_support::{DaemonGuard, TestEnv, reserve_port, scratch_dir, wait_for_server};

/// An IPv4 in the CGNAT range Tailscale itself uses, which is *not* configured
/// on this machine — so a daemon told to bind it fails and serves loopback
/// only, exactly as it does when the real probe misses its deadline.
const UNBINDABLE_TAILNET_IP: &str = "100.64.0.1";

/// A MagicDNS name no real tailnet will hand out. If it ever appears in a
/// command's output, that command probed rather than read.
const PHANTOM_FQDN: &str = "phantom.tail00000.ts.net";

/// A directory holding a `tailscale` that reports a fixed identity.
///
/// Returned rather than borrowed so the caller keeps it alive: the scripts stop
/// existing when it drops, and the daemon resolves `tailscale` when it starts,
/// not when this returns.
fn tailscale_shim(ip: &str, fqdn: &str) -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix("storyhook-tailscale-shim-")
        .tempdir_in("/private/tmp")
        .expect("a scratch directory for the shim");
    let script = format!(
        "#!/bin/sh\n\
         # A `tailscale` that answers instantly with a fixed identity.\n\
         if [ \"$1\" = \"status\" ]; then\n\
         \x20 printf '%s' '{{\"Self\":{{\"DNSName\":\"{fqdn}.\",\"TailscaleIPs\":[\"{ip}\"]}}}}'\n\
         \x20 exit 0\n\
         fi\n\
         exit 1\n"
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

/// `PATH` with `shim` ahead of everything the harness already puts there.
///
/// Prepended rather than replaced: the child still needs `sh`, and the harness's
/// own entry — the binary under test — must stay in front of the developer's
/// installed build.
fn path_with_shim(env: &TestEnv, shim: &Path) -> OsString {
    let mut entries: Vec<PathBuf> = vec![shim.to_path_buf()];
    entries.extend(std::env::split_paths(&env.path_with_binary()));
    std::env::join_paths(entries).expect("joining PATH")
}

/// The regression test for SH-110, in the direction a shim can produce.
///
/// The daemon is told about a tailnet it cannot bind, so it serves loopback
/// only — and the *same* shim goes on answering, so a client that probes gets a
/// MagicDNS name for an interface nothing is listening on. Reverting the fix
/// makes this fail with `http://phantom.tail00000.ts.net:<port>`.
#[test]
fn web_status_advertises_only_the_tailnet_the_daemon_bound() {
    let env = TestEnv::isolated();
    let dir = scratch_dir();
    let port = reserve_port();
    let shim = tailscale_shim(UNBINDABLE_TAILNET_IP, PHANTOM_FQDN);
    let path = path_with_shim(&env, shim.path());
    let _daemon = DaemonGuard::new(&env, dir.path());

    env.story(dir.path())
        .env("PATH", &path)
        .args(["web", "start", "--port", &port.to_string()])
        .assert()
        .success();
    wait_for_server(port);

    let status = env
        .story(dir.path())
        .env("PATH", &path)
        .args(["web", "status"])
        .output()
        .expect("running `story web status`");
    let stdout = String::from_utf8_lossy(&status.stdout).into_owned();

    assert!(
        stdout.contains(&format!("http://127.0.0.1:{port}")),
        "a daemon that bound loopback only must advertise loopback; got: {stdout}"
    );
    assert!(
        !stdout.contains(PHANTOM_FQDN),
        "`web status` named a host the daemon never bound — it probed the machine \
         instead of reading the daemon's own bind (SH-110); got: {stdout}"
    );
}

/// The user-facing half, and the reason SH-110 is a product defect rather than
/// a flaky test: the URL `story web address` puts on the clipboard is one the
/// user will paste into a browser. Advertising an interface the daemon never
/// bound makes that paste fail with no explanation.
#[test]
fn web_address_copies_only_the_tailnet_the_daemon_bound() {
    let env = TestEnv::isolated();
    let dir = scratch_dir();
    let port = reserve_port();
    let shim = tailscale_shim(UNBINDABLE_TAILNET_IP, PHANTOM_FQDN);
    let path = path_with_shim(&env, shim.path());
    let _daemon = DaemonGuard::new(&env, dir.path());

    env.story(dir.path())
        .env("PATH", &path)
        .args(["web", "start", "--port", &port.to_string()])
        .assert()
        .success();
    wait_for_server(port);

    // `cat` stands in for the clipboard, so what would have been copied is on
    // stdout where the test can read it.
    let copied = env
        .story(dir.path())
        .env("PATH", &path)
        .env("STORYHOOK_CLIPBOARD_CMD", "cat")
        .args(["web", "address"])
        .output()
        .expect("running `story web address`");
    let stdout = String::from_utf8_lossy(&copied.stdout).into_owned();

    assert!(
        !stdout.contains(PHANTOM_FQDN),
        "`web address` would have copied a URL for an interface the daemon never \
         bound, which refuses connections (SH-110); got: {stdout}"
    );
    assert!(
        stdout.contains(&format!("http://127.0.0.1:{port}/")),
        "the copied URL must be one that works; got: {stdout}"
    );
}

/// The shim itself has to be doing what the test believes, or both assertions
/// above could pass for the wrong reason — a `tailscale` that errors would
/// leave no tailnet identity at all, and the fix would look correct while doing
/// nothing.
///
/// This asserts the premise directly: with the shim on `PATH`, the daemon
/// really does discover a tailnet identity and really does fail to bind it.
#[test]
fn the_shim_is_discovered_and_its_address_is_genuinely_unbindable() {
    let shim = tailscale_shim(UNBINDABLE_TAILNET_IP, PHANTOM_FQDN);
    let mut entries: Vec<PathBuf> = vec![shim.path().to_path_buf()];
    entries.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let path = std::env::join_paths(entries).expect("joining PATH");

    let probed = std::process::Command::new("tailscale")
        .args(["status", "--json"])
        .env("PATH", &path)
        .output()
        .expect("the shim is on PATH and runs");
    let json = String::from_utf8_lossy(&probed.stdout).into_owned();
    assert!(probed.status.success(), "the shim must exit 0; got {json}");
    assert!(
        json.contains(UNBINDABLE_TAILNET_IP) && json.contains(PHANTOM_FQDN),
        "the shim must report the identity these tests assume; got {json}"
    );

    let refused = std::net::TcpListener::bind((UNBINDABLE_TAILNET_IP, 0));
    assert!(
        refused.is_err(),
        "{UNBINDABLE_TAILNET_IP} is bindable on this machine, so the daemon would \
         succeed and these tests would prove nothing"
    );
}
