//! [`TestEnv`] — a storyhook environment with nothing of the developer's own
//! in it.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tempfile::TempDir;

use crate::project::ProjectBuilder;
use crate::scratch::scratch_dir_named;

/// The environment variables a storyhook process is allowed to see.
///
/// `HOME` is the one storyhook reads today (`~/.storyhook/registry.toml`, the
/// dashboard pid file). The rest are set **unconditionally and from the start**,
/// before anything reads them: the data-layer rearchitecture moves storyhook's
/// state to an XDG-shaped global store, and a harness that only isolates what
/// today's code happens to consult would let the very first commit of that
/// store write into the developer's real `~/.local/share/storyhook`. Isolating
/// a variable nobody reads costs nothing; discovering afterwards that a test
/// run ate real data costs everything.
const ISOLATED_VARS: [&str; 5] = [
    "HOME",
    "XDG_DATA_HOME",
    "XDG_CONFIG_HOME",
    "XDG_STATE_HOME",
    "STORYHOOK_DATA_DIR",
];

/// A fully isolated storyhook environment: a private `HOME`, private XDG
/// directories, and a private storyhook data directory, all under a fixture
/// root that is reclaimed when the environment drops.
///
/// A `TestEnv` never mutates the *current* process's environment. Integration
/// tests run in parallel threads inside one binary, so a `set_var` here would
/// be a data race across every test in the file — and it would break the tests
/// that legitimately assert against the real `HOME`. Isolation is applied to
/// each child [`std::process::Command`] instead.
pub struct TestEnv {
    /// Held for its `Drop`: the whole environment lives inside it.
    _root: TempDir,
    home: PathBuf,
    data_home: PathBuf,
    config_home: PathBuf,
    state_home: PathBuf,
    data_dir: PathBuf,
}

