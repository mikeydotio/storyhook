//! `scripts/verify-window.sh` — the SH-545 verifier tmux mirror, provoked
//! directly against a stub `tmux` on `PATH`.
//!
//! SH-545 established a fixed session on the default server, independent log
//! readers and literal argv. SH-662 adds project window identity and real-tmux
//! concurrency regressions in the live module. Decisions are on those stories;
//! the current contract is docs/spec/verifier-windows.md.
//!
//! # Why a purpose-built stub rather than the plugin's fake tmux
//!
//! `plugins/story/tests/fakes/tmux` models `story.sh`'s own call shapes —
//! `\;`-chained `new-window` batches with `-P -F '#{pane_id}'` capture and
//! `-e NAME=value` markers. `verify-window.sh`'s calls are a different,
//! narrower shape (plain `respawn-pane`/`has-session`/`new-session`, no
//! chaining, no pane-id capture), so a fake tuned for the other caller
//! would be validating compatibility with a shape this file doesn't use,
//! not the property this file actually needs proven: exact argv content.
//! The stub here (`stub_tmux`) is deliberately minimal, in the same idiom
//! `src/service/engine.rs`'s own `dispatcher_with_tmux` test seam uses for
//! its tmux stub.
//!
//! # Mutation checks (measured by hand before merge)
//!
//! - Reverting `verifier_window_banner`'s positional-parameter passing to
//!   naive string interpolation (`bash -c "printf %s '$text'"`) makes
//!   `a_banner_with_shell_metacharacters_reaches_tmux_as_one_untouched_argv_element`
//!   red: the embedded backtick/semicolon/apostrophe corrupt the shell
//!   string instead of surviving as inert text in one argv slot.
//! - Deleting the `verifier_window_enabled` gate makes
//!   `the_kill_switch_disables_every_tmux_call` red: the stub tmux log
//!   file gets created even with the switch off.
//! - Deleting the `command -v tmux` guard makes
//!   `a_missing_tmux_fails_without_hanging_or_crashing` behave identically
//!   in this environment (a real `tmux` is never on this test's `PATH`
//!   fixture) but changes behavior on any machine where one is — the
//!   guard is what makes the failure the SAME regardless of the host.
//! - Changing the session/window target to interpolate the passed text
//!   (instead of the project identity) makes
//!   `the_target_never_changes_across_different_calls` red.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use storyhook_test_support::scratch_dir;

fn checkout() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Writes an executable stub `tmux` to `path`. Every invocation appends one
/// record to `log`: each argv element on its own line (so an embedded
/// space or newline in an argument is preserved rather than merged with a
/// neighbor — `"$@"` through `printf '%s\n'` expands to one line per
/// element, never joined), followed by a `---` separator. `has_session_ok`
/// controls only the `has-session` subcommand's exit status; every other
/// subcommand exits 0.
fn stub_tmux(path: &Path, log: &Path, has_session_ok: bool) {
    let has_session_exit = i32::from(!has_session_ok);
    std::fs::write(
        path,
        format!(
            "#!/bin/sh\n\
             printf '%s\\n' \"$@\" >> {log}\n\
             printf -- '---\\n' >> {log}\n\
             if [ \"$1\" = has-session ]; then exit {has_session_exit}; fi\n\
             exit 0\n",
            log = shell_quote(&log.display().to_string()),
            has_session_exit = has_session_exit,
        ),
    )
    .expect("fixture: writing stub tmux");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .expect("fixture: making stub tmux executable");
}

