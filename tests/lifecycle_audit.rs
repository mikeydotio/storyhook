//! Runs the offline lifecycle audit's synthetic event-history contracts.

use std::process::Command;

#[test]
fn lifecycle_audit_contracts() {
    let output = Command::new("python3")
        .args(["-B", "tests/support/lifecycle_audit.py"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run lifecycle audit contracts");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
