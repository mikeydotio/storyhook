//! SH-801: a local measurement operation cannot become a remote verifier action.

use std::path::Path;
use std::process::Command;

#[test]
fn throughput_cohort_and_execution_evidence_regressions() {
    for script in [
        "scripts/tests/test_gate_measurement_exposure.py",
        "scripts/tests/test_git_shim_measurement.py",
        "scripts/tests/test_gate_measurement_cohorts.py",
        "scripts/tests/test_gate_measurement_execution.py",
        "scripts/tests/test_gate_measurement_campaign.py",
        "scripts/tests/test_gate_measurement_native.py",
        "scripts/tests/test_gate_measurement_python_inputs.py",
        "scripts/tests/test_gate_measurement_ca_bundle.py",
    ] {
        let result =
            Command::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/python-bin/python3"))
                .args(["-B", script])
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .output()
                .unwrap();
        assert!(
            result.status.success(),
            "{script}: {}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

#[test]
fn measurement_live_storage_churn_regressions() {
    let result =
        Command::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/python-bin/python3"))
            .args(["-B", "scripts/tests/test_gate_measurement_storage_churn.py"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn measurement_cargo_dsym_alias_regressions() {
    let result =
        Command::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/python-bin/python3"))
            .args(["-B", "scripts/tests/test_gate_measurement_dsym_alias.py"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn measurement_discovery_is_operator_local_and_delegates_output() {
    use storyhook::cli::discovery::{self, Access, Audience, OutputClass};
    let args = ["verifier", "measure-gate-class", "--audience", "operator"].map(str::to_string);
    let document = discovery::describe(&args).unwrap();
    let entry = document
        .commands
        .iter()
        .find(|entry| entry.path == ["verifier", "measure-gate-class"])
        .unwrap();
    assert_eq!(entry.audience, Audience::Operator);
    assert_eq!(entry.capabilities.effects.store, Access::None);
    assert!(!entry.capabilities.effects.may_start_daemon);
    assert_eq!(entry.output[0].class, OutputClass::DelegatedHelper);
}

#[test]
fn measurement_bounded_policy_and_storage_regressions() {
    let result =
        Command::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/python-bin/python3"))
            .args(["-B", "scripts/tests/test_gate_measurement_bounds.py"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn measurement_requires_complete_arguments_and_is_store_free() {
    let args = [
        "verifier",
        "measure-gate-class",
        "/checkout",
        "abc",
        "--output",
        "/result",
    ];
    let invocation =
        storyhook::cli::parse_invocation(&args.map(str::to_string)).expect("measurement command");
    assert!(storyhook::invoke::needs_no_store(&invocation));
    assert!(
        storyhook::invoke::dispatch_without_store(invocation).is_err(),
        "only the local CLI may exec an owned measurement"
    );
    for words in [
        vec!["verifier", "measure-gate-class"],
        vec!["verifier", "measure-gate-class", "/checkout", "abc"],
        vec![
            "verifier",
            "measure-gate-class",
            "/checkout",
            "abc",
            "--output",
            "/result",
            "extra",
        ],
    ] {
        assert!(
            storyhook::cli::parse_invocation(
                &words.into_iter().map(str::to_string).collect::<Vec<_>>()
            )
            .is_err()
        );
    }
}

#[test]
fn measurement_evidence_and_owned_process_regressions() {
    let result = Command::new("python3")
        .arg("-B")
        .env(
            "STORYHOOK_MEASUREMENT_TEST_BINARY",
            storyhook_test_support::story_binary(),
        )
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/tests/test_gate_measurement.py"))
        .output()
        .expect("run scheduling measurement regressions");
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
#[cfg(target_os = "macos")]
fn launch_reserves_separate_output_without_overwriting_foreign_content() {
    let root = storyhook_test_support::scratch_dir();
    let checkout = root.path().join("source");
    std::fs::create_dir(&checkout).unwrap();
    let prepare = |output: &Path, commit: &str| {
        storyhook::service::gate_measurement::command(&checkout, commit, output)
    };
    let nested = checkout.join("measurement");
    assert!(prepare(&nested, "abc").is_err());
    assert!(!nested.exists());
    let output = root.path().join("output");
    let command = prepare(&output, "abc").unwrap();
    assert_eq!(
        command.get_current_dir(),
        Some(std::fs::canonicalize(&checkout).unwrap().as_path())
    );
    assert!(prepare(&output, "abc").is_ok());
    assert!(prepare(&output, "different").is_err());
    let foreign = root.path().join("foreign");
    std::fs::create_dir(&foreign).unwrap();
    std::fs::write(foreign.join("keep"), "unrelated").unwrap();
    assert!(prepare(&foreign, "abc").is_err());
    assert_eq!(
        std::fs::read_to_string(foreign.join("keep")).unwrap(),
        "unrelated"
    );
}

#[test]
#[cfg(target_os = "macos")]
fn launch_preserves_the_reference_locale() {
    const CHILD: &str = "STORYHOOK_MEASUREMENT_LOCALE_TEST";
    if std::env::var_os(CHILD).is_some() {
        let root = storyhook_test_support::scratch_dir();
        let source = root.path().join("source");
        std::fs::create_dir(&source).unwrap();
        let command = storyhook::service::gate_measurement::command(
            &source,
            "abc",
            &root.path().join("output"),
        )
        .unwrap();
        let locale = command
            .get_envs()
            .find(|(name, _)| *name == "LC_ALL")
            .and_then(|(_, value)| value);
        assert_eq!(locale, Some(std::ffi::OsStr::new("C.UTF-8")));
        assert!(!command.get_envs().any(|(name, _)| name == CHILD));
        return;
    }
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["launch_preserves_the_reference_locale", "--exact"])
        .env(CHILD, "1")
        .env("LC_ALL", "C.UTF-8")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}
