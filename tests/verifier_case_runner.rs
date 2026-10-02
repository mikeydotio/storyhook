//! SH-793: the Python case runner preserves isolation, coverage and diagnostics.

use std::path::Path;
use std::process::Command;

#[test]
fn verifier_python_cases_have_bounded_process_isolation() {
    let result = Command::new("python3")
        .arg("-B")
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("scripts/tests/test_verifier_case_runner.py"),
        )
        .output()
        .expect("run Python case runner contracts");
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