/// Single-quotes `s` for embedding in the stub's own `#!/bin/sh` body —
/// the fixture's own paths are controlled (under `scratch_dir()`), but
/// quoting them correctly here is what makes a future scratch-dir naming
/// scheme with a space in it (this project's own SH-493 precedent) safe
/// without anyone having to remember why.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Runs `scripts/verify-window.sh <args...>` with `tmux_dir` prepended to
/// `PATH` and the fixture's stable home exported as `HOME` — nothing else on
/// `PATH` is disturbed, so `bash`, `printf`, etc. resolve normally.
fn run_window(tmux_dir: &Path, args: &[&str]) -> Output {
    let existing = std::env::var_os("PATH").unwrap_or_default();
    let mut path = std::ffi::OsString::from(tmux_dir.as_os_str());
    path.push(":");
    path.push(&existing);
    let home = tmux_dir
        .parent()
        .expect("fixture tmux directory must have a parent")
        .join("stable home");
    Command::new("bash")
        .arg(checkout().join("scripts/verify-window.sh"))
        .args(args)
        .current_dir(tmux_dir.parent().unwrap())
        .env("HOME", home)
        .env("PATH", path)
        .env_remove("STORYHOOK_VERIFIER_MIRROR")
        .env_remove("STORYHOOK_ACTIVITY_LOG_DIR")
        .output()
        .expect("running scripts/verify-window.sh")
}

struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new(has_session_ok: bool) -> Self {
        let root = scratch_dir();
        let git = Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(root.path())
            .output()
            .unwrap();
        assert!(git.status.success(), "{git:?}");
        let tmux_dir = root.path().join("bin");
        std::fs::create_dir(&tmux_dir).unwrap();
        std::fs::create_dir(root.path().join("stable home")).unwrap();
        stub_tmux(
            &tmux_dir.join("tmux"),
            &root.path().join("calls.log"),
            has_session_ok,
        );
        Self { root }
    }

    fn tmux_dir(&self) -> PathBuf {
        self.root.path().join("bin")
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("stable home")
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.root.path().join("calls.log")).unwrap_or_default()
    }

    fn calls_exist(&self) -> bool {
        self.root.path().join("calls.log").exists()
    }
}

fn tmux_calls(log: &str) -> Vec<Vec<&str>> {
    let mut calls = Vec::new();
    let mut call = Vec::new();
    for line in log.lines() {
        if line == "---" {
            calls.push(std::mem::take(&mut call));
        } else {
            call.push(line);
        }
    }
    calls
}

fn assert_stable_home_cwd(call: &[&str], home: &Path, log: &str) {
    let expected = home.display().to_string();
    assert!(
        call.windows(2)
            .any(|pair| pair[0] == "-c" && pair[1] == expected),
        "{} must receive `-c` and stable HOME as distinct argv elements; calls:\n{log}",
        call.first().copied().unwrap_or("tmux invocation")
    );
}

