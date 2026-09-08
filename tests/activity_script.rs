//! File-backed observation executes real subprocesses and preserves their wire output.
use std::{
    io::Write as _,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use storyhook_test_support::{ChildGuard, STORY_COMMAND_DEADLINE, scratch_dir};

fn runner(logs: &std::path::Path, code: &str) -> Command {
    let mut command = Command::new("python3");
    command
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/activity-run.py"
        ))
        .args(["probe.sh", "--", "sh", "-c", code])
        .env("STORYHOOK_ACTIVITY_LOG_DIR", logs);
    command
}

fn test_output_runner(
    logs: &std::path::Path,
    capture: &std::path::Path,
    progress: &std::path::Path,
    code: &str,
) -> Command {
    let mut command = Command::new("python3");
    command
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/activity-run.py"
        ))
        .arg("--capture")
        .arg(capture)
        .args([
            "--test-progress",
            "release gate/rust-suite",
            "probe.sh",
            "--",
            "sh",
            "-c",
            code,
        ])
        .env("STORYHOOK_ACTIVITY_LOG_DIR", logs)
        .env("STORYHOOK_GATE_PROGRESS", progress);
    command
}

fn journal(logs: &std::path::Path) -> Vec<serde_json::Value> {
    let mut rows = Vec::new();
    if let Ok(entries) = std::fs::read_dir(logs) {
        for entry in entries {
            let bytes = std::fs::read_to_string(entry.unwrap().path()).unwrap();
            rows.extend(
                bytes
                    .lines()
                    .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok()),
            );
        }
    }
    rows
}

