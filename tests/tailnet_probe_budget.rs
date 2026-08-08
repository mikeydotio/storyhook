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
//! This test pins that structural guarantee with a counting shim rather than
//! trusting the reading above, and stands as the regression guard for the
//! defect the story actually described: if a future change reorders
//! `bind_listeners` so the probe runs before the loopback bind can fail, this
//! goes red.

use std::ffi::OsString;
use std::net::TcpListener;
use std::path::{Path, PathBuf};

use storyhook_test_support::{DaemonGuard, TestEnv, reserve_port, slug_at};

/// A directory holding a `tailscale` that counts every `status --json`
/// invocation into `counter_path` (one appended byte each) and always answers
/// with a fixed, bindable identity — fast and unconditional, so nothing here
/// depends on the 3s probe deadline.
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

#[test]
fn bind_preferred_probes_the_tailnet_at_most_once_when_it_falls_back_off_a_taken_port() {
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
    // its loopback bind before it can reach the probe at all.
    let occupied = reserve_port();
    let _hog = TcpListener::bind(("127.0.0.1", occupied))
        .expect("occupying the preferred port so bind_preferred must fall back");

    let counter = project.path().join("tailscale-probe-count");
    let shim = counting_tailscale_shim(
        &counter,
        "100.64.9.9",
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

    let probes = std::fs::read(&counter)
        .map(|bytes| bytes.len())
        .unwrap_or(0);
    assert_eq!(
        probes, 1,
        "SH-147: bind_preferred's fallback path probed the tailnet identity {probes} times \
         for one spawn; bind_listeners must reach the probe only after its own loopback bind \
         has already succeeded, so the occupied-preferred-port failure short-circuits before \
         any probe runs — a regression here means that ordering broke and the fallback path \
         can once again spend 2 * TAILNET_PROBE_TIMEOUT inside SPAWN_DEADLINE"
    );
}
