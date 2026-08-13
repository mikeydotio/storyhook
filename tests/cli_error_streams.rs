//! Which stream the CLI writes to, and why it matters.
//!
//! Diagnostics belong on stderr and results on stdout, because that is what
//! every caller assumes: `story move ... --quiet 2>/dev/null || true` (the
//! shape storyhook's own post-merge hook uses) can only suppress an error
//! that is actually on stderr. An error on stdout does not merely evade the
//! guard — it lands in the middle of whatever the caller was printing.
//!
//! `--json` is the exception, and deliberately so: there, stdout is a
//! machine-readable result channel carrying exactly one self-describing
//! document per run (`{"result":"ok"|"conflict"|"error", ...}`), and the
//! error envelope IS the result the caller asked for. Splitting it onto
//! stderr would force every consumer to read two streams and merge them, on
//! a stream that also carries free-text warnings. See `move_if_state.rs`,
//! which reads a conflict envelope off stdout, and
//! plugin/claude-code/bin/story.sh, whose CAS claim depends on it.

// TODO(rearch): migrate to storyhook_test_support::scratch_dir — see clippy.toml.
#![allow(clippy::disallowed_methods)]

use assert_cmd::Command;

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

/// A project with one story, closed, so `move` on it fails the way the
/// post-merge hook hits in practice.
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "A story"])
        .assert()
        .success();
    story(dir.path())
        .args(["move", "SH-1", "done"])
        .assert()
        .success();
    dir
}

#[test]
fn a_failed_command_writes_its_error_only_to_stderr() {
    let dir = project();
    let out = story(dir.path())
        .args(["move", "SH-1", "done", "already closed"])
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(2), "the exit code must survive");
    assert!(
        out.stdout.is_empty(),
        "a failed command must print nothing to stdout, got: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.starts_with("error: "),
        "the error belongs on stderr, got: {stderr}"
    );
}

#[test]
fn quiet_does_not_move_an_error_back_onto_stdout() {
    // The reported symptom: `story move ... --quiet 2>/dev/null || true` in
    // the post-merge hook printed the closed-story refusal (then worded
    // "error: story SH-41 is closed and cannot be modified") into the middle
    // of a merge diffstat.
    let dir = project();
    let out = story(dir.path())
        .args(["move", "SH-1", "done", "already closed", "--quiet"])
        .output()
        .unwrap();

    assert!(!out.status.success());
    assert!(
        out.stdout.is_empty(),
        "--quiet must not put the error back on stdout, got: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(!out.stderr.is_empty(), "the error must still reach stderr");
}

#[test]
fn an_unparseable_invocation_writes_its_usage_error_to_stderr() {
    // The argument-parsing failure path, which reports before the command
    // ever runs.
    let dir = project();
    let out = story(dir.path()).args(["bogus-command"]).output().unwrap();

    assert_eq!(out.status.code(), Some(2));
    assert!(
        out.stdout.is_empty(),
        "a usage error must not reach stdout, got: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unknown command"),
        "the usage error belongs on stderr"
    );
}

#[test]
fn a_successful_command_writes_only_to_stdout() {
    // The other half of the contract: moving errors off stdout must not push
    // anything else onto stderr.
    let dir = project();
    let out = story(dir.path()).args(["list"]).output().unwrap();

    assert!(out.status.success());
    assert!(!out.stdout.is_empty(), "results belong on stdout");
    assert!(
        out.stderr.is_empty(),
        "a successful command must be silent on stderr, got: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn json_mode_keeps_the_error_envelope_on_stdout() {
    // `--json` promises exactly one machine-readable document on stdout,
    // whatever the outcome. Callers parse that stream and branch on
    // `result`; moving the error envelope to stderr would make them read two
    // streams to find one answer.
    let dir = project();
    let out = story(dir.path())
        .args(["move", "SH-1", "done", "already closed", "--json"])
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(2));
    let envelope: serde_json::Value = serde_json::from_slice(&out.stdout)
        .expect("--json must emit one parseable document on stdout, whatever the outcome");
    assert_eq!(envelope["result"], "error");
    assert_eq!(envelope["exit_code"], 2);
    assert!(
        out.stderr.is_empty(),
        "with --json the envelope is the whole diagnostic; stderr must stay empty, got: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
