//! Runs the offline lifecycle audit's synthetic event-history contracts.

use std::process::Command;

#[test]
fn lifecycle_audit_contracts() {
    run_contracts("python3");
}

// launchd can resolve system Python even when an interactive shell uses Homebrew.
#[cfg(target_os = "macos")]
#[test]
fn lifecycle_audit_contracts_with_system_python() {
    run_contracts("/usr/bin/python3");
}

fn run_contracts(interpreter: &str) {
    let output = Command::new(interpreter)
        .args(["-B", "tests/support/lifecycle_audit.py"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run lifecycle audit contracts");
    assert!(
        output.status.success(),
        "{interpreter}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
