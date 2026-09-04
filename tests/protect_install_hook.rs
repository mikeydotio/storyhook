//! `hooks/protect-install.sh` refuses edits to storyhook's own installed copies
//! (SH-530).
//!
//! Behavioural throughout: every case runs the **tracked** hook, reached by an
//! absolute path rather than copied, and feeds it the payload shape the harness
//! actually sends on stdin. A test against a reimplementation of the hook's
//! logic would validate the reimplementation.
//!
//! # The case that matters most is the negative one
//!
//! A guard that denies edits to the installed plugin is worth nothing if it
//! also denies edits to the checkout, because the checkout is where the work is
//! *supposed* to happen — that is the whole redirect the refusal offers. So
//! `an_edit_in_a_checkout_is_untouched` is the load-bearing case here, not the
//! deny cases: a hook that denied everything would pass every other test in
//! this file.
//!
//! # What this cannot fence
//!
//! That the hook is fast enough. A Claude Code `PreToolUse` hook fails OPEN at
//! its timeout (SH-306), so a slow hook is a silently absent one — and no
//! assertion here can prove a bound that holds on a loaded machine. What the
//! design does instead is make the inert path pay no interpreter start, and
//! `the_inert_path_starts_no_interpreter` pins that structurally by proving the
//! common case answers correctly with `python3` removed from `$PATH` entirely.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use storyhook_test_support::scratch_dir;
use tempfile::TempDir;

/// The tracked hook, never a copy.
fn hook() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("plugins/story/hooks/protect-install.sh")
}

/// A data home carrying a manifest that names two managed prefixes.
fn managed_home() -> (TempDir, PathBuf, PathBuf) {
    let dir = scratch_dir();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).expect("creating the data home");
    let claude = dir.path().join("provider/.claude/plugins/cache/storyhook");
    let codex = dir.path().join("provider/.codex/storyhook");
    std::fs::write(
        data.join("managed-paths"),
        format!(
            "# written by `story plugin install`\n{}\n{}\n",
            claude.display(),
            codex.display()
        ),
    )
    .expect("writing the manifest");
    (dir, data, claude)
}

/// Runs the hook with `payload` on stdin and returns its stdout.
fn ask(data_home: &std::path::Path, payload: &str) -> String {
    ask_with_path(data_home, payload, None)
}

