//! SH-863: an explicit fixture floor survives the real process boundary.

use std::process::Command;
use storyhook::env::Environment;
use storyhook_test_support::{ChildGuard, STORY_COMMAND_DEADLINE, TestEnv, load_grace};

const VARIABLE: &str = "STORYHOOK_TEST_SUBPROCESS_PATIENCE_MS";

#[test]
fn process_declarations_survive_environment_clones_and_child_routing() {
    for value in [None, Some("1"), Some("3000"), Some("900000")] {
        let fixture = TestEnv::isolated();
        let mut command = Command::new(std::env::current_exe().unwrap());
        fixture.apply(&mut command);
        command.args(["--ignored", "--exact", "environment_worker", "--nocapture"]);
        if let Some(value) = value {
            command.env(VARIABLE, value);
        }
        let mut child = ChildGuard::spawn_with_output(&mut command).unwrap();
        let output = child
            .wait_with_output_within(load_grace::graced_now(STORY_COMMAND_DEADLINE), || {
                format!("environment worker did not finish for {value:?}")
            });
        assert!(output.status.success(), "{value:?}: {output:?}");
    }
}

#[test]
#[ignore = "isolated worker for process_declarations_survive_environment_clones_and_child_routing"]
fn environment_worker() {
    let env = Environment::from_process(None).unwrap();
    let expected = std::env::var_os(VARIABLE);
    let child_vars = env.clone().child_vars();
    let actual = child_vars
        .iter()
        .find(|(name, _)| *name == VARIABLE)
        .map(|(_, value)| value.clone());
    assert_eq!(actual, expected);
    assert!(
        !Environment::at(env.home())
            .child_vars()
            .iter()
            .any(|(name, _)| *name == VARIABLE),
        "an explicitly rooted environment must not adopt ambient patience"
    );
}

#[test]
fn the_real_cli_reports_invalid_declarations_before_starting_a_daemon() {
    let fixture = TestEnv::isolated();
    for value in ["", "0", "-1", "900001", "not-milliseconds"] {
        let output = fixture
            .story(fixture.home())
            .args(["daemon", "status"])
            .env(VARIABLE, value)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{value:?}: {output:?}");
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        assert!(diagnostic.contains(VARIABLE), "{diagnostic}");
        assert!(diagnostic.contains("1..=900000"), "{diagnostic}");
    }
    assert_eq!(load_grace::PATIENCE_CEILING.as_millis(), 900_000);
}
