//! [`TestEnv`] — a storyhook environment with nothing of the developer's own
//! in it.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tempfile::TempDir;

use crate::project::ProjectBuilder;
use crate::scratch::scratch_dir_named;

/// The environment variables a storyhook process is allowed to see, and what
/// each of them is set to, come from one place:
/// [`storyhook::env::test_environment::TEST_ENVIRONMENT`].
///
/// This harness used to carry its own two lists -- `ISOLATED_VARS` and
/// `CLEARED_VARS` -- which is how it came to be the only one of seven isolating
/// harnesses that cleared the developer's real `STORYHOOK_GITHUB_TOKEN`
/// (SH-153, fixed here and nowhere else) and one of only two that redirected
/// `HOME`. Both lists are read from the shared table now, so this harness and
/// the shell one cannot answer the same question differently.
///
/// [`Scope::StoryhookProcessOnly`] is what this harness passes, and it is the
/// whole reason `HOME` can be isolated here at all: isolation is applied to
/// each child [`std::process::Command`] rather than exported around a whole
/// run, so nothing but `story` ever sees the fake home. A shell wrapper around
/// `cargo` cannot do the same without costing cargo its registry.
use storyhook::env::test_environment::{self, Disposition, Scope, Setting};

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
    /// Every parameter of [`test_environment::TEST_ENVIRONMENT`], resolved
    /// against this environment's root. The single source for the accessors
    /// below, so no field can come to disagree with what a child is handed.
    settings: Vec<Setting>,
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
        // Resolved once, from the shared table, in the scope that permits
        // `HOME`: every path this environment answers with is one of these, so
        // there is no second place for the layout to be written down.
        let settings =
            test_environment::resolve(root.path(), std::process::id(), Scope::StoryhookProcessOnly);
        for dir in test_environment::directories(root.path()) {
            std::fs::create_dir_all(&dir)
                .unwrap_or_else(|e| panic!("creating {}: {e}", dir.display()));
        }
        TestEnv {
            _root: root,
            settings,
        }
    }

    /// One parameter's path, by name.
    ///
    /// Panics rather than returning an `Option`: every caller below names a
    /// parameter that is in the table, so an absent one means the table lost a
    /// parameter this harness still promises, and a `None` silently threaded
    /// onward would turn that into a fixture pointed somewhere unrelated.
    fn path_of(&self, name: &str) -> &Path {
        let setting = self
            .settings
            .iter()
            .find(|setting| setting.name == name)
            .unwrap_or_else(|| {
                panic!(
                    "{name} is no longer a test-environment parameter, but this \
                     harness still hands it out"
                )
            });
        Path::new(
            setting
                .value
                .as_ref()
                .unwrap_or_else(|| panic!("{name} is a removed parameter and has no path")),
        )
    }

    /// This environment's private `HOME`.
    pub fn home(&self) -> &Path {
        self.path_of("HOME")
    }

    /// This environment's private `XDG_DATA_HOME`.
    pub fn data_home(&self) -> &Path {
        self.path_of("XDG_DATA_HOME")
    }

    /// This environment's private `XDG_STATE_HOME`.
    pub fn state_home(&self) -> &Path {
        self.path_of("XDG_STATE_HOME")
    }

    /// This environment's private `STORYHOOK_DATA_DIR` — the directory the
    /// global store sits in.
    pub fn data_dir(&self) -> &Path {
        self.path_of("STORYHOOK_DATA_DIR")
    }

    /// The database file every `story` process this environment builds will
    /// use — the same one `$STORYHOOK_DATA_DIR` names, spelled the way
    /// `--store-path` and `$STORYHOOK_STORE_PATH` do.
    #[must_use]
    pub fn store_path(&self) -> &Path {
        self.path_of("STORYHOOK_STORE_PATH")
    }

    /// Every parameter this environment carries, resolved.
    ///
    /// A removed parameter has `value: None`; [`Self::apply`] turns that into an
    /// `env_remove` rather than an empty string, because a credential set to
    /// `""` is still a credential the child was handed.
    #[must_use]
    pub fn settings(&self) -> &[Setting] {
        &self.settings
    }

    /// The `(name, path)` pairs of the parameters that name a location.
    ///
    /// The removals are deliberately absent: a caller reconstructing an
    /// environment out of paths has nothing to do with a variable whose whole
    /// disposition is "not present".
    #[must_use]
    pub fn vars(&self) -> Vec<(&'static str, &Path)> {
        self.settings
            .iter()
            .filter_map(|setting| {
                setting
                    .value
                    .as_ref()
                    .filter(|_| {
                        test_environment::TEST_ENVIRONMENT.iter().any(|parameter| {
                            parameter.name == setting.name
                                && matches!(parameter.disposition, Disposition::Root(_))
                        })
                    })
                    .map(|value| (setting.name, Path::new(value)))
            })
            .collect()
    }

    /// The daemon-shaped settings every command this environment builds carries.
    ///
    /// Neither is optional, and the reason each exists is on
    /// [`daemon_containment`] — one copy of it, because a reason stated twice is
    /// a reason that can come to disagree with itself.
    #[must_use]
    pub fn daemon_vars(&self) -> Vec<(&'static str, String)> {
        daemon_containment()
    }

    /// `PATH` with the directory holding the binary under test in front.
    ///
    /// **The isolation is incomplete without this.** A managed git hook runs
    /// `story` *by name*, so a `git commit` in a fixture repository resolves it
    /// through `PATH` — which, left alone, is the developer's own installed
    /// build. That build is a different version of storyhook pointed at
    /// whatever store it defaults to, and it will happily write into the
    /// fixture: a pre-flip binary firing from a post-flip fixture's
    /// `post-commit` hook leaves a `.storyhook/lock` in the working tree, which
    /// is how this was found.
    ///
    /// Prepended rather than replaced: a hook still needs `git`, `sh` and
    /// `grep`.
    pub fn path_with_binary(&self) -> std::ffi::OsString {
        let dir = story_binary()
            .parent()
            .expect("the binary under test has a parent directory")
            .to_path_buf();
        let existing = std::env::var_os("PATH").unwrap_or_default();
        let mut entries = vec![dir];
        entries.extend(std::env::split_paths(&existing));
        std::env::join_paths(entries).expect("joining PATH")
    }

    /// Points `cmd` at this environment. Every command the harness builds goes
    /// through here, including the `git` invocations behind
    /// [`ProjectBuilder`] — a fixture repo that reads the developer's real
    /// `~/.gitconfig` is not isolated either.
    ///
    /// A removed parameter is `env_remove`d rather than set to `""`: an empty
    /// credential is still a credential the child was handed, and the two are
    /// only the same thing for the parameters that happen to be paths.
    pub fn apply(&self, cmd: &mut std::process::Command) {
        cmd.env("PATH", self.path_with_binary());
        for setting in &self.settings {
            match &setting.value {
                Some(value) => cmd.env(setting.name, value),
                None => cmd.env_remove(setting.name),
            };
        }
    }

    /// An `assert_cmd` handle on the `story` binary, running in `cwd`, with
    /// this environment applied. Replaces the per-file `fn story(dir)` helper
    /// that every integration test used to declare for itself.
    pub fn story(&self, cwd: impl AsRef<Path>) -> assert_cmd::Command {
        // Built as a `std::process::Command` and handed to `apply`, so the
        // isolation goes on through exactly one door. This used to carry its
        // own copy of the loop, which is a second list that can disagree with
        // the first. The removals still happen before the sets, so a test that
        // deliberately supplies a credential afterwards still wins.
        let mut inner = std::process::Command::new(story_binary());
        inner.current_dir(cwd.as_ref());
        self.apply(&mut inner);
        assert_cmd::Command::from_std(inner)
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
        storyhook::env::Environment::at(self.home())
    }

    /// The daemon this environment's `story` commands share, if one is running.
    #[must_use]
    pub fn daemon(&self) -> Option<storyhook::daemon::lifecycle::DaemonInfo> {
        storyhook::daemon::lifecycle::read_info(&self.environment())
    }

    /// Whether a daemon is holding this environment's store *now*.
    ///
    /// The pidfile lock rather than [`Self::daemon`], and the difference is the
    /// whole point: a portfile outlives the process that wrote it, so a test
    /// asking "is anything holding the database" would get a stale yes from one
    /// and the truth from the other.
    #[must_use]
    pub fn daemon_is_live(&self) -> bool {
        storyhook::daemon::lifecycle::is_live(&self.environment())
    }

    /// Stands down whatever daemon is holding this environment's store.
    ///
    /// A no-op when there is none, so it is safe to call before any test that
    /// wants the store to itself — which is every test that asks a question
    /// about bytes on disk. A live daemon answers reads from its own page cache
    /// and keeps a write-ahead log alive that would otherwise be checkpointed
    /// away.
    ///
    /// `StopMode::Force`, not the CLI's own graceful default: a test fixture
    /// must be bounded, and a daemon this call cannot get rid of is exactly
    /// the shape that should be killed rather than waited on.
    pub fn stop_daemon(&self) {
        let _ = storyhook::daemon::lifecycle::stop(
            &self.environment(),
            storyhook::daemon::lifecycle::StopMode::Force,
        );
    }
}