impl TestEnv {
    /// One environment per test binary, created on first use.
    ///
    /// The right default: an isolated `HOME` is worth having in every test, but
    /// tests within a binary do not need isolating from *each other* (they are
    /// already separated by their own project fixtures), and minting a fresh
    /// directory tree per test would pay that cost hundreds of times per run.
    ///
    /// Reach for [`TestEnv::isolated`] when a test asserts on the *contents* of
    /// the environment — global state a sibling test could also be writing.
    pub fn shared() -> &'static TestEnv {
        static SHARED: OnceLock<TestEnv> = OnceLock::new();
        SHARED.get_or_init(|| TestEnv::build("shared-"))
    }

    /// A private environment, reclaimed when the returned value drops.
    pub fn isolated() -> TestEnv {
        TestEnv::build("env-")
    }

    fn build(label: &str) -> TestEnv {
        let root = scratch_dir_named(label);
        let home = root.path().join("home");
        let env = TestEnv {
            home: home.clone(),
            data_home: home.join(".local/share"),
            config_home: home.join(".config"),
            state_home: home.join(".local/state"),
            data_dir: home.join(".local/share/storyhook"),
            _root: root,
        };
        for dir in [
            &env.home,
            &env.data_home,
            &env.config_home,
            &env.state_home,
            &env.data_dir,
        ] {
            std::fs::create_dir_all(dir)
                .unwrap_or_else(|e| panic!("creating {}: {e}", dir.display()));
        }
        env
    }

    /// This environment's private `HOME`.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// This environment's private `XDG_DATA_HOME`.
    pub fn data_home(&self) -> &Path {
        &self.data_home
    }

    /// This environment's private `XDG_CONFIG_HOME`.
    pub fn config_home(&self) -> &Path {
        &self.config_home
    }

    /// This environment's private `XDG_STATE_HOME`.
    pub fn state_home(&self) -> &Path {
        &self.state_home
    }

    /// This environment's private `STORYHOOK_DATA_DIR` — where the global
    /// store will live once the data layer moves there.
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// The `(name, value)` pairs every command this environment builds carries.
    pub fn vars(&self) -> [(&'static str, &Path); 5] {
        [
            (ISOLATED_VARS[0], self.home.as_path()),
            (ISOLATED_VARS[1], self.data_home.as_path()),
            (ISOLATED_VARS[2], self.config_home.as_path()),
            (ISOLATED_VARS[3], self.state_home.as_path()),
            (ISOLATED_VARS[4], self.data_dir.as_path()),
        ]
    }

    /// The daemon-shaped settings every command this environment builds carries.
    ///
    /// Two, and neither is optional:
    ///
    /// * **`STORYHOOK_DAEMON_ADDR` is loopback port 0.** The daemon's preferred
    ///   port is 3456, which is where a developer's own dashboard lives; a suite
    ///   that could bind it would, on any machine where it happened to be free,
    ///   and then a test would be talking to something a human was using.
    /// * **`STORYHOOK_PARENT_PID` is this test binary.** Every `story` this
    ///   environment runs inherits it, so a daemon one of them spawns inherits
    ///   it too and exits when this process does — including when this process
    ///   is killed rather than ended. A leaked daemon poisons every later run on
    ///   the machine, which is not a hypothetical: it cost 78 of 139 tests once.
    pub fn daemon_vars(&self) -> [(&'static str, String); 2] {
        [
            ("STORYHOOK_DAEMON_ADDR", "127.0.0.1:0".to_string()),
            ("STORYHOOK_PARENT_PID", std::process::id().to_string()),
        ]
    }

    /// Points `cmd` at this environment. Every command the harness builds goes
    /// through here, including the `git` invocations behind
    /// [`ProjectBuilder`] — a fixture repo that reads the developer's real
    /// `~/.gitconfig` is not isolated either.
    pub fn apply(&self, cmd: &mut std::process::Command) {
        for (name, value) in self.vars() {
            cmd.env(name, value);
        }
        for (name, value) in self.daemon_vars() {
            cmd.env(name, value);
        }
    }

    /// An `assert_cmd` handle on the `story` binary, running in `cwd`, with
    /// this environment applied. Replaces the per-file `fn story(dir)` helper
    /// that every integration test used to declare for itself.
    pub fn story(&self, cwd: impl AsRef<Path>) -> assert_cmd::Command {
        let mut cmd = assert_cmd::Command::new(story_binary());
        cmd.current_dir(cwd.as_ref());
        for (name, value) in self.vars() {
            cmd.env(name, value);
        }
        for (name, value) in self.daemon_vars() {
            cmd.env(name, value);
        }
        cmd
    }

    /// A plain [`std::process::Command`] on the `story` binary, for the tests
    /// that need to `spawn` several at once and race them (an `assert_cmd`
    /// command runs to completion).
    pub fn raw_story(&self, cwd: impl AsRef<Path>) -> std::process::Command {
        let mut cmd = std::process::Command::new(story_binary());
        cmd.current_dir(cwd.as_ref());
        self.apply(&mut cmd);
        cmd
    }

    /// Starts building a storyhook project inside this environment.
    pub fn project(&self) -> ProjectBuilder<'_> {
        ProjectBuilder::new(self)
    }

    /// This environment as the library sees it, for the in-process callers that
    /// cannot be isolated by setting a variable.
    ///
    /// The bridge between the two halves of the harness: [`Self::vars`] isolates
    /// a child process, and this isolates a direct library call. They must agree
    /// — a test that starts a server in-process and then drives it with a `story`
    /// subprocess is looking at one store or it is testing nothing —
    /// which is what `the_in_process_environment_matches_what_a_child_resolves`
    /// pins.
    #[must_use]
    pub fn environment(&self) -> storyhook::env::Environment {
        storyhook::env::Environment::at(&self.home)
    }

    /// The daemon this environment's `story` commands share, if one is running.
    #[must_use]
    pub fn daemon(&self) -> Option<storyhook::daemon::lifecycle::DaemonInfo> {
        storyhook::daemon::lifecycle::read_info(&self.environment())
    }
}

