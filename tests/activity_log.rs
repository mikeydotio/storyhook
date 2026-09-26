//! SH-590: the real daemon's activity journal, independent of its tmux view.

use serde_json::Value;
use storyhook_test_support::TestEnv;

#[test]
fn project_directory_logs_are_read_without_a_daemon_or_store_journal() {
    let env = TestEnv::isolated();
    let directory = env.home().join("project with spaces/.storyhook/logs");
    std::fs::create_dir_all(&directory).unwrap();
    let row = serde_json::json!({"at":"2026-09-20T00:00:00Z", "level":"INFO",
        "source":"fixture", "stream":"stderr", "pid":1,
        "context":"project=moshtail MT-1000 attempt=one", "message":"fixture output"});
    std::fs::write(
        directory.join(format!("{}.jsonl", chrono::Utc::now().format("%Y-%m-%d"))),
        format!("{row}\n"),
    )
    .unwrap();
    let output = env
        .story(env.home())
        .args(["daemon", "logs", "--directory"])
        .arg(&directory)
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(serde_json::from_slice::<Value>(&output).unwrap(), row);
    assert!(env.daemon().is_none());
    env.story(env.home())
        .args(["daemon", "logs", "--json"])
        .assert()
        .success()
        .stdout("");
}

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
fn daemon_start_does_not_allocate_a_store_activity_window() {
    use std::os::unix::fs::PermissionsExt;
    let env = TestEnv::isolated();
    let bin = env.home().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let calls = env.home().join("tmux-calls");
    let stub = bin.join("tmux");
    std::fs::write(
        &stub,
        "#!/bin/sh\nprintf called >> \"$ACTIVITY_TEST_TMUX_CALLS\"\nexit 99\n",
    )
    .unwrap();
    std::fs::set_permissions(stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    env.story(env.home())
        .args(["daemon", "start"])
        .env("PATH", path)
        .env("STORYHOOK_VERIFIER_MIRROR", "1")
        .env("ACTIVITY_TEST_TMUX_CALLS", &calls)
        .assert()
        .success();
    env.stop_daemon();
    assert!(
        !calls.exists(),
        "store startup must not allocate a terminal reader"
    );
}

/// SH-771: a journal directory also holds its ignore file and a reader's
/// lock. Neither is a journal, and `daemon logs --directory` prints only
/// the day's records.
#[test]
fn project_logs_read_only_records_beside_the_ignore_file_and_view_lock() {
    let env = TestEnv::isolated();
    let directory = env.home().join("checkout/.storyhook/logs");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join(storyhook::daemon::activity::IGNORE_FILE),
        storyhook::daemon::activity::JOURNAL_IGNORE,
    )
    .unwrap();
    std::fs::write(directory.join(".view.lock"), "").unwrap();
    let row = serde_json::json!({"at":"2026-09-26T00:00:00Z", "level":"INFO",
        "source":"fixture", "stream":"event", "pid":1,
        "context":"project=fixture", "message":"only this"});
    std::fs::write(
        directory.join(format!("{}.jsonl", chrono::Utc::now().format("%Y-%m-%d"))),
        format!("{row}\n"),
    )
    .unwrap();
    let output = env
        .story(env.home())
        .args(["daemon", "logs", "--directory"])
        .arg(&directory)
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(serde_json::from_slice::<Value>(&output).unwrap(), row);
}

/// `git` in `cwd` under the fixture's environment, with global excludes
/// disabled so only the journal's own ignore file can hide it.
fn git_output(env: &TestEnv, cwd: &std::path::Path, args: &[&str]) -> String {
    let mut command = storyhook::env::git_env::command(cwd);
    env.apply(&mut command);
    let output = command
        .args(["-c", "core.excludesFile=/dev/null"])
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

/// SH-771, end to end through a real daemon: started over a checkout whose
/// repository already committed a journal file, the daemon makes the
/// journal ignore itself at start, reports the committed file on `story
/// daemon status` and `story verifier status` with the command that fixes
/// it, and never touches the index. A stopped daemon reports nothing.
#[test]
fn a_starting_daemon_fixes_the_journal_and_reports_committed_journal_files() {
    use storyhook_test_support::STORY_COMMAND_DEADLINE;
    let env = TestEnv::isolated();
    let project = env.project().prefix("HYG").git().build();
    env.stop_daemon();
    let logs = project.path().join(".storyhook/logs");
    std::fs::create_dir_all(&logs).unwrap();
    std::fs::write(logs.join("2026-09-24.jsonl"), "{}\n").unwrap();
    std::fs::write(logs.join(".view.lock"), "").unwrap();
    git_output(
        &env,
        project.path(),
        &["add", "-f", ".storyhook/logs/2026-09-24.jsonl"],
    );
    git_output(
        &env,
        project.path(),
        &["commit", "-qm", "a journal committed by mistake"],
    );
    let head = git_output(&env, project.path(), &["rev-parse", "HEAD"]);
    let index = git_output(&env, project.path(), &["ls-files", "-s"]);

    // Any store command starts the daemon, and its first sweep is its start.
    project.run(&["list"]).success();
    let deadline = std::time::Instant::now() + STORY_COMMAND_DEADLINE;
    let warnings = loop {
        let status = project.json(&["daemon", "status"]);
        let warnings = status["warnings"].as_array().unwrap().clone();
        if !warnings.is_empty() {
            break warnings;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no journal warning on daemon status: {status}"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    let warning = warnings[0].as_str().unwrap();
    assert!(
        warning.contains("git rm -r --cached .storyhook/logs"),
        "{warning}"
    );
    assert!(warning.contains("1 activity journal file in "), "{warning}");
    let human = project.run(&["verifier", "status"]).success();
    let human = String::from_utf8_lossy(&human.get_output().stdout).into_owned();
    assert!(human.contains(&format!("warning: {warning}")), "{human}");
    let verifier = project.json(&["verifier", "status"]);
    assert_eq!(
        verifier["verifier"]["journal_warning"], warning,
        "{verifier}"
    );

    assert_eq!(
        std::fs::read(logs.join(storyhook::daemon::activity::IGNORE_FILE)).unwrap(),
        storyhook::daemon::activity::JOURNAL_IGNORE,
        "the daemon made the pre-existing journal ignore itself"
    );
    let status = git_output(
        &env,
        project.path(),
        &["status", "--porcelain", "--untracked-files=all"],
    );
    assert!(!status.contains(".storyhook/logs"), "{status}");
    assert_eq!(
        git_output(&env, project.path(), &["rev-parse", "HEAD"]),
        head
    );
    assert_eq!(git_output(&env, project.path(), &["ls-files", "-s"]), index);

    env.stop_daemon();
    let stopped = project.json(&["daemon", "status"]);
    assert_eq!(stopped["warnings"], serde_json::json!([]), "{stopped}");
}
