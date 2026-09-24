//! An uninstalled build is refused `story plugin install|uninstall|reinstall`
//! (SH-760).
//!
//! The incident: `tests/invoker_seam.rs` ran `plugin uninstall claude`
//! in-process, with the test binary's own `HOME` and `PATH`, and so removed
//! the developer's real Claude Code registration, its cache and its install
//! receipt on every run — under `make test` and under a bare `cargo test`
//! alike. The guard (`storyhook::plugin::guard`) refuses the verbs to any
//! test build and to any binary still in its build directory, before a
//! provider is invoked or a file is touched, unless the override says the
//! home is the operator's to change.
//!
//! Two binaries, one build, as in `tests/seat_guard.rs`: the test binary
//! itself, uninstalled by construction, and `installed_copy()`, the same
//! bytes copied out of the build directory — which is still a test build, and
//! is what the test-build clause exists for.
//!
//! | subject | override | outcome |
//! |---|---|---|
//! | test binary, each verb, fake `claude` first on `PATH` | unset | exit 2; the fake never runs; residue and receipt untouched |
//! | installed copy, `uninstall claude` | unset | exit 2 by the test-build clause alone |
//! | installed copy, `install` then `uninstall` | `1` | proceeds; the receipt records the override — the control |
//! | this test binary, in-process `plugin::uninstall` | unset | `AppError::Usage`, nothing invoked — the incident |
//!
//! **Why the refusal rows check what is left afterwards.** A guard one line
//! too low — after `provider_available` — is indistinguishable from a correct
//! one in the exit code and the message; the fake's invocation log and the
//! planted residue are the observables that separate them.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};

use storyhook::error::AppError;
use storyhook::plugin::guard::OVERRIDE_VAR;
use storyhook_test_support::{TestEnv, scratch_dir};
use tempfile::TempDir;

/// Records every invocation and answers `plugin *` with success, so a run
/// that reaches the provider is visible and a permitted run can complete.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
set -u
printf '%s\n' "$*" >> "$HOME/claude-invocations"
if [ "${1:-}" = "--version" ]; then
  echo 'Claude Code 2.1.280'
  exit 0
fi
if [ "${1:-}" = plugin ]; then exit 0; fi
echo "unexpected claude invocation: $*" >&2
exit 64
"#;

/// One isolated home with a fake `claude`, and its own daemon — one fixture
/// per test, because the test binary and the installed copy are "another
/// build" to each other's daemon and the seat guard would answer first.
struct Fixture {
    env: TestEnv,
    _bin_dir: TempDir,
    bin: PathBuf,
    cwd: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let env = TestEnv::isolated();
        let bin_dir = scratch_dir();
        let fake = bin_dir.path().join("claude");
        fs::write(&fake, FAKE_CLAUDE).unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        // `preflight_provider` wants `~/.claude` to exist for Claude.
        fs::create_dir_all(env.home().join(".claude")).unwrap();
        Self {
            env,
            bin: bin_dir.path().to_path_buf(),
            _bin_dir: bin_dir,
            cwd: scratch_dir(),
        }
    }

    fn prepare(&self, mut command: Command, override_set: bool) -> Command {
        command.env("PATH", format!("{}:/usr/bin:/bin", self.bin.display()));
        if override_set {
            command.env(OVERRIDE_VAR, "1");
        } else {
            command.env_remove(OVERRIDE_VAR);
        }
        command
    }

    /// The test binary, still in its build directory.
    fn test_binary(&self, override_set: bool, args: &[&str]) -> Output {
        self.prepare(self.env.raw_story(self.cwd.path()), override_set)
            .args(args)
            .output()
            .expect("running the test binary")
    }

    /// The same bytes, copied out of the build directory.
    fn installed_copy(&self, override_set: bool, args: &[&str]) -> Output {
        self.prepare(self.env.raw_installed_story(self.cwd.path()), override_set)
            .args(args)
            .output()
            .expect("running the installed copy")
    }

    fn invocations(&self) -> String {
        fs::read_to_string(self.env.home().join("claude-invocations")).unwrap_or_default()
    }

    fn receipt(&self) -> PathBuf {
        self.env.data_dir().join("provider-installs/claude")
    }

    /// What a lost registration leaves behind, planted so a refusal that ran
    /// the sweep anyway would be caught by its absence.
    fn plant_residue(&self) -> PathBuf {
        let cache = self
            .env
            .home()
            .join(".claude/plugins/cache/storyhook/story/3.0.3");
        fs::create_dir_all(&cache).unwrap();
        self.env.home().join(".claude/plugins/cache/storyhook")
    }
}

