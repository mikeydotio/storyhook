//! Production admission components under deterministic policies and owned processes.

#[test]
fn host_resource_admission_contract() {
    run("test_host_admission.py");
}

#[test]
fn host_admission_native_observation_preserves_custody() {
    run("test_host_native_observation.py");
}

#[test]
fn host_admission_native_process_contract() {
    run("test_host_admission_system.py");
}

#[test]
fn host_admission_sensor_contract() {
    run("test_host_admission_sensors.py");
}

#[test]
fn host_admission_activation_contract() {
    run("test_host_admission_activation.py");
}

#[test]
fn host_admission_publisher_contract() {
    run("test_host_admission_evidence.py");
}

#[test]
fn host_admission_usage_contract() {
    run("test_host_admission_usage.py");
}

#[test]
fn host_admission_runner_adapter_contract() {
    run("test_host_admission_adapter.py");
}

#[test]
fn host_admission_compiler_wrapper_contract() {
    run("test_rustc_slot_admission.py");
}

#[test]
fn host_admission_runner_wiring_contract() {
    run("test_runner_admission_wiring.py");
}

#[test]
fn host_admission_verifier_gate_contract() {
    run("test_verifier_admission.py");
}

#[test]
fn host_admission_runner_adoption_contract() {
    run("test_host_admission_runners.py");
}

#[test]
fn causal_diagnosis_requires_supported_admission_and_settled_sessions() {
    run("test_attribution_admission.py");
}

#[test]
fn host_admission_native_restoration_contract() {
    run("test_host_restoration.py");
}

/// These suites drive their own fixture authorities. A grant inherited from
/// the production runner that admitted this test binary (SH-869) would make
/// the client refuse every fixture root, so the bearer capability is removed;
/// the lease descriptor stays, and it names no fixture authority.
fn run(script: &str) {
    let output = std::process::Command::new("python3")
        .env_remove("STORYHOOK_HOST_GRANT")
        .env_remove("STORYHOOK_HOST_REQUEST")
        .arg("-B")
        .arg(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("scripts/tests")
                .join(script),
        )
        .output()
        .expect("run host admission regression tests");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn host_restoration_native_bridge_and_module_ship_with_the_verifier() {
    let files: Vec<_> = storyhook::daemon::verifier_bundle::files()
        .map(|(path, _, _)| path)
        .collect();
    assert!(files.contains(&"host-restoration.py"));
    assert!(files.contains(&"host_admission/restoration.py"));
}
