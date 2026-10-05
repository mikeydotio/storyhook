//! A test environment's daemon listens on loopback only.
//!
//! On a machine with a tailnet, every daemon used to bind the tailnet address
//! `tailscale` reports, test daemons included. A fixture's dashboard and API
//! were then reachable from every device on the tailnet for as long as the
//! daemon lived, and a leaked fixture daemon lives until somebody kills it.
//! Every test environment now sets `STORYHOOK_TAILNET=0`, and a loopback-only
//! daemon never even asks `tailscale`.
//!
//! The shim reports a TEST-NET-1 address (RFC 5737), which no machine owns, so
//! even the opted-in control never listens anywhere but loopback. Each probe
//! appends one byte to a counter, so "the daemon asked" is a fact on disk
//! rather than an inference from a bind.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use storyhook::daemon::tailnet::TAILNET_PROBE_TIMEOUT;
use storyhook::env::TailnetPolicy;
use storyhook_test_support::load_grace::{Patience, graced_now};
use storyhook_test_support::{DaemonGuard, TestEnv, scratch_dir, wait_for_server};

/// An address no machine owns: a bind to it always fails, so a daemon that
/// probes still serves loopback only.
const DOCUMENTATION_IP: &str = "192.0.2.1";

/// How long a daemon that binds the tailnet takes, at most, to ask for its
/// address once it is serving: its first probe fires at once, and this leaves
/// one probe window for scheduling. The control below proves the window is
/// enough; the negative case waits exactly as long before it reads.
const FIRST_PROBE_WINDOW: Duration = TAILNET_PROBE_TIMEOUT.saturating_mul(2);

/// A directory holding a `tailscale` that appends one byte to `counter` per
/// `status` call and reports [`DOCUMENTATION_IP`].
fn counting_tailscale_shim(counter: &Path) -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix("storyhook-tailscale-containment-")
        .tempdir_in("/private/tmp")
        .expect("a scratch directory for the shim");
    let script = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = \"status\" ]; then\n\
         \x20 printf 'x' >> '{counter}'\n\
         \x20 printf '%s' '{{\"Self\":{{\"DNSName\":\"fixture.tail00000.ts.net.\",\"TailscaleIPs\":[\"{DOCUMENTATION_IP}\"]}}}}'\n\
         \x20 exit 0\n\
         fi\n\
         exit 1\n",
        counter = counter.display(),
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
fn path_with_shim(env: &TestEnv, shim: &Path) -> OsString {
    let mut entries: Vec<PathBuf> = vec![shim.to_path_buf()];
    entries.extend(std::env::split_paths(&env.path_with_binary()));
    std::env::join_paths(entries).expect("joining PATH")
}

fn probes(counter: &Path) -> usize {
    std::fs::read(counter).map(|bytes| bytes.len()).unwrap_or(0)
}

/// Starts a daemon with the counting shim on `PATH`, plus `extra` variables,
/// and waits until it serves.
fn start_daemon(env: &TestEnv, dir: &Path, path: &OsString, extra: &[(&str, &str)]) -> u16 {
    let mut command = env.story(dir);
    command.env("PATH", path);
    for (name, value) in extra {
        command.env(name, value);
    }
    command.args(["web", "start"]).assert().success();
    let info = env
        .daemon()
        .expect("the daemon published a portfile once it was healthy");
    wait_for_server(info.port);
    info.port
}

/// The control: a daemon that opts into the tailnet asks the shim within the
/// window. Without it, the negative case below could pass because the shim
/// was never on the daemon's `PATH`.
#[test]
fn a_daemon_opted_into_the_tailnet_asks_tailscale_for_its_address() {
    let env = TestEnv::isolated();
    let dir = scratch_dir();
    let counter = dir.path().join("probes");
    let shim = counting_tailscale_shim(&counter);
    let path = path_with_shim(&env, shim.path());
    let _daemon = DaemonGuard::new(&env, dir.path());

    start_daemon(
        &env,
        dir.path(),
        &path,
        &[(TailnetPolicy::VARIABLE, TailnetPolicy::Bind.as_env_value())],
    );

    let mut patience = Patience::new(FIRST_PROBE_WINDOW);
    while probes(&counter) == 0 && !patience.expired() {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        probes(&counter) >= 1,
        "an opted-in daemon never asked the shim for a tailnet address; the \
         negative case in this file proves nothing until it does"
    );
}

/// The property: a daemon a test environment starts never asks `tailscale`,
/// so it can never bind the tailnet, whatever the machine has.
#[test]
fn a_test_environment_daemon_never_asks_tailscale_and_listens_on_loopback_only() {
    let env = TestEnv::isolated();
    let dir = scratch_dir();
    let counter = dir.path().join("probes");
    let shim = counting_tailscale_shim(&counter);
    let path = path_with_shim(&env, shim.path());
    let _daemon = DaemonGuard::new(&env, dir.path());

    start_daemon(&env, dir.path(), &path, &[]);

    // A fixed negative observation, as long as the control's patience.
    std::thread::sleep(graced_now(FIRST_PROBE_WINDOW));
    assert_eq!(
        probes(&counter),
        0,
        "a test environment's daemon asked `tailscale` for a tailnet address; \
         on a machine with a tailnet it would listen there"
    );
    let info = env.daemon().expect("the daemon is still serving");
    assert!(
        info.tailnet.is_none(),
        "a test environment's daemon reports a tailnet bind: {:?}",
        info.tailnet
    );
}
