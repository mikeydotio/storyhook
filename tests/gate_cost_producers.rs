//! Exercises the real case and phase producers without running a verification gate.

#[test]
fn producer_evidence_preserves_exact_identity_and_measured_boundaries() {
    let output = std::process::Command::new("python3")
        .arg("-B")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/tests/test_gate_cost.py"
        ))
        .output()
        .expect("run gate cost producer regressions");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