/// The parameters that stop a test-spawned daemon reaching anything real, for a
/// caller that has no environment root to hand out.
///
/// A free function rather than a method, because the sites that most need it are
/// the ones with no [`TestEnv`] to ask: several test files call `env_clear()` on
/// purpose, so that the variable *they* are about cannot arrive from the ambient
/// shell. A daemon with a cleared environment would prefer port 3456 — the
/// developer's own dashboard — and would outlive the run that made it.
///
/// **Derived, not listed.** These are exactly the parameters of
/// [`test_environment::TEST_ENVIRONMENT`] whose value does not come from a root
/// directory, which is the property a caller in this position actually has: it
/// has cleared its environment, so the removals are already satisfied, and it
/// has no root, so the paths are not its to set. A parameter added to the table
/// in either of those two shapes arrives here with no edit; one added as a path
/// correctly does not, because such a caller could not honour it.
///
/// One definition rather than a copy per site is deliberate. SH-136 is about a
/// hand-maintained list of places that export this pair, and adding more by hand
/// would have made that story worse.
#[must_use]
pub fn daemon_containment() -> Vec<(&'static str, String)> {
    // Resolved against a root that is never used: every parameter selected below
    // ignores it by construction, and the assertion is what keeps that true
    // rather than assumed.
    let unused_root = Path::new("/dev/null/no-root-is-needed-here");
    test_environment::resolve(unused_root, std::process::id(), Scope::Anywhere)
        .into_iter()
        .filter(|setting| {
            test_environment::TEST_ENVIRONMENT.iter().any(|parameter| {
                parameter.name == setting.name
                    && matches!(
                        parameter.disposition,
                        Disposition::Literal(_) | Disposition::OwnPid
                    )
            })
        })
        .map(|setting| {
            let value = setting
                .value
                .expect("a literal or a pid always has a value");
            let value = value
                .into_string()
                .expect("a literal and a pid are both valid UTF-8");
            assert!(
                !value.contains("no-root-is-needed-here"),
                "{} was selected as root-independent but resolved through the \
                 root anyway",
                setting.name
            );
            (setting.name, value)
        })
        .collect()
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

    /// This harness carries the WHOLE parameter set, in the table's own order.
    ///
    /// Pinned by name rather than by count, because a count is green while two
    /// parameters swap: the shell rendering and this one are compared to each
    /// other by sequence, and a harness that quietly reordered them would fail
    /// there with a message about the shell rather than about itself.
    #[test]
    fn isolation_covers_every_parameter_in_the_table() {
        let env = TestEnv::isolated();
        let names: Vec<&str> = env.settings().iter().map(|s| s.name).collect();
        let expected: Vec<&str> = storyhook::env::test_environment::TEST_ENVIRONMENT
            .iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(
            names, expected,
            "TestEnv applies the full table, HOME included: it isolates each \
             `story` child rather than exporting around a whole run, which is \
             the level at which HOME may be redirected"
        );
    }

    /// **A real credential must not reach a fixture.**
    ///
    /// Checked against what the child actually receives rather than what the
    /// harness believes it set, because the failure this prevents is invisible
    /// from inside: on a developer machine with `STORYHOOK_GITHUB_TOKEN`
    /// exported, every fixture child and every test daemon used to inherit that
    /// PAT, so what the suite did depended on whose shell ran it (SH-153). The
    /// variable is set here deliberately so the removal has something to remove.
    #[test]
    fn no_real_credential_reaches_a_fixture_child() {
        let env = TestEnv::isolated();
        let mut cmd = std::process::Command::new("/usr/bin/env");
        cmd.env("STORYHOOK_GITHUB_TOKEN", "ghp_the_developers_real_token");
        env.apply(&mut cmd);
        let out = cmd.output().expect("running env(1)");
        let seen = String::from_utf8_lossy(&out.stdout).into_owned();

        assert!(
            !seen.contains("ghp_the_developers_real_token"),
            "a fixture child must not inherit a credential; it saw:\n{seen}"
        );
        // Derived: every parameter the table says to remove, not the one this
        // test happened to plant. A `Clear` parameter added to the table is
        // checked here with no edit.
        for parameter in storyhook::env::test_environment::TEST_ENVIRONMENT {
            if parameter.disposition != Disposition::Clear {
                continue;
            }
            let name = parameter.name;
            assert!(
                !seen
                    .lines()
                    .any(|line| line.starts_with(&format!("{name}="))),
                "{name} must not reach the child at all"
            );
        }
    }

    /// A test that *wants* a credential still gets one — the harness clears
    /// before it applies, so a later `env` wins. `tests/github_sync_token.rs`
    /// depends on this, and it is the sort of ordering that breaks silently.
    #[test]
    fn a_test_can_still_supply_a_credential_on_purpose() {
        let env = TestEnv::isolated();
        let mut cmd = std::process::Command::new("/usr/bin/env");
        env.apply(&mut cmd);
        cmd.env("STORYHOOK_GITHUB_TOKEN", "ghp_supplied_by_the_test");
        let out = cmd.output().expect("running env(1)");
        let seen = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            seen.contains("ghp_supplied_by_the_test"),
            "a deliberate credential must survive the clearing:\n{seen}"
        );
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
        // Canonicalized on both sides: the store's *canonical* path is what
        // names its daemon's state directory, so two halves of the harness that
        // agreed only on the literal spelling would still be two daemons.
        assert_eq!(
            environment.store_path(),
            std::fs::canonicalize(env.data_dir())
                .expect("canonicalizing the fixture data directory")
                .join("store.db")
        );
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