/// [`ask`], optionally PREPENDING to `$PATH` — used to prove the inert path
/// starts no interpreter, by putting a tattling `python3` ahead of the real one.
fn ask_with_path(data_home: &std::path::Path, payload: &str, path: Option<&str>) -> String {
    let mut command = Command::new("bash");
    command
        .arg(hook())
        .env("STORYHOOK_DATA_DIR", data_home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(prefix) = path {
        let existing = std::env::var("PATH").unwrap_or_default();
        command.env("PATH", format!("{prefix}:{existing}"));
    }
    let mut child = command.spawn().expect("spawning the hook");
    use std::io::Write;
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("writing the payload");
    let out = child.wait_with_output().expect("running the hook");
    assert!(
        out.status.success(),
        "the hook must always exit 0 — a nonzero exit is not how a decision is \
         expressed, and the harness would read it as a broken hook: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The refusal reason, or `None` when the hook stayed inert.
fn refusal(stdout: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(stdout).ok()?;
    let output = value.get("hookSpecificOutput")?;
    if output.get("permissionDecision")?.as_str()? != "deny" {
        return None;
    }
    Some(
        output
            .get("permissionDecisionReason")?
            .as_str()?
            .to_string(),
    )
}

#[test]
fn an_edit_to_an_installed_file_is_refused_and_names_the_checkout() {
    let (_dir, data, claude) = managed_home();
    let target = claude.join("story/2.2.0/bin/story.sh");
    let payload = serde_json::json!({
        "tool_name": "Edit",
        "tool_input": { "file_path": target },
    })
    .to_string();

    let reason = refusal(&ask(&data, &payload)).expect("an installed file must be refused");

    assert!(
        reason.contains("CHECKOUT"),
        "the refusal must redirect, not merely refuse: {reason}"
    );
    assert!(
        reason.contains("is lost by this refusal"),
        "the refusal must say the work survives — it is aimed at the wrong file, \
         not wrong in itself: {reason}"
    );
    assert!(
        reason.contains("ALLOW_INSTALLED_EDITS"),
        "the refusal must name its own escape hatch: {reason}"
    );
}

#[test]
fn an_edit_in_a_checkout_is_untouched() {
    let (_dir, data, _claude) = managed_home();
    let payload = serde_json::json!({
        "tool_name": "Edit",
        "tool_input": { "file_path": "/Volumes/Code/mikeyward/storyhook/plugins/story/bin/story.sh" },
    })
    .to_string();

    assert_eq!(
        ask(&data, &payload),
        "{}",
        "editing the checkout is the behaviour this hook exists to redirect \
         people TOWARDS; refusing it would make the guard worse than useless"
    );
}

#[test]
fn the_shell_cannot_walk_around_the_structured_editors() {
    let (_dir, data, claude) = managed_home();
    let target = claude.join("story/2.2.0/bin/story.sh");
    // `sed -i`, `tee`, `cp`, `install` and `rm` all reach the same file. A hook
    // matched only on Write/Edit would be bypassed by the shell it left open.
    for command in [
        format!("sed -i '' s/a/b/ {}", target.display()),
        format!("cp /tmp/x {}", target.display()),
        format!("rm -f {}", target.display()),
        format!("echo hi > {}", target.display()),
        format!(
            "sed -n 1p {} && rm -f {}",
            target.display(),
            target.display()
        ),
        format!("mystery-reader {}", target.display()),
        format!("rg --pre 'rm -f /tmp/unrelated' x {}", target.display()),
    ] {
        let payload = serde_json::json!({
            "tool_name": "Bash",
            "tool_input": { "command": command },
        })
        .to_string();
        assert!(
            refusal(&ask(&data, &payload)).is_some(),
            "a shell command reaching an installed path must be refused: {command}"
        );
    }
}

#[test]
fn read_only_shell_inspection_of_an_installed_file_is_untouched() {
    let (_dir, data, claude) = managed_home();
    let target = claude.join("story/2.2.0/skills/story/SKILL.md");
    for command in [
        format!("sed -n '1,240p' {}", target.display()),
        format!("rg -n '^' {}", target.display()),
        format!("cat {}", target.display()),
        format!("head -20 {}", target.display()),
        format!("tail -20 {}", target.display()),
        format!("grep -n Storyhook {}", target.display()),
        format!("cat {} | head -20", target.display()),
        format!(
            "sed -n '1,20p' {} && story show SH-550 --json",
            target.display()
        ),
    ] {
        let payload = serde_json::json!({
            "tool_name": "Bash",
            "tool_input": { "command": command },
        })
        .to_string();
        assert_eq!(
            ask(&data, &payload),
            "{}",
            "a command proven to only read the installed artifact must be allowed: {command}"
        );
    }
}

#[test]
fn an_ordinary_shell_command_is_untouched() {
    let (_dir, data, _claude) = managed_home();
    let payload = serde_json::json!({
        "tool_name": "Bash",
        "tool_input": { "command": "cargo test --quiet" },
    })
    .to_string();
    assert_eq!(ask(&data, &payload), "{}");
}

#[test]
fn the_escape_hatch_is_a_deliberate_file_and_it_works() {
    let (_dir, data, claude) = managed_home();
    let target = claude.join("story/2.2.0/bin/story.sh");
    let payload = serde_json::json!({
        "tool_name": "Edit",
        "tool_input": { "file_path": target },
    })
    .to_string();
    assert!(
        refusal(&ask(&data, &payload)).is_some(),
        "refused by default"
    );

    std::fs::write(data.join("ALLOW_INSTALLED_EDITS"), "").expect("creating the override");

    assert_eq!(
        ask(&data, &payload),
        "{}",
        "an operator who has deliberately created the override must not be \
         refused; a file rather than an environment variable so the decision \
         leaves a trace (SH-411)"
    );
}

#[test]
fn no_manifest_means_nothing_is_installed_to_protect() {
    let dir = scratch_dir();
    let payload = serde_json::json!({
        "tool_name": "Edit",
        "tool_input": { "file_path": "/anywhere/at/all" },
    })
    .to_string();
    assert_eq!(
        ask(dir.path(), &payload),
        "{}",
        "a machine where `story plugin install` never ran has no installed copy \
         to drift, so the hook has nothing to say"
    );
}

#[test]
fn the_inert_path_starts_no_interpreter() {
    let (dir, data, claude) = managed_home();

    // A `python3` that tattles, placed ahead of the real one. Asserting on
    // timing would be the obvious test and the wrong one — it would be a bare
    // wall-clock literal (SH-394) measuring this machine's load rather than the
    // property. Whether an interpreter was STARTED is the actual claim, and it
    // is exactly observable.
    let shim_dir = dir.path().join("shim");
    std::fs::create_dir_all(&shim_dir).expect("creating the shim directory");
    let marker = dir.path().join("python-was-started");
    let shim = shim_dir.join("python3");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\ntouch {}\nexec /usr/bin/env -i /usr/bin/python3 \"$@\"\n",
            marker.display()
        ),
    )
    .expect("writing the shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))
            .expect("making the shim executable");
    }

    let inert = serde_json::json!({
        "tool_name": "Bash",
        "tool_input": { "command": "ls -la" },
    })
    .to_string();
    assert_eq!(
        ask_with_path(&data, &inert, Some(&shim_dir.display().to_string())),
        "{}",
        "the inert path must still answer correctly"
    );
    assert!(
        !marker.exists(),
        "the inert path must reach its answer in shell alone: a PreToolUse hook          that times out fails OPEN and silently (SH-306), so the common case          cannot afford an interpreter start"
    );

    // The control: the interpreter IS started when there is a real decision to
    // make, so the assertion above is measuring the hook rather than a shim
    // that never works.
    let live = serde_json::json!({
        "tool_name": "Edit",
        "tool_input": { "file_path": claude.join("story/2.2.0/bin/story.sh") },
    })
    .to_string();
    let _ = ask_with_path(&data, &live, Some(&shim_dir.display().to_string()));
    assert!(
        marker.exists(),
        "control: a payload that names a managed path must reach the parser"
    );
}
