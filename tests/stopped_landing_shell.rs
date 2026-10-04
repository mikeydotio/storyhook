//! SH-882 keeps ordinary certification separate from stopped-mode preparation.

#[test]
fn stopped_landing_preserves_real_git_and_receipt_boundaries() {
    let output = storyhook_test_support::ChildGuard::spawn_with_output(
        std::process::Command::new("python3").arg("-B").arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/tests/test_stopped_landing.py"
        )),
    )
    .expect("run stopped landing boundary regressions")
    .wait_with_output_within(
        storyhook_test_support::load_grace::graced_now(std::time::Duration::from_secs(180)),
        || "stopped landing boundary regressions did not finish".into(),
    );
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
