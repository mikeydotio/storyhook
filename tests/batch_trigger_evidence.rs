//! The offline SH-841 report must not turn missing or retry evidence green.

#[test]
fn retained_batch_trigger_evidence_preserves_unknowns() {
    let result = std::process::Command::new("python3")
        .arg("-B")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/tests/test_batch_trigger_evidence.py"
        ))
        .output()
        .expect("run offline batch trigger evidence regressions");
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
