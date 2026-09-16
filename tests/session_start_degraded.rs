//! SH-736: production hook/CLI behavior when invocation fails before any RPC.
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use storyhook_test_support::{ChildGuard, STORY_COMMAND_DEADLINE, TestEnv, scratch_dir};

const POINTER: &str = "schema = 1\nuuid = '55c4f44d-885e-4288-b9be-bc0418da0711'\nprefix = 'SH'\n";

fn payload(cwd: &Path) -> serde_json::Value {
    serde_json::json!({"cwd": cwd, "session_id": "claude-startup-736",
        "hook_event_name": "SessionStart", "source": "startup"})
}

fn run(env: &TestEnv, cwd: &Path, input: &serde_json::Value) -> Output {
    let mut cmd = Command::new("bash");
    env.apply(&mut cmd);
    cmd.arg(storyhook_test_support::hook_script("session-start.sh"))
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = ChildGuard::spawn_with_output(&mut cmd).unwrap();
    child
        .stdin()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    child.wait_with_output_within(STORY_COMMAND_DEADLINE, || "SessionStart hook hung".into())
}

fn fail_before_rpc(env: &TestEnv) {
    // A directory in place of the spawn-lock file deterministically refuses
    // daemon admission without starting a daemon or relying on wall time.
    std::fs::create_dir_all(env.environment().daemon_spawn_lock()).unwrap();
}

#[test]
fn early_failure_preserves_cause_and_publishes_only_hook_evidence() {
    let env = TestEnv::isolated();
    let dir = scratch_dir();
    std::fs::write(dir.path().join(".storyhook.toml"), POINTER).unwrap();
    fail_before_rpc(&env);
    let output = run(&env, dir.path(), &payload(dir.path()));
    assert!(output.status.success());
    let answer: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let context = answer["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(context.contains("story load-context"));
    assert!(
        !context.contains("loaded in time"),
        "an immediate I/O failure is not a timeout"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("SessionStart") && stderr.contains("directory"),
        "lost cause: {stderr}"
    );
    let sentinel: serde_json::Value = serde_json::from_slice(
        &std::fs::read(dir.path().join(".claude/dispatch-sentinel.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(sentinel["protocol_version"], 2);
    assert_eq!(sentinel["session_id"], "claude-startup-736");
    assert_eq!(sentinel["context_status"], "unavailable");
    assert_eq!(
        sentinel["plugin_root"],
        storyhook_test_support::hook_script("session-start.sh")
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap()
            .to_str()
            .unwrap()
    );
    assert!(env.daemon().is_none());
}

#[test]
fn fallback_does_not_invent_project_or_session_authority() {
    let env = TestEnv::isolated();
    fail_before_rpc(&env);
    for case in [
        "no-pointer",
        "bad-pointer",
        "disabled",
        "disabled-parent",
        "bad-plugin",
        "bad-legacy",
        "missing-session",
        "wrong-event",
        "bad-cwd",
        "not-object",
    ] {
        let dir = scratch_dir();
        let root = dir.path();
        let pointer = root.join(".storyhook.toml");
        if case != "no-pointer" {
            std::fs::write(
                &pointer,
                if case == "bad-pointer" {
                    "not valid toml ["
                } else {
                    POINTER
                },
            )
            .unwrap();
        }
        if matches!(case, "disabled" | "disabled-parent" | "bad-plugin") {
            std::fs::write(
                &pointer,
                format!(
                    "{POINTER}[plugin]\nenabled = {}\n",
                    if case == "bad-plugin" { "42" } else { "false" }
                ),
            )
            .unwrap();
        }
        if case == "bad-legacy" {
            std::fs::create_dir(root.join(".storyhook")).unwrap();
            std::fs::write(root.join(".storyhook/plugin-config.toml"), "not toml [").unwrap();
        }
        let cwd = if case == "disabled-parent" {
            root.join("subdir")
        } else {
            root.to_path_buf()
        };
        std::fs::create_dir_all(&cwd).unwrap();
        let mut input = payload(&cwd);
        match case {
            "missing-session" => {
                input.as_object_mut().unwrap().remove("session_id");
            }
            "wrong-event" => input["hook_event_name"] = "Stop".into(),
            "bad-cwd" => input["cwd"] = "/does-not-exist/sh-736".into(),
            "not-object" => input = serde_json::json!([]),
            _ => {}
        }
        let output = run(&env, &cwd, &input);
        assert!(output.status.success(), "{case}: {output:?}");
        assert!(
            serde_json::from_slice::<serde_json::Value>(&output.stdout)
                .unwrap()
                .is_object()
        );
        assert!(
            !cwd.join(".claude/dispatch-sentinel.json").exists(),
            "{case} published unauthorized evidence"
        );
    }
}

#[test]
fn fallback_write_failure_preserves_both_diagnostics() {
    let env = TestEnv::isolated();
    let dir = scratch_dir();
    std::fs::write(dir.path().join(".storyhook.toml"), POINTER).unwrap();
    std::fs::write(dir.path().join(".claude"), "file blocks directory").unwrap();
    fail_before_rpc(&env);
    let output = run(&env, dir.path(), &payload(dir.path()));
    assert!(output.status.success());
    assert!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout)
            .unwrap()
            .is_object()
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("dispatch-sentinel.json") && stderr.contains("SessionStart"),
        "{stderr}"
    );
}

#[test]
fn held_spawn_lock_keeps_the_deadline_and_publishes_degraded_evidence() {
    use fs4::FileExt;
    let env = TestEnv::isolated();
    let dir = scratch_dir();
    std::fs::write(dir.path().join(".storyhook.toml"), POINTER).unwrap();
    let path = env.environment().daemon_spawn_lock();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let lock = std::fs::File::create(path).unwrap();
    lock.try_lock_exclusive().unwrap();
    let mut input = payload(dir.path());
    input["storyhook_plugin_root"] = storyhook_test_support::hook_script("session-start.sh")
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_str()
        .unwrap()
        .into();
    let output = env
        .story(dir.path())
        .args(["--deadline", "1", "session-start"])
        .write_stdin(input.to_string())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("deadline"));
    let sentinel: serde_json::Value = serde_json::from_slice(
        &std::fs::read(dir.path().join(".claude/dispatch-sentinel.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(sentinel["context_status"], "unavailable");
    FileExt::unlock(&lock).unwrap();
}

#[test]
fn direct_fallback_rejects_missing_or_nonexistent_package_identity() {
    let dir = scratch_dir();
    std::fs::write(dir.path().join(".storyhook.toml"), POINTER).unwrap();
    for root in [
        serde_json::Value::Null,
        "relative".into(),
        "/not-a-real-plugin/sh-736".into(),
    ] {
        let mut input = payload(dir.path());
        input["storyhook_plugin_root"] = root;
        storyhook::service::session::publish_unavailable(
            dir.path(),
            Some(&input.to_string()),
            "now".into(),
        );
        assert!(!dir.path().join(".claude/dispatch-sentinel.json").exists());
    }
}
