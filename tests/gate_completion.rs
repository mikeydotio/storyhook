//! SH-701: run the tracked Makefile and gate orchestration against controlled leaves.

use std::process::Command;

#[test]
fn independent_gate_legs_finish_and_dependencies_remain_honest() {
    let output = Command::new("python3")
        .args(["-B", "tests/support/gate_completion.py"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run gate continuation contracts");
    assert!(
        output.status.success(),
        "gate continuation contracts failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