#[test]
fn a_banner_with_shell_metacharacters_reaches_tmux_as_one_untouched_argv_element() {
    let fixture = Fixture::new(true);
    let text = "it's a test; `echo pwned` — and a $(dangerous) substitution";

    let out = run_window(&fixture.tmux_dir(), &["banner", text]);
    assert!(
        out.status.success(),
        "banner should succeed against a healthy stub tmux\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let calls = fixture.calls();
    // The text must appear verbatim, on its own line -- proving it arrived
    // as a single argv element (tmux's own `$1` inside `bash -c`), not
    // concatenated into a shell string where the embedded backtick,
    // semicolon or `$(...)` would either corrupt the command or execute.
    assert!(
        calls.lines().any(|line| line == text),
        "the banner text must appear as its own untouched argv line; calls:\n{calls}"
    );
    // The shell-command word handed to `bash -c` must be the fixed
    // literal with no interpolation of the caller's text into it.
    assert!(
        calls.contains("printf \"%s\\n\" \"$1\"; exec sleep 2147483647"),
        "the bash -c script must be the fixed literal; calls:\n{calls}"
    );
}

#[test]
fn a_log_path_with_a_space_reaches_tail_as_one_untouched_argv_element() {
    let fixture = Fixture::new(true);
    let log_dir = fixture.root.path().join("verification logs");
    std::fs::create_dir(&log_dir).unwrap();
    let log_path = log_dir.join("pr 42.log");
    let log_path_str = log_path.display().to_string();

    let out = run_window(&fixture.tmux_dir(), &["tail", &log_path_str]);
    assert!(out.status.success(), "tail should succeed: {out:?}");

    let calls = fixture.calls();
    assert!(
        calls.lines().any(|line| line == log_path_str),
        "the spaced log path must appear as its own untouched argv line; calls:\n{calls}"
    );
    // `tail` must be invoked directly (multi-argv respawn-pane), not
    // wrapped in a shell string that would need the space escaped.
    assert!(
        calls.lines().any(|line| line == "tail"),
        "tail must be its own argv element, not embedded in a shell string; calls:\n{calls}"
    );
}

#[test]
fn the_kill_switch_disables_every_tmux_call() {
    let fixture = Fixture::new(true);

    let out = Command::new("bash")
        .arg(checkout().join("scripts/verify-window.sh"))
        .args(["banner", "should never reach tmux"])
        .env("PATH", {
            let existing = std::env::var_os("PATH").unwrap_or_default();
            let mut path = std::ffi::OsString::from(fixture.tmux_dir().as_os_str());
            path.push(":");
            path.push(&existing);
            path
        })
        .env("STORYHOOK_VERIFIER_MIRROR", "0")
        .output()
        .expect("running scripts/verify-window.sh");

    assert!(
        !out.status.success(),
        "a disabled mirror must report failure to its caller (for the caller to ignore)"
    );
    assert!(
        !fixture.calls_exist(),
        "the kill switch must stop the mirror before any tmux subprocess is even started"
    );
}

#[test]
fn a_missing_tmux_fails_without_hanging_or_crashing() {
    // PATH deliberately excludes any directory that could contain a real
    // tmux. `/bin` resolves `bash` itself (needed to spawn the child at
    // all -- `Command::new("bash")` looks the program up via the CHILD's
    // own PATH) and every builtin verify-window.sh's missing-tmux path
    // uses (`command`, `printf`, `[`), but on this OS never ships tmux,
    // which lives at `/opt/homebrew/bin/tmux` or `/usr/local/bin/tmux`.
    // Asserted rather than assumed, so a future base-system change fails
    // this test loudly instead of silently testing the wrong thing.
    assert!(
        !Path::new("/bin/tmux").exists(),
        "this test's PATH exclusion assumes /bin never ships tmux on this platform"
    );
    let out = Command::new("bash")
        .arg(checkout().join("scripts/verify-window.sh"))
        .args(["banner", "no tmux available"])
        .env("PATH", "/bin")
        .output()
        .expect("running scripts/verify-window.sh");

    assert!(
        !out.status.success(),
        "a missing tmux must be reported as a failure, not silently succeed"
    );
    assert!(
        out.stdout.is_empty(),
        "a missing tmux must produce no stdout a caller could misparse"
    );
}

#[test]
fn the_target_never_changes_across_different_calls() {
    let fixture = Fixture::new(true);

    run_window(
        &fixture.tmux_dir(),
        &["banner", "SH-999 — a story-shaped banner"],
    );
    run_window(&fixture.tmux_dir(), &["tail", "/some/other/pr-7-tree.log"]);
    run_window(
        &fixture.tmux_dir(),
        &["banner", "PR #123 — a totally different candidate"],
    );

    let calls = fixture.calls();
    let calls = tmux_calls(&calls);
    let targets: Vec<_> = calls
        .iter()
        .filter(|call| call.first() == Some(&"respawn-pane"))
        .map(|call| call.windows(2).find(|arg| arg[0] == "-t").unwrap()[1])
        .collect();
    assert_eq!(targets.len(), 3);
    assert!(targets[0].starts_with("=storyhook-verifier:=verification-"));
    assert!(targets.iter().all(|target| *target == targets[0]));
    assert!(!targets[0].contains("SH-999"));
}

#[test]
fn ensure_creates_a_session_only_when_none_exists() {
    let fresh = Fixture::new(false);
    run_window(&fresh.tmux_dir(), &["banner", "first ever call"]);
    let calls = fresh.calls();
    assert!(
        calls.contains("new-session"),
        "a fresh environment (has-session failing) must create the session; calls:\n{calls}"
    );

    let existing = Fixture::new(true);
    run_window(&existing.tmux_dir(), &["banner", "session already exists"]);
    let calls = existing.calls();
    assert!(
        !calls.contains("new-session"),
        "an existing session (has-session succeeding) must never be recreated; calls:\n{calls}"
    );
}

#[test]
fn journal_view_passes_binary_and_store_as_literal_arguments() {
    let fixture = Fixture::new(true);
    let binary = "/path with spaces/story";
    let store_path = fixture.root.path().join("store's $(inert).db");
    std::fs::write(&store_path, "").unwrap();
    let store = store_path.to_str().unwrap();
    let output = run_window(&fixture.tmux_dir(), &["logs", binary, store]);
    assert!(output.status.success(), "{output:?}");
    let log = fixture.calls();
    let calls = tmux_calls(&log);
    let respawn = calls
        .iter()
        .find(|call| call.first() == Some(&"respawn-pane"))
        .unwrap();
    assert!(respawn.ends_with(&[binary, "--store-path", store, "daemon", "logs", "--follow"]));
    assert_stable_home_cwd(respawn, &fixture.home(), &log);
}

#[test]
#[cfg(target_os = "macos")]
fn journal_identity_works_with_system_python_and_rejects_unresolved_paths() {
    use sha2::{Digest, Sha256};
    use std::os::unix::{ffi::OsStrExt, fs::symlink};

    let fixture = Fixture::new(true);
    symlink("/usr/bin/python3", fixture.tmux_dir().join("python3")).unwrap();
    let store = fixture.root.path().join("store.db");
    std::fs::write(&store, "").unwrap();
    let alias = fixture.root.path().join("alias.db");
    symlink(&store, &alias).unwrap();
    let canonical = std::fs::canonicalize(&store).unwrap();
    let digest = format!("{:x}", Sha256::digest(canonical.as_os_str().as_bytes()));
    for path in [&store, &alias] {
        let output = run_window(
            &fixture.tmux_dir(),
            &["logs", "/literal/story", path.to_str().unwrap()],
        );
        assert!(output.status.success(), "{output:?}");
    }
    let log = fixture.calls();
    let calls = tmux_calls(&log);
    let targets: Vec<_> = calls
        .iter()
        .filter(|call| call.first() == Some(&"respawn-pane"))
        .map(|call| call.windows(2).find(|pair| pair[0] == "-t").unwrap()[1])
        .collect();
    assert_eq!(targets.len(), 2);
    assert_eq!(targets[0], targets[1]);
    assert!(targets[0].ends_with(&digest));

    let missing = fixture.root.path().join("missing.db");
    let dangling = fixture.root.path().join("dangling.db");
    symlink(&missing, &dangling).unwrap();
    let cycle = fixture.root.path().join("cycle.db");
    symlink(&cycle, &cycle).unwrap();
    for path in [&missing, &dangling, &cycle] {
        let output = run_window(
            &fixture.tmux_dir(),
            &["logs", "/literal/story", path.to_str().unwrap()],
        );
        assert!(!output.status.success(), "{output:?}");
        assert!(
            !output.stderr.is_empty(),
            "path failure must explain its cause"
        );
        assert_eq!(fixture.calls(), log, "unresolved paths must not reach tmux");
    }
}

#[test]
fn daemon_phases_have_a_project_view_and_still_emit_journal_banners() {
    let fixture = Fixture::new(true);
    let mut path = fixture.tmux_dir().into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());
    let output = Command::new("bash")
        .arg(checkout().join("scripts/verify-window.sh"))
        .args(["banner", "landing SH-590"])
        .current_dir(fixture.root.path())
        .env("PATH", path)
        .env("STORYHOOK_VERIFIER_MIRROR", "1")
        .env("STORYHOOK_ACTIVITY_LOG_DIR", fixture.root.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("landing SH-590"));
    let log = fixture.calls();
    let calls = tmux_calls(&log);
    let respawn = calls
        .iter()
        .find(|call| call.first() == Some(&"respawn-pane"))
        .expect("daemon phases must now have their own project view");
    assert!(
        respawn
            .iter()
            .any(|arg| arg.starts_with("=storyhook-verifier:=verification-"))
    );
}

