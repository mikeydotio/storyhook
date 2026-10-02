//! Real-Git lifecycle regressions run against the same bundled Python/shell helpers.

use std::path::Path;
use std::process::Command;

#[test]
fn shared_verifier_lifecycle_recovers_without_losing_evidence() {
    let result = Command::new("python3")
        .arg("-B")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/tests/run_verifier_tests.py"))
        .arg("lifecycle")
        .output()
        .expect("run isolated verifier lifecycle regressions");
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn completed_verifier_verdicts_survive_cleanup_failures() {
    let result = Command::new("python3")
        .arg("-B")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/tests/run_verifier_tests.py"))
        .arg("verdict")
        .output()
        .expect("run isolated completed-verdict regressions");
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn verifier_gates_run_at_the_gate_scheduling_class() {
    let result = Command::new("python3")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/tests/test_gate_class.py"))
        .output()
        .expect("run gate scheduling-class regressions");
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
