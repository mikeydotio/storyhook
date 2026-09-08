//! File-backed observation executes real subprocesses and preserves their wire output.
use std::{
    process::Command,
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
