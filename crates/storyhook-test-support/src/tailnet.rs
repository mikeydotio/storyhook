//! A `PATH` on which `tailscale` cannot succeed.
//!
//! Since SH-186, a daemon this harness starts probes for a tailnet on a
//! background thread moments after it starts (`daemon::serve::tailnet_reprobe`),
//! and rewrites its own portfile the instant a probe succeeds
//! (`daemon::lifecycle::run`'s `on_late_tailnet_bind`). On a tailnet-equipped
//! machine that now happens for nearly every daemon shortly after start, not
//! only for SH-146's rare self-heal — so a fixture that doctors a live daemon's
//! portfile and needs the doctoring to hold still cannot let that background
//! probe ever succeed.
//!
//! `tests/web_test.rs`'s `web_open_falls_back_to_the_bare_url_when_arming_fails`
//! was the first fixture that needed this denied outright, and did so by
//! narrowing `PATH` to just the directory holding the binary under test — which
//! also hides `sh` and `git`, harmless there but not everywhere. This is the
//! same idea generalized to a shim, the shape `tests/tailnet_advertise.rs` and
//! `tests/tailnet_startup.rs` already use for a `tailscale` that answers a
//! fixed identity or wedges: here it always fails, so `sh` and `git` stay
//! reachable through the rest of `PATH`.

use std::ffi::OsString;

use tempfile::TempDir;

use crate::env::TestEnv;
use crate::scratch::scratch_dir;

/// `env`'s usual `PATH` ([`TestEnv::path_with_binary`]), with a `tailscale`
/// ahead of it that always exits nonzero.
///
/// The returned [`TempDir`] must outlive whatever command resolves
/// `tailscale` through the returned `PATH` — the daemon does that resolution
/// when its background probe fires, not when this function returns, so
/// dropping the guard early reopens the exact race this exists to close.
#[must_use]
pub fn path_without_tailscale(env: &TestEnv) -> (TempDir, OsString) {
    let shim = scratch_dir();
    let path = shim.path().join("tailscale");
    std::fs::write(&path, "#!/bin/sh\nexit 1\n").expect("writing the tailscale-denial shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("making the shim executable");
    }
    let mut entries = vec![shim.path().to_path_buf()];
    entries.extend(std::env::split_paths(&env.path_with_binary()));
    let joined = std::env::join_paths(entries).expect("joining PATH");
    (shim, joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shim must actually deny `tailscale` — not merely be present, but
    /// resolve ahead of any real one and fail when run.
    #[test]
    fn the_shimmed_tailscale_exits_nonzero() {
        let env = TestEnv::isolated();
        let (_guard, path) = path_without_tailscale(&env);
        let status = std::process::Command::new("tailscale")
            .arg("status")
            .env("PATH", &path)
            .status()
            .expect("running the shimmed tailscale");
        assert!(
            !status.success(),
            "the denial shim must make every `tailscale` invocation fail"
        );
    }

    /// `sh` and `git` must still resolve — only `tailscale` is denied.
    #[test]
    fn everything_else_on_path_still_resolves() {
        let env = TestEnv::isolated();
        let (_guard, path) = path_without_tailscale(&env);
        for tool in ["sh", "git"] {
            let status = std::process::Command::new(tool)
                .arg("--version")
                .env("PATH", &path)
                .status()
                .unwrap_or_else(|e| panic!("running {tool} --version: {e}"));
            assert!(status.success(), "{tool} must still resolve on {path:?}");
        }
    }
}
