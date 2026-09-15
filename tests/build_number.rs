//! SH-732: numbered build identity survives every public transport.

use storyhook::api::wire::{WireRequest, WireResponse};
use storyhook::cli::Invocation;
use storyhook::daemon::lifecycle::{DaemonInfo, Hello};

#[test]
fn cli_json_reports_the_same_numbered_identity_as_text() {
    let env = storyhook_test_support::TestEnv::shared();
    let output = env
        .story(env.home())
        .args(["--json", "--version"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["message"], storyhook::version::full());
}

#[test]
fn production_counter_wrapper_passes_its_process_contracts() {
    let output = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/test-build-number.py"
        ))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn request_and_response_publish_numeric_identity_without_changing_semver() {
    let request = WireRequest::new(Invocation::Version, "/tmp");
    let response = WireResponse::new(
        "number-test".into(),
        Ok(storyhook::output::Response::Message("ok".into())),
    );
    for (value, semver_key, build_key) in [
        (
            serde_json::to_value(request).unwrap(),
            "client_version",
            "client_build_number",
        ),
        (
            serde_json::to_value(response).unwrap(),
            "server_version",
            "server_build_number",
        ),
    ] {
        assert_eq!(value[semver_key], env!("CARGO_PKG_VERSION"));
        assert_eq!(value[build_key], storyhook::version::build_number());
    }
}

#[test]
fn legacy_daemon_identity_has_no_invented_number() {
    let info: DaemonInfo = serde_json::from_value(serde_json::json!({
        "pid": 1, "port": 2, "version": "1.2.3", "protocol": 1,
        "exe": "/tmp/story", "exe_mtime": 0, "started_at": "then", "token": "test"
    }))
    .unwrap();
    assert_eq!(info.build_number, None);
    assert_eq!(info.display_version(), "1.2.3");
    let hello: Hello = serde_json::from_value(serde_json::json!({
        "version": "1.2.3", "protocol": 1, "pid": 1, "started_at": "then"
    }))
    .unwrap();
    assert_eq!(hello.build_number, None);
}

#[test]
fn build_mismatch_refuses_same_path_and_mtime_daemon() {
    let exe = std::env::current_exe().unwrap();
    let metadata = std::fs::metadata(&exe).unwrap();
    use std::os::unix::fs::MetadataExt;
    let mut info: DaemonInfo = serde_json::from_value(serde_json::json!({
        "pid": 1, "port": 2, "version": env!("CARGO_PKG_VERSION"), "protocol": 1,
        "build_number": storyhook::version::build_number(),
        "exe": exe, "exe_mtime": metadata.mtime(), "started_at": "then", "token": "test"
    }))
    .unwrap();
    assert!(info.is_this_binary());
    info.build_number = Some(storyhook::version::build_number().wrapping_add(1));
    assert!(!info.is_this_binary());
}

#[test]
fn old_wire_envelopes_and_crash_records_decode_without_a_number() {
    let request = WireRequest::new(Invocation::Version, "/tmp");
    let mut value = serde_json::to_value(request).unwrap();
    value.as_object_mut().unwrap().remove("client_build_number");
    let old: WireRequest = serde_json::from_value(value).unwrap();
    assert_eq!(old.client_build_number, None);
    let response = WireResponse::new(
        "old".into(),
        Ok(storyhook::output::Response::Message("ok".into())),
    );
    let mut value = serde_json::to_value(response).unwrap();
    value.as_object_mut().unwrap().remove("server_build_number");
    let old: WireResponse = serde_json::from_value(value).unwrap();
    assert_eq!(old.server_build_number, None);
    let crash: storyhook::daemon::crash::CrashedDaemon =
        serde_json::from_value(serde_json::json!({
            "pid": 1, "version": "1.2.3", "started_at": "then"
        }))
        .unwrap();
    assert_eq!(crash.build_number, None);
}
