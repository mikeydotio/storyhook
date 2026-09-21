//! Isolated probes of activity startup and its real shell subprocess boundary.

use super::*;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use storyhook_test_support::{ChildGuard, STORY_COMMAND_DEADLINE, daemon_containment, scratch_dir};

const PROBE_ROOT: &str = "SH699_ACTIVITY_PROBE_ROOT";
const PROBE_MODE: &str = "SH699_ACTIVITY_PROBE_MODE";
const START_TEST: &str = "daemon::activity::isolation_tests::fixture_activity_start_keeps_journaling_without_window_launch";

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
        "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$SH699_TMUX_CALLS\"\nexit 0\n",
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
