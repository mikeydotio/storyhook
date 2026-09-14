//! Isolated probes of activity startup and its real shell subprocess boundary.

use super::*;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::sync::atomic::AtomicUsize;

use storyhook_test_support::{ChildGuard, STORY_COMMAND_DEADLINE, daemon_containment, scratch_dir};

// The window opener is detached. Count its launch synchronously so a forbidden
// launch cannot evade a negative assertion through thread scheduling.
pub(super) static WINDOW_STARTS: AtomicUsize = AtomicUsize::new(0);

const PROBE_ROOT: &str = "SH699_ACTIVITY_PROBE_ROOT";
const PROBE_MODE: &str = "SH699_ACTIVITY_PROBE_MODE";
const START_TEST: &str = "daemon::activity::isolation_tests::fixture_activity_start_keeps_journaling_without_window_launch";
const WINDOW_TEST: &str = "daemon::activity::isolation_tests::activity_window_child_receives_resolved_policy_and_state_home";

fn executable(path: &Path, text: &str) {
    std::fs::write(path, text).expect("write recording tool");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .expect("make recording tool executable");
}

fn run_probe(test: &str, mode: &str, mirror: Option<&str>) {
    let root = scratch_dir();
    let bin = root.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    executable(
        &bin.join("bash"),
        "#!/bin/sh\n/usr/bin/env > \"$SH699_HELPER_ENV\"\nexec /bin/bash \"$@\"\n",
    );
    executable(
        &bin.join("tmux"),
        "#!/bin/sh\nprintf '%s\\n' \"$1\" >> \"$SH699_TMUX_CALLS\"\nexit 0\n",
    );
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").expect("executable search path"),
    ));
    let mut command = Command::new(std::env::current_exe().expect("this unit test executable"));
    command
        .env_clear()
        .envs(daemon_containment())
        .envs(Environment::at(root.path()).child_vars())
        .env("PATH", std::env::join_paths(paths).unwrap())
        .env("HOME", root.path())
        .env("XDG_STATE_HOME", root.path().join("ambient-state"))
        .env("SH699_HELPER_ENV", root.path().join("helper.env"))
        .env("SH699_TMUX_CALLS", root.path().join("tmux.calls"))
        .env("TMUX", "must-not-reach-activity-helper")
        .env("TMUX_PANE", "%999")
        .env(PROBE_ROOT, root.path())
        .env(PROBE_MODE, mode)
        .args(["--exact", test, "--nocapture"]);
    match mirror {
        Some(value) => {
            command.env("STORYHOOK_VERIFIER_MIRROR", value);
        }
        None => {
            command.env_remove("STORYHOOK_VERIFIER_MIRROR");
        }
    }
    let output = ChildGuard::spawn_with_output(&mut command)
        .expect("spawn isolated activity probe")
        .wait_with_output_within(STORY_COMMAND_DEADLINE, || {
            format!("activity probe {mode} with ambient mirror {mirror:?} did not finish")
        });
    assert!(
        output.status.success(),
        "activity probe {mode} with ambient mirror {mirror:?} failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn fixture_activity_start_keeps_journaling_without_window_launch() {
    let Some(root) = std::env::var_os(PROBE_ROOT) else {
        for mirror in [None, Some("1")] {
            run_probe(START_TEST, "start", mirror);
        }
        return;
    };
    let root = Path::new(&root);
    let env = Environment::at(root.join("fixture-home"));
    std::fs::create_dir_all(env.daemon_state_dir()).unwrap();
    std::fs::write(env.daemon_log(), "").unwrap();

    let guard = start(&env);
    assert!(enabled(), "fixture activity still installs its journal");
    emit(
        "INFO",
        "SH-699",
        "event",
        "fixture",
        "journal survives isolation",
    );
    drop(guard);

    assert_eq!(
        WINDOW_STARTS.load(Ordering::SeqCst),
        0,
        "Environment::at must prevent the detached activity-window launch itself"
    );
    assert!(
        !root.join("helper.env").exists(),
        "a window helper was launched"
    );
    assert!(
        !root.join("tmux.calls").exists(),
        "a fixture contacted tmux"
    );
    let records: Vec<Record> = std::fs::read_dir(env.daemon_state_dir().join("activity"))
        .unwrap()
        .flat_map(|entry| {
            std::fs::read_to_string(entry.unwrap().path())
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect::<Vec<Record>>()
        })
        .collect();
    for message in ["journal survives isolation", "daemon stopped"] {
        assert!(
            records.iter().any(|record| record.message == message),
            "disabled mirrors lost activity record {message}"
        );
    }
    assert!(
        records
            .iter()
            .any(|record| record.message.starts_with("daemon started "))
    );
}

#[test]
fn activity_window_child_receives_resolved_policy_and_state_home() {
    let Some(root) = std::env::var_os(PROBE_ROOT) else {
        run_probe(WINDOW_TEST, "fixture", Some("1"));
        run_probe(WINDOW_TEST, "process", Some("1"));
        return;
    };
    let root = Path::new(&root);
    let fixture = std::env::var(PROBE_MODE).unwrap() == "fixture";
    let env = if fixture {
        Environment::at(root.join("fixture-home"))
    } else {
        Environment::from_process(None).unwrap()
    };
    std::fs::create_dir_all(env.store_path().parent().unwrap()).unwrap();
    std::fs::write(env.store_path(), "").unwrap();

    window::open(&env);

    let seen: std::collections::BTreeMap<String, String> =
        std::fs::read_to_string(root.join("helper.env"))
            .expect("the real window boundary launched its shell helper")
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect();
    assert_eq!(
        seen["STORYHOOK_VERIFIER_MIRROR"],
        if fixture { "0" } else { "1" }
    );
    assert_eq!(
        Path::new(&seen["XDG_STATE_HOME"]),
        env.state_home().parent().unwrap()
    );
    assert_eq!(Path::new(&seen["STORYHOOK_STORE_PATH"]), env.store_path());
    assert!(!seen.contains_key("TMUX"));
    assert!(!seen.contains_key("TMUX_PANE"));
    if fixture {
        assert!(
            !root.join("tmux.calls").exists(),
            "disabled helper contacted tmux"
        );
        assert_ne!(
            seen["XDG_STATE_HOME"],
            root.join("ambient-state").to_str().unwrap(),
            "the fixture must differ from ambient state so the propagation assertion is meaningful"
        );
    } else {
        let calls = std::fs::read_to_string(root.join("tmux.calls"))
            .expect("positive control: enabled shipping shell calls recording tmux");
        assert!(calls.lines().any(|call| call == "respawn-pane"), "{calls}");
    }
}
