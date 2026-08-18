//! The launchd login agent: its plist, written **and** read.
//!
//! `story daemon install` registers a user agent that starts the daemon at
//! login, so the dashboard is there after a reboot without anybody running a
//! command. That plist is a durable, machine-wide artifact — it outlives the
//! shell that created it, every build it names, and most reboots — which is
//! what makes both of this module's halves necessary.
//!
//! # Why this is a module of its own
//!
//! SH-411 is about to give the plist a *reader*, and a format with a writer in
//! one file and a hand-rolled reader in another is the SH-136 class this
//! project has already paid for four times: the two drift, agree with each
//! other about nothing, and no test is looking. This module is where both
//! halves will live, so a round-trip test can keep them honest.

use std::path::{Path, PathBuf};

use crate::env::Environment;

/// The launchd label for the user agent.
///
/// Reverse-DNS under the author's own domain, which is the convention every
/// other bundle identifier in this ecosystem follows.
pub const LAUNCHD_LABEL: &str = "io.mikey.storyhook.daemon";

/// Where the launchd agent's plist goes.
pub fn path(env: &Environment) -> PathBuf {
    env.home()
        .join("Library/LaunchAgents")
        .join(format!("{LAUNCHD_LABEL}.plist"))
}

/// The launchd agent definition.
///
/// `KeepAlive` is deliberately absent. The daemon is *supposed* to exit on a
/// version-skew restart, and an agent that resurrected it immediately would race
/// the client that just asked for a newer one. `RunAtLoad` is what this is for:
/// the dashboard is there after a reboot without anybody running a command.
pub fn plist(exe: &Path, env: &Environment) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LAUNCHD_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
{store}        <string>daemon</string>
        <string>--serve</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>ProcessType</key>
    <string>Background</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
</dict>
</plist>
"#,
        exe = exe.display(),
        // A login agent for the default store needs no flag, and leaving it off
        // keeps the plist identical to the one every existing installation has.
        // For any other store the flag is required rather than tidy: the log
        // path below is already this store's, so an agent without it would run
        // one store's daemon while writing into another's directory.
        store = if env.store().is_default() {
            String::new()
        } else {
            format!(
                "        <string>--store-path</string>\n        <string>{}</string>\n",
                env.store_path().display()
            )
        },
        log = env.daemon_log().display(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("storyhook-daemon-agent-")
            .tempdir_in("/private/tmp")
            .expect("a scratch directory")
    }

    /// **`RunAtLoad` yes, `KeepAlive` no**, and both halves are decisions rather
    /// than defaults.
    ///
    /// It was worth re-taking once the daemon became mandatory (SH-114): an
    /// optional process that nothing restarts is a choice, and a *required* one
    /// that nothing restarts sounds like an oversight. It is not, and the
    /// reasons got stronger rather than weaker.
    ///
    /// - **Availability is already self-healing.** Every client calls
    ///   `lifecycle::ensure`, so a daemon that is not running is started by
    ///   whoever needs it next. A restarter would be a second mechanism for
    ///   something that has one.
    /// - **A restarter is now more dangerous than it was.** `spawn_locked`
    ///   stands the old daemon down and *then* claims the pidfile, so launchd
    ///   racing that window turns a deterministic failure into an intermittent
    ///   one — and since SH-114 that window is on every command's path, not on
    ///   the subset that chose the daemon.
    /// - **The version-upgrade race survives verbatim**: an agent that restarts
    ///   the daemon immediately races the client that just asked it to stop
    ///   because the binary moved.
    /// - **`KeepAlive{SuccessfulExit:false}`**, the surgical variant that looks
    ///   like it avoids all of the above, is the worst of them. It turns "the
    ///   store is damaged and the daemon cannot open it" — the exact scenario
    ///   the cannot-start diagnostic exists for — into a respawn loop on
    ///   launchd's ten-second throttle.
    #[test]
    fn the_agent_runs_the_daemon_at_login_and_never_resurrects_it() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let plist = plist(Path::new("/usr/local/bin/story"), &env);
        assert!(plist.contains("<string>daemon</string>"));
        assert!(plist.contains("<string>--serve</string>"));
        assert!(plist.contains(LAUNCHD_LABEL));
        assert!(
            plist.contains("<key>RunAtLoad</key>"),
            "the daemon is started at login, so a machine that has just booted \
             has a dashboard without anybody typing a command: {plist}"
        );
        assert!(
            !plist.contains("KeepAlive"),
            "and it is never restarted. A restarter races `spawn_locked`'s \
             stand-down window on every command's path, and the surgical \
             `SuccessfulExit: false` variant turns a damaged store into a \
             respawn loop on launchd's ten-second throttle: {plist}"
        );
    }

    /// The agent's log path is already this store's. An agent that ran the
    /// *default* store's daemon while writing into a named store's directory
    /// would be describing one process and starting another.
    #[test]
    fn the_agent_names_a_store_that_is_not_the_default_one() {
        let dir = scratch();
        let named = dir.path().join("named.db");
        let env = Environment::at(dir.path());
        let default_plist = plist(Path::new("/usr/local/bin/story"), &env);
        assert!(
            !default_plist.contains("--store-path"),
            "the default store's agent must stay byte-identical to the one every \
             existing installation has"
        );

        let env = env.with_store(
            crate::env::StoreLocation::resolve(
                Some(&named),
                &crate::env::StoreVars::default(),
                dir.path(),
            )
            .expect("resolving a named store"),
        );
        let plist = plist(Path::new("/usr/local/bin/story"), &env);
        assert!(plist.contains("<string>--store-path</string>"), "{plist}");
        assert!(
            plist.contains(&format!("<string>{}</string>", env.store_path().display())),
            "{plist}"
        );
    }

    #[test]
    fn the_agent_label_follows_the_bundle_convention() {
        assert_eq!(LAUNCHD_LABEL, "io.mikey.storyhook.daemon");
    }
}
