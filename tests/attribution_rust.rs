//! Exact diagnostic selection and interpretation against a real libtest subprocess.
use std::process::{Command, Output};
use storyhook::service::attribution::{ProbeOutcome, RustCase, RustTarget};

const CASE: &str = "native_case";

fn case() -> RustCase {
    RustCase::new(
        "storyhook",
        RustTarget::Integration("attribution_rust".into()),
        CASE,
    )
    .unwrap()
}

fn native(args: &[String], mode: &str) -> Output {
    assert!(
        std::env::var_os("SH870_NATIVE_CASE").is_none(),
        "an incorrect selection must fail instead of recursively launching test drivers"
    );
    Command::new(std::env::current_exe().unwrap())
        .args(args)
        .env("SH870_NATIVE_CASE", mode)
        .env("RUST_BACKTRACE", "0")
        .output()
        .unwrap()
}

#[test]
fn native_case() {
    if let Ok(mode) = std::env::var("SH870_NATIVE_CASE") {
        assert_eq!(mode, "pass", "exact retained assertion");
    }
}

#[test]
fn native_case_sibling_must_not_run() {
    assert!(std::env::var_os("SH870_NATIVE_CASE").is_none());
}

#[test]
#[ignore = "an ignored case is not a diagnostic execution"]
fn native_ignored() {}

