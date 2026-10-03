//! Production admission components under deterministic policies and owned processes.

#[test]
fn host_resource_admission_contract() {
    run("test_host_admission.py");
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

fn run(script: &str) {
    let output = std::process::Command::new("python3")
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