#[test]
fn a_new_session_and_its_banner_respawn_use_the_stable_home_cwd() {
    let fixture = Fixture::new(false);
    let out = run_window(&fixture.tmux_dir(), &["banner", "first ever call"]);
    assert!(out.status.success(), "banner should succeed: {out:?}");

    let log = fixture.calls();
    let calls = tmux_calls(&log);
    assert!(
        calls
            .iter()
            .any(|call| call.first() == Some(&"new-session")),
        "a missing session must exercise new-session; calls:\n{log}"
    );
    assert!(
        calls
            .iter()
            .any(|call| call.first() == Some(&"respawn-pane")),
        "the banner must exercise respawn-pane; calls:\n{log}"
    );
    for call in calls.iter().filter(|call| {
        matches!(
            call.first(),
            Some(&"new-session" | &"new-window" | &"respawn-pane")
        )
    }) {
        assert_stable_home_cwd(call, &fixture.home(), &log);
    }
}

#[test]
fn existing_session_banner_and_tail_respawns_use_the_stable_home_cwd() {
    let fixture = Fixture::new(true);
    let banner = run_window(&fixture.tmux_dir(), &["banner", "existing session"]);
    let tail = run_window(&fixture.tmux_dir(), &["tail", "/tmp/verification.log"]);
    assert!(banner.status.success(), "banner should succeed: {banner:?}");
    assert!(tail.status.success(), "tail should succeed: {tail:?}");

    let log = fixture.calls();
    let calls = tmux_calls(&log);
    let respawns: Vec<_> = calls
        .iter()
        .filter(|call| call.first() == Some(&"respawn-pane"))
        .collect();
    assert_eq!(
        respawns.len(),
        2,
        "banner and tail must each exercise respawn-pane; calls:\n{log}"
    );
    assert!(
        respawns.iter().any(|call| call.contains(&"bash")),
        "the banner respawn must be present; calls:\n{log}"
    );
    assert!(
        respawns.iter().any(|call| call.contains(&"tail")),
        "the tail respawn must be present; calls:\n{log}"
    );
    assert!(
        calls.iter().any(|call| call.first() == Some(&"new-window")),
        "the existing-session fixture must exercise new-window; calls:\n{log}"
    );
    for call in calls.iter().filter(|call| {
        matches!(
            call.first(),
            Some(&"new-session" | &"new-window" | &"respawn-pane")
        )
    }) {
        assert_stable_home_cwd(call, &fixture.home(), &log);
    }
}

#[path = "support/verify_window_live.rs"]
mod live;

#[test]
fn standalone_calls_do_not_inherit_a_dispatch_socket() {
    let fixture = Fixture::new(true);
    let tmux_dir = fixture.tmux_dir();
    std::fs::write(
        tmux_dir.join("tmux"),
        "#!/bin/sh\n[ -z \"${TMUX:-}\" ] && [ -z \"${TMUX_PANE:-}\" ] || exit 23\nexit 0\n",
    )
    .unwrap();
    let mut path = tmux_dir.into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());
    let output = Command::new("bash")
        .arg(checkout().join("scripts/verify-window.sh"))
        .args(["banner", "default server only"])
        .current_dir(fixture.root.path())
        .env("PATH", path)
        .env("HOME", fixture.home())
        .env("TMUX", "/dispatch/socket,123,0")
        .env("TMUX_PANE", "%99")
        .env("STORYHOOK_VERIFIER_MIRROR", "1")
        .env_remove("STORYHOOK_ACTIVITY_LOG_DIR")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "mirror must use the default server: {output:?}"
    );
}