#[test]
fn rust_selection_is_literal_and_missing_or_ignored_tests_are_unavailable() {
    let selected = case();
    assert_eq!(
        selected.build_arguments(),
        [
            "test",
            "--offline",
            "--locked",
            "--package",
            "storyhook",
            "--test",
            "attribution_rust",
            "--no-run",
            "--message-format=json"
        ]
    );
    let listed = native(&selected.list_arguments(), "pass");
    selected
        .validate_listing(&listed.stdout, &listed.stderr, false, listed.status.code())
        .unwrap();
    let result = native(&selected.run_arguments(), "pass");
    let observed = selected.observe(&result.stdout, &result.stderr, false, result.status.code());
    assert_eq!(
        observed.executions,
        1,
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
    assert_eq!(observed.outcome, ProbeOutcome::Passed);
    for name in ["no_such_case", "native_ignored"] {
        let check = RustCase::new(
            "storyhook",
            RustTarget::Integration("attribution_rust".into()),
            name,
        )
        .unwrap();
        let out = native(&check.run_arguments(), "pass");
        assert_unavailable(&check, &out.stdout, &out.stderr, false, out.status.code());
        if name == "no_such_case" {
            let out = native(&check.list_arguments(), "pass");
            assert!(
                check
                    .validate_listing(&out.stdout, &out.stderr, false, out.status.code())
                    .is_err()
            );
        }
    }
}

#[test]
fn rust_failures_keep_assertion_identity_across_native_processes() {
    let selected = case();
    let result = |mode| {
        let out = native(&selected.run_arguments(), mode);
        let observed = selected.observe(&out.stdout, &out.stderr, false, out.status.code());
        assert_eq!(
            observed.executions,
            1,
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
        let ProbeOutcome::Failed { signature } = observed.outcome else {
            panic!("{observed:?}")
        };
        signature
    };
    let first = result("first failure");
    assert_eq!(first, result("first failure"));
    assert_ne!(first, result("different assertion value"));
}

#[test]
fn original_gate_failure_requires_one_complete_exact_target_frame() {
    let selected = case();
    let out = native(&selected.run_arguments(), "original assertion");
    let ProbeOutcome::Failed { signature } = selected
        .observe(&out.stdout, &out.stderr, false, out.status.code())
        .outcome
    else {
        panic!("native case did not fail");
    };
    let body = String::from_utf8(out.stdout).unwrap();
    let header =
        "     Running tests/attribution_rust.rs (/owned/target/debug/deps/attribution_rust-abc)\n";
    let log = format!(
        "preparation output\n{header}{body}error: test failed, to rerun pass --test attribution_rust\n"
    );
    assert_eq!(
        selected.original_failure(log.as_bytes()).unwrap(),
        signature
    );
    for invalid in [
        body.clone(),
        log.replace("tests/attribution_rust.rs", "tests/another.rs"),
        log.replace("test native_case ... FAILED", "test foreign ... FAILED"),
        log.replace("1 failed", "2 failed"),
        log.replace("test result: FAILED", "truncated: FAILED"),
        format!("{log}{log}"),
        format!("{log}running 1 test\ntest native_case ... FAILED\n"),
        log.replace("failures:\n    native_case", "failures:\n    foreign"),
        log.replace(&body, "\nrunning 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n"),
    ] { assert!(selected.original_failure(invalid.as_bytes()).is_err(), "{invalid}"); }
}

fn assert_unavailable(check: &RustCase, out: &[u8], err: &[u8], cut: bool, code: Option<i32>) {
    let result = check.observe(out, err, cut, code);
    assert_eq!(result.executions, 0, "{result:?}");
    assert!(
        matches!(result.outcome, ProbeOutcome::Unavailable { .. }),
        "{result:?}"
    );
}

#[test]
fn ambiguous_truncated_or_inconsistent_native_results_cannot_count_as_execution() {
    let selected = case();
    let pass = native(&selected.run_arguments(), "pass");
    let fail = native(&selected.run_arguments(), "failure");
    assert_unavailable(&selected, &pass.stdout, &[], true, Some(0));
    assert_unavailable(&selected, &pass.stdout, &[], false, None);
    assert_unavailable(&selected, &pass.stdout, &[], false, Some(101));
    assert_unavailable(&selected, &fail.stdout, &[], false, Some(0));
    assert_unavailable(&selected, &pass.stdout, b"host tool failed", false, Some(0));
    assert_unavailable(
        &selected,
        b"error[E0001]: compile failure",
        &[],
        false,
        Some(101),
    );
    assert_unavailable(&selected, &[0xff], &[], false, Some(0));
    let pass_text = String::from_utf8(pass.stdout).unwrap();
    for edited in [
        format!("{pass_text}{pass_text}"),
        pass_text.replace("running 1 test", "running 2 tests"),
        pass_text.replace(
            "native_case ... ok",
            "native_case_sibling_must_not_run ... ok",
        ),
        pass_text.replace("1 passed; 0 failed", "0 passed; 0 failed"),
        pass_text.replace("0 ignored", "1 ignored"),
        pass_text.replace("0 measured", "1 measured"),
        pass_text.replace("test result:", "unknown result:"),
        pass_text.replace(" ... ok", " ... ignored"),
        format!("foreign output\n{pass_text}"),
        pass_text.replace(
            "test native_case ... ok",
            "test native_case ... ok\ntest native_case ... ok",
        ),
    ] {
        assert_unavailable(&selected, edited.as_bytes(), &[], false, Some(0));
    }
    let failure = String::from_utf8(fail.stdout).unwrap();
    let assertion_text = failure.replace(
        "exact retained assertion",
        "thread 'foreign' panicked at somewhere",
    );
    let observed = selected.observe(assertion_text.as_bytes(), &[], false, Some(101));
    assert_eq!(observed.executions, 1, "{observed:?}");
    assert_ne!(
        observed.outcome,
        selected
            .observe(failure.as_bytes(), &[], false, Some(101))
            .outcome,
        "assertion data is retained, not stripped as a runtime header"
    );
    for edited in [
        failure.replace("---- native_case stdout ----", "---- foreign stdout ----"),
        failure.replace("thread 'native_case'", "thread 'foreign'"),
        failure.replace("failures:", "failures:\nfailures:"),
        failure.replace("1 failed", "2 failed"),
        failure.replace(
            "exact retained assertion",
            "\nthread 'foreign' panicked at somewhere:",
        ),
    ] {
        assert_unavailable(&selected, edited.as_bytes(), &[], false, Some(101));
    }
}

#[test]
fn rust_selection_and_listing_reject_broad_or_forged_identities() {
    for invalid in ["", "--all", "*", "a?b", "a[b]", "../a", "a\nb", "a b"] {
        assert!(
            RustCase::new(invalid, RustTarget::Library, CASE).is_err(),
            "{invalid}"
        );
        assert!(
            RustCase::new("pkg", RustTarget::Integration(invalid.into()), CASE).is_err(),
            "{invalid}"
        );
        assert!(
            RustCase::new("pkg", RustTarget::Library, invalid).is_err(),
            "{invalid}"
        );
    }
    let check = RustCase::new("pkg", RustTarget::Library, "module::case").unwrap();
    assert!(check.build_arguments().iter().any(|arg| arg == "--lib"));
    let selected = case();
    let good = b"native_case: test\n\n1 test, 0 benchmarks\n";
    selected
        .validate_listing(good, &[], false, Some(0))
        .unwrap();
    for bytes in [
        b"native_case: benchmark\n\n0 tests, 1 benchmark\n".as_slice(),
        b"native_case: test\nnative_case: test\n\n2 tests, 0 benchmarks\n",
        b"native_case_sibling: test\n\n1 test, 0 benchmarks\n",
        b"0 tests, 0 benchmarks\n",
    ] {
        assert!(
            selected
                .validate_listing(bytes, &[], false, Some(0))
                .is_err()
        );
    }
    assert!(selected.validate_listing(good, &[], true, Some(0)).is_err());
    assert!(
        selected
            .validate_listing(good, &[], false, Some(1))
            .is_err()
    );
    assert!(
        selected
            .validate_listing(good, b"unavailable", false, Some(0))
            .is_err()
    );
}

#[test]
fn diagnosis_head_refresh_uses_only_metadata_endpoint() {
    let output = Command::new("python3")
        .arg("-B")
        .arg(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("scripts/tests/test_attribution_head.py"),
        )
        .output()
        .expect("run metadata-only shell regression");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