/// The `story` binary under test — always this build's, never a globally
/// installed one.
///
/// Resolved from the running test binary's own location rather than
/// `assert_cmd::cargo::cargo_bin`, whose failure message (`CARGO_BIN_EXE_story`
/// is unset) names a compile-time variable that a *different* crate's test
/// binary could never have had, and so points at nothing actionable.
pub fn story_binary() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        // Set for the integration tests of the package that declares the
        // binary; absent everywhere else, including this crate's own tests.
        if let Some(path) = std::env::var_os("CARGO_BIN_EXE_story") {
            return PathBuf::from(path);
        }
        let exe = std::env::current_exe().expect("locating the running test binary");
        let mut dir = exe
            .parent()
            .expect("a test binary always has a parent directory")
            .to_path_buf();
        if dir.ends_with("deps") {
            dir.pop();
        }
        let candidate = dir.join(format!("story{}", std::env::consts::EXE_SUFFIX));
        assert!(
            candidate.is_file(),
            "the `story` binary is not at {} — this harness only ever runs the binary this \
             build produced. Run `cargo build -p storyhook` (or `make test`, which does).",
            candidate.display()
        );
        candidate
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_isolated_variable_reaches_the_child_process() {
        let env = TestEnv::isolated();
        // `env` with no arguments prints the child's whole environment, so
        // this checks what the child actually receives rather than what the
        // harness believes it set.
        let mut cmd = std::process::Command::new("/usr/bin/env");
        env.apply(&mut cmd);
        let out = cmd.output().expect("running env(1)");
        let seen = String::from_utf8_lossy(&out.stdout).into_owned();

        for (name, value) in env.vars() {
            let expected = format!("{name}={}", value.display());
            assert!(
                seen.lines().any(|line| line == expected),
                "the child process must see {expected}; it saw:\n{seen}"
            );
        }
    }

    #[test]
    fn isolation_covers_every_variable_the_rearchitecture_will_read() {
        // Pinned by name, not by count: the flip to a global XDG-shaped store
        // is the moment an unisolated variable starts writing to the
        // developer's real data, and by then the harness must already have
        // been isolating it.
        let env = TestEnv::isolated();
        let names: Vec<_> = env.vars().iter().map(|(name, _)| *name).collect();
        assert_eq!(names, ISOLATED_VARS);
    }

    /// The production port belongs to whoever is using it, and on a machine
    /// where it happens to be free a suite that could take it would.
    #[test]
    fn no_command_this_harness_builds_can_bind_the_production_port() {
        let env = TestEnv::isolated();
        let addr = env
            .daemon_vars()
            .into_iter()
            .find(|(name, _)| *name == "STORYHOOK_DAEMON_ADDR")
            .map(|(_, value)| value)
            .expect("the harness must pin the daemon address");
        assert_eq!(addr, "127.0.0.1:0");
    }

    #[test]
    fn every_command_names_this_process_as_the_daemons_parent() {
        let env = TestEnv::isolated();
        let mut cmd = std::process::Command::new("/usr/bin/env");
        env.apply(&mut cmd);
        let out = cmd.output().expect("running env(1)");
        let seen = String::from_utf8_lossy(&out.stdout).into_owned();
        let expected = format!("STORYHOOK_PARENT_PID={}", std::process::id());
        assert!(
            seen.lines().any(|line| line == expected),
            "a daemon that does not know its parent cannot outlive it politely; \
             expected {expected} in:\n{seen}"
        );
    }

    #[test]
    fn a_story_command_is_isolated_from_the_real_home() {
        let env = TestEnv::isolated();
        let real_home = std::env::var("HOME").expect("HOME must be set to run this test");
        assert_ne!(
            env.home(),
            Path::new(&real_home),
            "an isolated environment that reuses the developer's HOME is not isolated"
        );

        let dir = crate::scratch_dir();
        let out = env
            .story(dir.path())
            .arg("--version")
            .output()
            .expect("running story --version");
        assert!(
            out.status.success(),
            "story --version must succeed: {out:?}"
        );
    }

    /// The two halves of the harness must name the same directories: one
    /// isolates a child process by setting variables, the other isolates an
    /// in-process call by being passed down. A test that used both and got two
    /// different stores would pass while proving nothing.
    #[test]
    fn the_in_process_environment_matches_what_a_child_resolves() {
        let env = TestEnv::isolated();
        let environment = env.environment();
        assert_eq!(environment.data_home(), env.data_dir());
        assert_eq!(environment.home(), env.home());
        assert_eq!(
            environment.state_home(),
            env.state_home().join("storyhook"),
            "a child resolves $XDG_STATE_HOME/storyhook"
        );
    }

    #[test]
    fn the_shared_environment_is_the_same_one_every_time() {
        assert_eq!(
            TestEnv::shared().home(),
            TestEnv::shared().home(),
            "shared() must hand back one environment per test binary, not a fresh one per call"
        );
    }

    #[test]
    fn isolated_environments_do_not_share_a_home() {
        let a = TestEnv::isolated();
        let b = TestEnv::isolated();
        assert_ne!(a.home(), b.home());
        assert_ne!(a.home(), TestEnv::shared().home());
    }

    #[test]
    fn building_a_command_never_mutates_the_current_process_environment() {
        // Tests in a binary run as parallel threads in one process: a harness
        // that set process-wide variables would race every sibling test, and
        // would break the tests that assert against the real HOME.
        let before = std::env::var_os("HOME");
        let env = TestEnv::isolated();
        let dir = crate::scratch_dir();
        let _ = env.story(dir.path());
        assert_eq!(std::env::var_os("HOME"), before);
    }

    #[test]
    fn the_binary_under_test_is_this_builds_own() {
        let bin = story_binary();
        assert!(bin.is_file(), "{} must exist", bin.display());
        assert!(
            !bin.starts_with(dirs_home_bin()),
            "the harness must never run an installed `story` — a stale global binary turns a \
             red test green (and the reverse); got {}",
            bin.display()
        );
    }

    fn dirs_home_bin() -> PathBuf {
        PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/bin")
    }
}
