//! Detached debug products retain explicit generations and pins before pruning.

#[test]
fn detached_build_product_retention_contract() {
    let output = std::process::Command::new("python3")
        .arg("-B")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/tests/test_build_product_retention.py"
        ))
        .env_remove("STORYHOOK_HOST_GRANT")
        .env_remove("STORYHOOK_HOST_REQUEST")
        .output()
        .expect("run detached build product retention regressions");
    assert!(output.status.success(), "{output:?}");
}
