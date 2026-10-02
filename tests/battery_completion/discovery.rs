//! SH-795: the production pooled runner owns exact discovery before execution.

use super::{Fixture, combined};
use std::fs;
use std::process::Command;

/// A real libtest binary supplies filtering and ignore behavior, including the
/// doctest that must already be counted when a pooled case starts.
#[test]
fn pooled_discovery_counts_the_runnable_selection_before_execution() {
    let fixture = Fixture::new();
    fixture.write(
        "tests/second.rs",
        r#"fn check_total() {
    let path = std::env::var("EXPECTED_JOURNAL").unwrap();
    let journal = std::fs::read_to_string(path).unwrap();
    let expected = std::env::var("EXPECTED_TOTAL").unwrap();
    assert!(journal.contains(&format!("\"total\":{}", expected)), "{}", journal);
}
#[test] fn selected_runs() { check_total(); }
#[test] #[ignore] fn selected_ignored() { check_total(); }
#[test] fn skipped_case() { check_total(); }
"#,
    );
    fixture.write("src/lib.rs", "//! ```\n//! assert_eq!(1, 1);\n//! ```\n");
    let cargo = Command::new("sh")
        .args(["-c", "command -v cargo"])
        .output()
        .expect("locating real cargo");
    let cargo = String::from_utf8(cargo.stdout).expect("cargo path");
    fixture.fake_cargo(&format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> cargo-calls\nexec '{}' \"$@\"\n",
        cargo.trim().replace('\'', "'\\''")
    ));
    // Counts include doctests only on the unfiltered first row. The other rows
    // exercise the same test executable with libtest's actual selection logic.
    let cases: &[(&str, &[&str], u64)] = &[
        ("--only", &[], 3),
        ("--only-no-doc", &["--ignored"], 1),
        ("--only-no-doc", &["--include-ignored"], 3),
        ("--only-no-doc", &["selected"], 1),
        ("--only-no-doc", &["selected_runs", "--exact"], 1),
        ("--only-no-doc", &["--skip", "skipped_case"], 1),
        ("--only-no-doc", &["absent_case"], 0),
    ];
    for (index, (mode, flags, expected)) in cases.iter().enumerate() {
        let journal = fixture.path().join(format!("progress-{index}.ndjson"));
        fixture.write("cargo-calls", "");
        let mut args = vec![*mode, "second", "--"];
        args.extend_from_slice(flags);
        let out = fixture
            .run_tests(&args)
            .env("STORYHOOK_TEST_THREAD_BUDGET", "2")
            .env("STORYHOOK_GATE_PROGRESS", &journal)
            .env("EXPECTED_JOURNAL", &journal)
            .env("EXPECTED_TOTAL", expected.to_string())
            .output()
            .expect("running the pooled selection");
        assert!(out.status.success(), "{args:?}: {}", combined(&out));
        let events: Vec<serde_json::Value> = fs::read_to_string(&journal)
            .expect("reading progress")
            .lines()
            .map(|line| serde_json::from_str(line).expect("progress record"))
            .collect();
        let totals: Vec<_> = events
            .iter()
            .enumerate()
            .filter(|(_, e)| e.get("total").is_some())
            .collect();
        assert_eq!(totals.len(), 1, "{events:?}");
        assert_eq!(totals[0].1["total"], *expected, "{args:?}");
        let activity = |status: &str| {
            events
                .iter()
                .position(|e| e["label"] == "discovering tests" && e["status"] == status)
                .expect("discovery activity")
        };
        assert!(activity("running") < totals[0].0 && totals[0].0 < activity("passed"));
        if let Some(first) = events.iter().position(|e| e["kind"] == "case") {
            assert!(activity("passed") < first, "{events:?}");
        }
        let calls = fs::read_to_string(fixture.path().join("cargo-calls")).unwrap();
        for line in calls.lines().filter(|line| line.contains("--list")) {
            assert!(
                line.contains("--doc"),
                "pooled binaries were listed serially: {line}"
            );
        }
    }
}

/// Discovering integration and library artifacts together must retain their
/// package identities and include both in the denominator.
#[test]
fn pooled_discovery_counts_workspace_library_and_integration_targets() {
    let fixture = Fixture::new();
    fixture.write("Cargo.toml", "[package]\nname=\"storyhook\"\nversion=\"0.0.0\"\nedition=\"2021\"\n[workspace]\nmembers=[\"auxiliary\"]\n");
    fs::create_dir_all(fixture.path().join("auxiliary/src")).unwrap();
    fs::create_dir_all(fixture.path().join("auxiliary/tests")).unwrap();
    fixture.write(
        "auxiliary/Cargo.toml",
        "[package]\nname=\"auxiliary-checks\"\nversion=\"0.0.0\"\nedition=\"2021\"\n",
    );
    fixture.write(
        "auxiliary/src/lib.rs",
        "#[test] fn unit_runs() {}\n#[test] #[ignore] fn ignored_unit() {}\n",
    );
    fixture.write("auxiliary/tests/lint.rs", "#[test] fn lint_runs() {}\n");
    let journal = fixture.path().join("progress.ndjson");
    let out = fixture
        .run_tests(&["--only-no-doc", "second", "lint", "auxiliary_checks"])
        .env("STORYHOOK_TEST_THREAD_BUDGET", "3")
        .env("STORYHOOK_GATE_PROGRESS", &journal)
        .output()
        .expect("running pooled workspace selection");
    assert!(out.status.success(), "{}", combined(&out));
    let progress = fs::read_to_string(journal).unwrap();
    assert!(progress.contains("\"total\":3"), "{progress}");
}

/// Python cases inject listing faults at the subprocess boundary, leaving the
/// production discovery and pool logic in charge of cancellation and refusal.
#[test]
fn pooled_discovery_rejects_incomplete_evidence_and_reaps_cancelled_listings() {
    let out = Command::new("python3")
        .arg(super::checkout().join("scripts/tests/test_test_discovery.py"))
        .output()
        .expect("running discovery regressions");
    assert!(out.status.success(), "{}", combined(&out));
}
