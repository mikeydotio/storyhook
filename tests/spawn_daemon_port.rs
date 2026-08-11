//! SH-195: `spawn_daemon` (and the equivalent direct spawn in
//! `tests/daemon_git_env.rs`) used to ask `reserve_port()` for a specific
//! port, bound-then-immediately-released to rule out a long-lived squatter,
//! then hand that number to `story daemon --serve --port N` milliseconds
//! later. The release-to-rebind gap was a genuine TOCTOU window: nothing
//! proved the port would still be free when the daemon actually bound it.
//!
//! It went unfixed by hardening the reservation (candidates considered and
//! rejected: hand over an already-bound fd, shrink the window, retry on
//! failure) because every caller that spawned a daemon this way already
//! discovers its *real* port from the portfile afterwards rather than
//! trusting the one it asked for — `bind_preferred` treats a requested port
//! as a preference and falls back to a kernel-assigned one the instant it is
//! taken (`crates/storyhook-test-support/src/crash.rs::port_of` already said
//! so in the doc comment that introduced it). A pre-picked port bought these
//! callers nothing they needed, so the fix removes the reservation instead of
//! defending it: `--port 0` has no release-to-rebind gap to race, because
//! there is no separate reservation step at all.
//!
//! This pins the property the fix relies on: a directly-spawned daemon with
//! no port pre-selected by this process still comes up and is fully
//! discoverable from its own portfile.

use std::time::{Duration, Instant};

use storyhook_test_support::{TestEnv, scratch_dir, spawn_daemon};

#[test]
fn a_directly_spawned_daemon_is_discoverable_from_its_portfile_with_no_port_preselected() {
    let env = TestEnv::isolated();
    env.stop_daemon();
    let cwd = scratch_dir();

    let daemon = spawn_daemon(&env, cwd.path(), None);

    let deadline = Instant::now() + Duration::from_secs(10);
    let info = loop {
        if let Some(info) = env.daemon()
            && info.pid == daemon.pid()
        {
            break info;
        }
        assert!(
            Instant::now() < deadline,
            "the directly-spawned daemon never published a portfile naming its own pid \
             within {deadline:?} of being spawned with no port pre-selected"
        );
        std::thread::sleep(Duration::from_millis(25));
    };

    assert_ne!(
        info.port, 0,
        "a bound daemon never reports port 0 on its portfile -- that spelling means \
         'let the kernel pick', not 'this one did'"
    );
}
