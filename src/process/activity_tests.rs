//! The two capture deadline policies must share the daemon's activity sink.

use super::*;
use storyhook_test_support::{ChildGuard, STORY_COMMAND_DEADLINE, TestEnv, scratch_dir};

#[test]
fn both_deadline_modes_record_output_and_timeouts() {
    // The sink is process-wide and daemon-only. Run this probe in its own
    // test process so it cannot capture another library test's activity.
    if std::env::var_os("STORYHOOK_ACTIVITY_TEST_CHILD").is_none() {
        let env = TestEnv::isolated();
        let test_binary = std::env::current_exe().unwrap();
        let mut command = Command::new(test_binary);
        env.apply(&mut command);
        command
            .args([
                "--exact",
                "process::activity_tests::both_deadline_modes_record_output_and_timeouts",
                "--nocapture",
            ])
            .env("STORYHOOK_ACTIVITY_TEST_CHILD", "1");
        let mut child = ChildGuard::spawn_with_output(&mut command).unwrap();
        let output = child.wait_with_output_within(STORY_COMMAND_DEADLINE, || {
            "activity/progress integration probe did not finish".into()
        });
        assert!(output.status.success(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "the isolated probe must actually execute one test: {output:?}"
        );
        return;
    }

    let root = scratch_dir();
    let env = crate::env::Environment::at(root.path());
    let _activity = crate::daemon::activity::start(&env);
    let progress = root.path().join("progress.jsonl");
    std::fs::write(&progress, "").unwrap();
    let idle = Duration::from_secs(1);

    let mut command = Command::new("sh");
    command.args(["-c", "printf absolute-out; printf absolute-err >&2"]);
    let captured = run_captured(command, idle).unwrap_or_else(|error| panic!("{}", error.detail()));
    assert!(captured.status.success());
    assert_eq!(captured.stdout, b"absolute-out");
    assert_eq!(captured.stderr, b"absolute-err");

    let mut command = Command::new("sh");
    command.args([
        "-c",
        "test -d \"$STORYHOOK_ACTIVITY_LOG_DIR\" || exit 91; printf progressing-out; printf progressing-err >&2; for i in 1 2 3 4 5 6 7 8; do printf '{}\\n' >> \"$1\"; sleep \"$2\"; done",
        "progress-probe",
    ]).arg(&progress).arg((idle / 4).as_secs_f64().to_string());
    let started = Instant::now();
    let captured = run_captured_with_progress_and_registration(
        command,
        idle,
        TerminationPolicy::Kill,
        &progress,
        &Cancellation::default(),
        |_| Ok(()),
    )
    .unwrap_or_else(|error| panic!("{}", error.detail()));
    assert!(captured.status.success());
    assert!(
        started.elapsed() > idle,
        "progress must renew the original budget"
    );
    assert_eq!(captured.stdout, b"progressing-out");
    assert_eq!(captured.stderr, b"progressing-err");

    let mut command = Command::new("sh");
    command
        .args(["-c", "printf stalled-err >&2; sleep \"$1\"", "stall-probe"])
        .arg((idle * 4).as_secs_f64().to_string());
    assert!(matches!(
        run_captured_with_progress_and_registration(
            command,
            idle,
            TerminationPolicy::Kill,
            &progress,
            &Cancellation::default(),
            |_| Ok(()),
        ),
        Err(CaptureError::Timeout(_))
    ));

    let journal = std::fs::read_dir(env.daemon_state_dir().join("activity"))
        .unwrap()
        .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect::<String>();
    let records: Vec<serde_json::Value> = journal
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    for (stream, message) in [
        ("stdout", "absolute-out"),
        ("stderr", "absolute-err"),
        ("stdout", "progressing-out"),
        ("stderr", "progressing-err"),
        ("stderr", "stalled-err"),
        ("event", "process timed out; group terminated"),
    ] {
        assert!(
            records.iter().any(|record| record["source"] == "sh"
                && record["stream"] == stream
                && record["message"] == message),
            "missing {stream} {message}: {journal}"
        );
    }
}