#[test]
fn both_streams_are_visible_before_exit_and_final_fragments_keep_the_exit_status() {
    let root = scratch_dir();
    let logs = root.path().join("activity");
    let release = root.path().join("release");
    let mut command = runner(
        &logs,
        "printf 'out\n'; printf 'err\n' >&2; while [ ! -f \"$1\" ]; do sleep 0.05; done; printf final; exit 7",
    );
    command.arg("probe").arg(&release);
    let mut child = ChildGuard::spawn_with_output(&mut command).unwrap();
    let deadline = Instant::now() + STORY_COMMAND_DEADLINE;
    loop {
        let rows = journal(&logs);
        if rows
            .iter()
            .any(|r| r["stream"] == "stdout" && r["message"] == "out")
            && rows
                .iter()
                .any(|r| r["stream"] == "stderr" && r["message"] == "err")
        {
            break;
        }
        assert!(
            child.try_wait().is_none(),
            "observer exited before publishing its streams: {rows:?}"
        );
        assert!(
            Instant::now() < deadline,
            "streams did not arrive while command was still running: {rows:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    std::fs::write(release, "go").unwrap();
    let output = child.wait_with_output_within(STORY_COMMAND_DEADLINE, || {
        "activity runner did not finish".into()
    });
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stdout, b"out\nfinal");
    assert_eq!(output.stderr, b"err\n");
    let rows = journal(&logs);
    assert!(rows.iter().any(|r| r["message"] == "final"));
    assert!(rows.iter().all(|r| r["source"] == "probe.sh"));
}

#[test]
fn logging_failure_does_not_change_output_or_status() {
    let root = scratch_dir();
    let blocked = root.path().join("not-a-directory");
    std::fs::write(&blocked, "owned fixture").unwrap();
    let mut child =
        ChildGuard::spawn_with_output(&mut runner(&blocked, "printf unchanged; exit 9")).unwrap();
    let output = child.wait_with_output_within(STORY_COMMAND_DEADLINE, || {
        "activity runner did not finish".into()
    });
    assert_eq!(output.status.code(), Some(9));
    assert_eq!(output.stdout, b"unchanged");
    assert!(String::from_utf8_lossy(&output.stderr).contains("activity journal unavailable"));
}

#[test]
fn a_missing_python_interpreter_preserves_the_unobserved_command() {
    let root = scratch_dir();
    let mut command = Command::new("/bin/bash");
    command.args(["-c", "bash_command() { printf unchanged; return 9; }; . \"$1\"; activity_run probe bash_command", "probe"])
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/activity-log.sh"))
        // Only shell builtins are needed by the fallback. An empty PATH
        // proves the logger does not make Python a new verifier prerequisite.
        .env("PATH", root.path())
        .env("STORYHOOK_ACTIVITY_LOG_DIR", root.path().join("activity"));
    let mut child = ChildGuard::spawn_with_output(&mut command).unwrap();
    let output =
        child.wait_with_output_within(STORY_COMMAND_DEADLINE, || "fallback did not finish".into());
    assert_eq!(output.status.code(), Some(9), "{output:?}");
    assert_eq!(output.stdout, b"unchanged");
    assert!(String::from_utf8_lossy(&output.stderr).contains("python3"));
}

#[test]
fn large_and_non_utf8_output_is_complete_while_journal_text_is_safe() {
    let root = scratch_dir();
    let logs = root.path().join("activity");
    let mut command = runner(
        &logs,
        "exec python3 -c 'import os; os.write(1, b\"x\" * 200000 + b\"\\xff\\n\"); os.write(2, b\"\\x1b[31mghp_testsecret\\n\")'",
    );
    let mut child = ChildGuard::spawn_with_output(&mut command).unwrap();
    let output =
        child.wait_with_output_within(STORY_COMMAND_DEADLINE, || "large output was blocked".into());
    assert!(output.status.success());
    assert_eq!(output.stdout.len(), 200002);
    assert_eq!(&output.stdout[200000..], b"\xff\n");
    assert_eq!(output.stderr, b"\x1b[31mghp_testsecret\n");
    let rows = journal(&logs);
    let joined: String = rows
        .iter()
        .filter(|r| r["stream"] == "stdout")
        .map(|r| r["message"].as_str().unwrap())
        .collect();
    assert_eq!(joined, format!("{}�", "x".repeat(200000)));
    assert!(
        rows.iter()
            .all(|r| !r["message"].as_str().unwrap().contains("ghp_testsecret"))
    );
    assert!(
        rows.iter()
            .all(|r| !r["message"].as_str().unwrap().contains('\u{1b}'))
    );
}

#[test]
fn a_descendant_holding_the_output_file_cannot_delay_completion() {
    let root = scratch_dir();
    let release = root.path().join("release");
    let done = root.path().join("done");
    struct Release(std::path::PathBuf);
    impl Drop for Release {
        fn drop(&mut self) {
            std::fs::write(&self.0, "release").unwrap();
        }
    }
    let guard = Release(release.clone());
    let mut command = runner(
        &root.path().join("activity"),
        "(while [ ! -f \"$1\" ]; do sleep 0.05; done; printf done > \"$2\") & printf parent-finished",
    );
    command.arg("probe").arg(&release).arg(&done);
    let mut child = ChildGuard::spawn_with_output(&mut command).unwrap();
    let deadline = Duration::from_secs(storyhook::event_hooks::HOOK_TIMEOUT_CEILING_SECS);
    let output = child.wait_with_output_within(deadline, || {
        "the observer waited for its child's descendant".into()
    });
    assert!(output.status.success());
    assert_eq!(output.stdout, b"parent-finished");
    assert!(
        !done.exists(),
        "the descendant must still hold its descriptors when the observer returns"
    );
    drop(guard);
    let end = Instant::now() + deadline;
    while !done.exists() {
        assert!(
            Instant::now() < end,
            "descendant did not finish after release"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn test_output_capture_cannot_be_held_open_by_a_descendant() {
    let root = scratch_dir();
    let release = root.path().join("release");
    let done = root.path().join("done");
    let capture = root.path().join("test-output.log");
    let progress = root.path().join("gate-progress.ndjson");
    struct Release(std::path::PathBuf);
    impl Drop for Release {
        fn drop(&mut self) {
            std::fs::write(&self.0, "release").unwrap();
        }
    }
    let guard = Release(release.clone());
    let mut command = test_output_runner(
        &root.path().join("activity"),
        &capture,
        &progress,
        "(while [ ! -f \"$1\" ]; do sleep 0.05; done; printf done > \"$2\") & printf '     Running tests/probe.rs (probe)\\ntest completes ... ok\\n'",
    );
    command.arg("probe").arg(&release).arg(&done);
    let mut child = ChildGuard::spawn_with_output(&mut command).unwrap();
    let deadline = Duration::from_secs(storyhook::event_hooks::HOOK_TIMEOUT_CEILING_SECS);
    let output = child.wait_with_output_within(deadline, || {
        "test-output observer waited for the command's descendant".into()
    });
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        std::fs::read_to_string(&capture).unwrap(),
        "     Running tests/probe.rs (probe)\ntest completes ... ok\n"
    );
    let progress = std::fs::read_to_string(progress).unwrap();
    assert!(progress.contains(r#""kind":"activity""#), "{progress}");
    assert!(progress.contains(r#""status":"running""#), "{progress}");
    assert!(progress.contains(r#""kind":"case""#), "{progress}");
    assert!(
        !done.exists(),
        "the observer must return before the descendant"
    );
    drop(guard);
    let end = Instant::now() + deadline;
    while !done.exists() {
        assert!(
            Instant::now() < end,
            "descendant did not finish after release"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn required_capture_failure_does_not_start_the_command() {
    let root = scratch_dir();
    let marker = root.path().join("command-started");
    let capture = root.path().join("missing-parent/test-output.log");
    let progress = root.path().join("gate-progress.ndjson");
    let mut command = test_output_runner(
        &root.path().join("activity"),
        &capture,
        &progress,
        "printf started > \"$1\"",
    );
    command.arg("probe").arg(&marker);

    let output = command.output().expect("running the observer");
    assert!(!output.status.success(), "{output:?}");
    assert!(
        !marker.exists(),
        "a command ran without its required capture"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("required capture unavailable"),
        "the failure must identify the lost evidence boundary: {output:?}"
    );
}

#[test]
fn shared_test_output_parser_preserves_ledger_identity_and_ignores_chatter() {
    let mut child = Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/test_output.py"
        ))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("starting the shared test-output parser");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            b"   Compiling fixture v0.0.0\nrandom heartbeat\n     Running tests/one.rs (target/one)\ntest same_name ... ok\n     Running tests/two.rs (target/two)\ntest same_name ... FAILED\ntest ignored ... ignored\n",
        )
        .unwrap();

    let output = child.wait_with_output().expect("waiting for the parser");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "one\tsame_name\tPASS\ntwo\tsame_name\tFAIL\n"
    );
}

#[test]
fn importing_the_shared_parser_does_not_dirty_the_checkout() {
    let root = scratch_dir();
    let scripts = root.path().join("scripts");
    std::fs::create_dir(&scripts).unwrap();
    for name in ["activity-run.py", "test_output.py"] {
        std::fs::copy(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("scripts")
                .join(name),
            scripts.join(name),
        )
        .unwrap();
    }

    let output = Command::new("python3")
        .arg(scripts.join("activity-run.py"))
        .args(["probe", "--", "true"])
        .output()
        .expect("running the observer through a fixture path");
    assert!(output.status.success(), "{output:?}");
    assert!(
        !scripts.join("__pycache__").exists(),
        "loading the parser must not leave Python bytecode in its checkout"
    );
}

#[test]
fn group_cancellation_preserves_the_commands_cleanup_and_final_output() {
    use std::os::unix::process::CommandExt;
    let root = scratch_dir();
    let logs = root.path().join("activity");
    let mut command = runner(
        &logs,
        "trap 'printf cleaned; exit 23' TERM; printf 'ready\\n'; while :; do sleep 0.05; done",
    );
    command.process_group(0);
    let mut child = ChildGuard::spawn_with_output(&mut command).unwrap();
    struct Group(i32);
    impl Drop for Group {
        fn drop(&mut self) {
            // This test creates and owns the group; clean descendants on unwind too.
            unsafe {
                libc::kill(-self.0, libc::SIGKILL);
            }
        }
    }
    let _group = Group(child.pid() as i32);
    let deadline = Instant::now() + STORY_COMMAND_DEADLINE;
    while !journal(&logs).iter().any(|r| r["message"] == "ready") {
        assert!(
            child.try_wait().is_none(),
            "observer exited before readiness"
        );
        assert!(Instant::now() < deadline, "command never became ready");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        unsafe { libc::kill(-(child.pid() as i32), libc::SIGTERM) },
        0
    );
    let output = child.wait_with_output_within(STORY_COMMAND_DEADLINE, || {
        "group cancellation did not complete".into()
    });
    assert_eq!(output.status.code(), Some(23), "{output:?}");
    assert_eq!(output.stdout, b"ready\ncleaned");
    assert!(journal(&logs).iter().any(|r| r["message"] == "cleaned"));
}
