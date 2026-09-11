//! SH-590: the real daemon's activity journal, independent of its tmux view.

use serde_json::Value;
use storyhook_test_support::TestEnv;

#[test]
fn logs_are_readable_without_starting_a_daemon() {
    let env = TestEnv::isolated();
    env.story(env.home())
        .args(["daemon", "logs", "--json"])
        .assert()
        .success();
    assert!(
        env.daemon().is_none(),
        "reading logs must never start a daemon"
    );
}

#[test]
fn daemon_records_committed_lifecycle_and_successful_hook_streams() {
    let env = TestEnv::isolated();
    let project = env.project().prefix("LOG").build();
    let pointer = project.path().join(".storyhook.toml");
    let before = std::fs::read_to_string(&pointer).unwrap();
    std::fs::write(
        &pointer,
        format!(
            "{before}\n[hooks.on_create]\ncommand = \"printf hook-out; printf hook-err >&2\"\n"
        ),
    )
    .unwrap();
    let id = project.new_story("private title must not appear in activity metadata");
    project.run(&["move", &id, "in-progress"]).success();
    project
        .run(&["new", "must roll back", "--type", "nonexistent"])
        .failure();
    env.stop_daemon();
    let out = env
        .story(project.path())
        .args(["daemon", "logs", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let raw = String::from_utf8(out).unwrap();
    let records: Vec<Value> = raw
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    for needle in [
        "daemon started",
        "daemon stopped",
        "StoryCreated",
        "StoryStateChanged",
        "hook-out",
        "hook-err",
    ] {
        assert!(
            records
                .iter()
                .any(|row| row["message"].as_str().unwrap().contains(needle)),
            "missing {needle}: {raw}"
        );
    }
    assert!(records.iter().any(|row| row["source"] == "hook:create"
        && row["stream"] == "stdout"
        && row["message"] == "hook-out"));
    assert!(records.iter().any(|row| row["source"] == "hook:create"
        && row["stream"] == "stderr"
        && row["message"] == "hook-err"));
    assert!(!raw.contains("private title"));
    assert_eq!(
        records
            .iter()
            .filter(|row| row["message"].as_str().unwrap().starts_with("StoryCreated"))
            .count(),
        1,
        "a refused write must not publish a committed event"
    );
    assert!(!raw.contains('\u{1b}'));
    assert!(env.daemon().is_none());
}

#[test]
fn a_script_and_the_daemon_reader_share_the_same_format_and_store_destination() {
    use storyhook_test_support::{ChildGuard, STORY_COMMAND_DEADLINE};
    let env = TestEnv::isolated();
    let other = TestEnv::isolated();
    let mut command = std::process::Command::new("python3");
    command
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/activity-run.py"
        ))
        .args(["source.sh", "--", "sh", "-c", "printf shared-format"])
        .env(
            "STORYHOOK_ACTIVITY_LOG_DIR",
            env.environment().daemon_state_dir().join("activity"),
        );
    let mut child = ChildGuard::spawn_with_output(&mut command).unwrap();
    let out = child.wait_with_output_within(STORY_COMMAND_DEADLINE, || {
        "script log writer did not complete".into()
    });
    assert!(out.status.success());
    let rows = env
        .story(env.home())
        .args(["daemon", "logs", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let rows = String::from_utf8(rows).unwrap();
    assert!(
        rows.lines()
            .map(|row| serde_json::from_str::<Value>(row).unwrap())
            .any(|row| row["source"] == "source.sh" && row["message"] == "shared-format")
    );
    other
        .story(other.home())
        .args(["daemon", "logs", "--json"])
        .assert()
        .success()
        .stdout("");
    assert!(env.daemon().is_none());
    assert!(other.daemon().is_none());
}

#[test]
fn an_unwritable_activity_destination_does_not_prevent_story_work() {
    let env = TestEnv::isolated();
    let state = env.environment().daemon_state_dir();
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(state.join("activity"), "blocking fixture").unwrap();
    let project = env.project().prefix("LOG").build();
    project.new_story("work still succeeds");
    env.stop_daemon();
    let diagnostics = std::fs::read_to_string(env.environment().daemon_log()).unwrap();
    assert_eq!(
        diagnostics.matches("activity journal unavailable").count(),
        1,
        "failure reporting must not feed itself: {diagnostics}"
    );
}

#[test]
fn daemon_start_opens_the_continuous_view_with_its_own_binary_and_store() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};
    let env = TestEnv::isolated();
    let bin = env.home().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let calls = env.home().join("tmux-calls");
    let stub = bin.join("tmux");
    std::fs::write(
        &stub,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$ACTIVITY_TEST_TMUX_CALLS\"\nexit 0\n",
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    // launchd can resolve Apple's Python even when the interactive shell uses
    // Homebrew. Exercise that interpreter instead of inheriting the shell's.
    #[cfg(target_os = "macos")]
    std::os::unix::fs::symlink("/usr/bin/python3", bin.join("python3")).unwrap();
    let mut path = std::ffi::OsString::from(bin);
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());
    env.story(env.home())
        .args(["daemon", "start"])
        .env("PATH", path)
        .env("STORYHOOK_VERIFIER_MIRROR", "1")
        .env("ACTIVITY_TEST_TMUX_CALLS", &calls)
        .assert()
        .success();
    let deadline = Instant::now() + storyhook_test_support::STORY_COMMAND_DEADLINE;
    let text = loop {
        let text = std::fs::read_to_string(&calls).unwrap_or_default();
        if text.contains("--follow\n") {
            break text;
        }
        assert!(
            env.daemon_is_live(),
            "daemon exited before opening the view"
        );
        let journal: String =
            std::fs::read_dir(env.environment().daemon_state_dir().join("activity"))
                .unwrap()
                .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
                .collect();
        assert!(
            !journal.contains("tmux activity view unavailable"),
            "activity helper failed: {journal}"
        );
        assert!(
            Instant::now() < deadline,
            "view never opened: {text}\nactivity journal: {journal}"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(text.contains("=storyhook-verifier:=activity-"));
    assert!(text.contains(&format!(
        "{}\n--store-path\n{}\ndaemon\nlogs\n--follow\n",
        storyhook_test_support::story_binary().display(),
        env.environment().store_path().display()
    )));
    env.stop_daemon();
}