fn combined(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn assert_refused(out: &Output, verb_line: &str, clause: &str) -> String {
    let text = combined(out);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a refusal is a usage failure:\n{text}"
    );
    assert!(text.contains(&format!("refusing `{verb_line}`")), "{text}");
    assert!(text.contains(clause), "{text}");
    assert!(text.contains(OVERRIDE_VAR), "{text}");
    assert!(text.contains("make install"), "{text}");
    assert!(text.contains("Nothing has been changed"), "{text}");
    text
}

fn no_override_in_this_process() {
    assert!(
        std::env::var_os(OVERRIDE_VAR).is_none(),
        "{OVERRIDE_VAR} is exported into this test's environment; unset it or run under `make test`"
    );
}

#[test]
fn a_test_binary_is_refused_every_verb_before_any_provider_call() {
    no_override_in_this_process();
    for (args, verb_line) in [
        (
            &["plugin", "install", "claude"][..],
            "story plugin install claude",
        ),
        (
            &["plugin", "uninstall", "claude"][..],
            "story plugin uninstall claude",
        ),
        (&["plugin", "reinstall"][..], "story plugin reinstall"),
    ] {
        let fixture = Fixture::new();
        let residue = fixture.plant_residue();
        let out = fixture.test_binary(false, args);
        let text = assert_refused(&out, verb_line, "test build");
        assert_eq!(
            fixture.invocations(),
            "",
            "{verb_line}: the provider must never be invoked:\n{text}"
        );
        assert!(
            residue.is_dir(),
            "{verb_line}: the residue sweep must not have run:\n{text}"
        );
        assert!(
            !fixture.receipt().exists(),
            "{verb_line}: no receipt may be written or tombstoned:\n{text}"
        );
    }
}

/// Copied out of the build directory, the binary is still a test build, and
/// that clause alone refuses it — the build-directory clause must not be
/// what the message cites.
#[test]
fn an_installed_copy_of_a_test_build_is_refused_by_the_test_build_clause_alone() {
    no_override_in_this_process();
    let fixture = Fixture::new();
    let out = fixture.installed_copy(false, &["plugin", "uninstall", "claude"]);
    let text = assert_refused(&out, "story plugin uninstall claude", "test build");
    assert!(
        !text.contains("still where cargo built it"),
        "the copy has left its build directory:\n{text}"
    );
    assert_eq!(fixture.invocations(), "", "{text}");
}

/// The override is the operator's word, and the control: the same copy
/// proceeds, invokes the provider, and its receipt says the word was given.
#[test]
fn the_override_permits_the_verbs_and_the_receipt_records_it() {
    let fixture = Fixture::new();
    let out = fixture.installed_copy(true, &["plugin", "install", "claude"]);
    assert!(out.status.success(), "{}", combined(&out));
    let invocations = fixture.invocations();
    assert!(invocations.contains("--version"), "{invocations}");
    assert!(
        invocations.contains("plugin marketplace add"),
        "{invocations}"
    );
    let body = fs::read_to_string(fixture.receipt()).expect("the install wrote a receipt");
    assert!(body.starts_with("state installed\n"), "{body}");
    assert!(body.contains("\nbuild test\n"), "{body}");
    assert!(body.contains("\noverride yes\n"), "{body}");

    let out = fixture.installed_copy(true, &["plugin", "uninstall", "claude"]);
    assert!(out.status.success(), "{}", combined(&out));
    let body = fs::read_to_string(fixture.receipt()).expect("the uninstall left a tombstone");
    assert!(body.starts_with("state uninstalled\n"), "{body}");
    assert!(body.contains("\noverride yes\n"), "{body}");
}

/// The incident itself: this process is a test binary, and the call goes
/// nowhere near `HOME`. Asserted through the library, exactly as
/// `tests/invoker_seam.rs` reaches it.
#[test]
fn the_in_process_call_is_refused_as_a_usage_error() {
    no_override_in_this_process();
    let cwd = scratch_dir();
    let refusals = [
        storyhook::plugin::uninstall("claude", cwd.path()).expect_err("uninstall"),
        storyhook::plugin::install("claude", cwd.path()).expect_err("install"),
        storyhook::plugin::reinstall::run(cwd.path()).expect_err("reinstall"),
    ];
    for error in refusals {
        assert!(matches!(error, AppError::Usage(_)), "{error:?}");
        let text = error.to_string();
        assert!(text.starts_with("refusing `story plugin "), "{text}");
        assert!(text.contains("test build"), "{text}");
        assert!(text.contains(OVERRIDE_VAR), "{text}");
    }
}
