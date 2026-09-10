//! SH-635: `scripts/e2e-daemon-check.sh` decides, after a browser run, whether
//! the daemon that answered it is the one `scripts/run-e2e.sh` started, and
//! names which fact failed when it is not. Driven for real — the tracked
//! script, real bash, real portfiles on disk, a real pid — never a copy of its
//! logic (SH-136).
//!
//! The one thing pinned hardest is a negative: the check must NOT compare the
//! pid recorded at start, because `e2e/specs/untrusted-origin-cookie.spec.ts`
//! restarts the daemon on purpose and keeps its port (SH-321). A restarted
//! daemon on the same port from the same leased binary is still ours.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use storyhook_test_support::scratch_dir;

fn check_script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/e2e-daemon-check.sh")
}

fn check(portfile: &Path, port: &str, leased: &Path) -> Output {
    Command::new("bash")
        .arg("-c")
        .arg(format!(
            "set -uo pipefail\n. '{}'\nstoryhook_daemon_is_still_ours '{}' '{port}' '{}'",
            check_script().display(),
            portfile.display(),
            leased.display()
        ))
        .output()
        .expect("spawning bash")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

struct Fixture {
    root: tempfile::TempDir,
    leased: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = scratch_dir();
        let leased = root.path().join("lease/story");
        std::fs::create_dir_all(leased.parent().unwrap()).unwrap();
        std::fs::write(&leased, b"#!/bin/sh\n").unwrap();
        Self { root, leased }
    }

    /// Writes a portfile in the daemon's own shape, with only the fields the
    /// check reads varied by the caller.
    fn portfile(&self, pid: u32, port: u16, exe: &Path) -> PathBuf {
        let path = self.root.path().join("daemon.json");
        std::fs::write(
            &path,
            format!(
                r#"{{"pid":{pid},"port":{port},"version":"0.0.0","protocol":1,"exe":"{}","exe_mtime":0,"started_at":"2026-01-01T00:00:00Z","token":"t","store_path":"/x","cookie_name":"c"}}"#,
                exe.display()
            ),
        )
        .unwrap();
        path
    }

    /// A second path to the SAME inode as the lease -- a symlink, so a string
    /// compare would say "different" while the inode says "same". macOS's
    /// `current_exe()` reports the invocation spelling, so a daemon started
    /// through any alias of the lease is still running the lease.
    fn other_spelling_of_lease(&self) -> PathBuf {
        let other = self.root.path().join("alias-to-story");
        std::os::unix::fs::symlink(&self.leased, &other).unwrap();
        other
    }

    /// A different inode with the same basename: a daemon from another build.
    fn other_build(&self) -> PathBuf {
        let other = self.root.path().join("elsewhere/story");
        std::fs::create_dir_all(other.parent().unwrap()).unwrap();
        std::fs::write(&other, b"#!/bin/sh\n").unwrap();
        other
    }
}

/// A pid the platform cannot have live: `kill -0` answers ESRCH.
fn dead_pid() -> u32 {
    i32::MAX as u32
}

#[test]
fn a_live_daemon_on_the_expected_port_from_the_lease_is_ours() {
    let fx = Fixture::new();
    let pf = fx.portfile(std::process::id(), 4321, &fx.leased);
    let out = check(&pf, "4321", &fx.leased);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stderr(&out).is_empty(), "a passing check says nothing");
}

#[test]
fn the_exe_comparison_is_by_inode_not_spelling() {
    let fx = Fixture::new();
    let pf = fx.portfile(std::process::id(), 4321, &fx.other_spelling_of_lease());
    let out = check(&pf, "4321", &fx.leased);
    assert!(out.status.success(), "{}", stderr(&out));
}

/// The SH-321 restart: a different pid, same port, same lease. The check must
/// not know or care which pid the run started with.
#[test]
fn a_restarted_daemon_on_the_same_port_from_the_lease_is_still_ours() {
    let fx = Fixture::new();
    // The "original" pid is irrelevant to the function's inputs by design:
    // it takes no pid at all. Prove that by handing it a portfile whose pid is
    // any live pid other than a hypothetical recorded one.
    let pf = fx.portfile(std::process::id(), 4321, &fx.leased);
    let out = check(&pf, "4321", &fx.leased);
    assert!(out.status.success(), "{}", stderr(&out));
    let script = std::fs::read_to_string(check_script()).unwrap();
    let signature = script
        .lines()
        .find(|l| l.contains("local portfile=\"$1\""))
        .expect("the function binds its positional arguments on one line");
    assert!(
        !signature.contains("pid"),
        "the function must take no expected pid; a restart keeps the port, not the pid \
         (SH-321): {signature}"
    );
}

#[test]
fn a_moved_port_is_named_as_one_replaced_daemon() {
    let fx = Fixture::new();
    let pf = fx.portfile(std::process::id(), 5555, &fx.leased);
    let out = check(&pf, "4321", &fx.leased);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(
        err.contains("port 4321") && err.contains("port 5555"),
        "{err}"
    );
    assert!(err.contains("not a tree failure"), "{err}");
    assert!(err.contains("SH-627"), "names the misread shape: {err}");
}

#[test]
fn a_daemon_from_another_build_is_not_ours() {
    let fx = Fixture::new();
    let other = fx.other_build();
    let pf = fx.portfile(std::process::id(), 4321, &other);
    let out = check(&pf, "4321", &fx.leased);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains(&other.display().to_string()), "{err}");
    assert!(err.contains(&fx.leased.display().to_string()), "{err}");
}

#[test]
fn a_dead_pid_behind_a_live_looking_portfile_is_named() {
    let fx = Fixture::new();
    let pf = fx.portfile(dead_pid(), 4321, &fx.leased);
    let out = check(&pf, "4321", &fx.leased);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains(&format!("pid {}", dead_pid())), "{err}");
    assert!(err.contains("dead"), "{err}");
}

#[test]
fn a_missing_portfile_is_named() {
    let fx = Fixture::new();
    let pf = fx.root.path().join("absent.json");
    let out = check(&pf, "4321", &fx.leased);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(
        err.contains(&pf.display().to_string()) && err.contains("gone"),
        "{err}"
    );
}

#[test]
fn an_unparsable_portfile_fails_closed() {
    let fx = Fixture::new();
    let pf = fx.root.path().join("daemon.json");
    std::fs::write(&pf, b"not json").unwrap();
    let out = check(&pf, "4321", &fx.leased);
    assert!(!out.status.success(), "garbage must never read as ours");
    assert!(!stderr(&out).is_empty(), "and must say something");
}

#[test]
fn facts_are_checked_in_the_order_a_reader_can_act_on() {
    // Port before exe before liveness: a moved port is the incident's own
    // signature and the most useful first word; a dead pid on the right port
    // is a different story. Pin the order by presenting a portfile that fails
    // several facts at once.
    let fx = Fixture::new();
    let other = fx.other_build();
    let pf = fx.portfile(dead_pid(), 5555, &other);
    let out = check(&pf, "4321", &fx.leased);
    let err = stderr(&out);
    assert!(err.contains("moved from port"), "{err}");
    assert!(
        !err.contains("is dead"),
        "only the first failing fact is reported: {err}"
    );
}
