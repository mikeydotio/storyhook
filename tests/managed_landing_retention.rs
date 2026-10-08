//! SH-871: private managed landing retains its branch after normal certification.
//! This local transport fixture does not certify GitHub's service behavior.

#[test]
fn managed_landing_retains_owned_branch_and_preserves_ordinary_protected_flow() {
    let output = storyhook_test_support::ChildGuard::spawn_with_output(
        std::process::Command::new("python3").arg("-B").arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/tests/test_managed_landing_retention.py"
        )),
    )
    .expect("run managed landing retention regressions")
    .wait_with_output_within(
        storyhook_test_support::load_grace::graced_now(std::time::Duration::from_secs(180)),
        || "managed landing retention regressions did not finish".into(),
    );
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
