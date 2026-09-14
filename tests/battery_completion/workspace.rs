//! Workspace integration targets must survive the gate's bare-name selection.

use super::{Fixture, combined};
use std::fs;

#[test]
fn workspace_integration_and_library_targets_run_in_their_own_package() {
    let fixture = Fixture::new();
    fixture.write(
        "Cargo.toml",
        "[package]\nname = \"storyhook\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[workspace]\nmembers = [\"auxiliary\"]\n",
    );
    for dir in ["auxiliary/src", "auxiliary/tests"] {
        fs::create_dir_all(fixture.path().join(dir)).expect("creating the workspace member");
    }
    fixture.write(
        "auxiliary/Cargo.toml",
        "[package]\nname = \"auxiliary-checks\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    );
    fixture.write(
        "auxiliary/src/lib.rs",
        "#[test]\nfn auxiliary_unit_runs() {}\n",
    );
    fixture.write(
        "auxiliary/tests/lint.rs",
        "#[test]\nfn auxiliary_lint_runs() {}\n",
    );
    fixture.git(&["add", "."]);

    let out = fixture
        .run_tests(&[
            "--only-no-doc",
            "second",
            "lint",
            "auxiliary_checks",
            "--",
            "--test-threads=1",
        ])
        .output()
        .expect("running the workspace selection");
    let output = combined(&out);
    assert!(out.status.success(), "{output}");
    for case in [
        "second_passes",
        "auxiliary_lint_runs",
        "auxiliary_unit_runs",
    ] {
        assert!(
            output.contains(&format!("test {case} ... ok")),
            "selected target did not run {case}\n{output}"
        );
    }
    assert!(
        !output.contains("test first_fails"),
        "selection ran an unselected root integration test\n{output}"
    );
}

#[test]
fn ambiguous_workspace_names_are_refused_before_discovery_or_execution() {
    for second_kind in ["lib", "test"] {
        let fixture = Fixture::new();
        fixture.fake_cargo(&format!(
            r#"#!/bin/sh
case "$1" in
(metadata)
    printf '%s\n' '{{"packages":[{{"name":"one","targets":[{{"kind":["test"],"name":"lint"}}]}},{{"name":"two","targets":[{{"kind":["{second_kind}"],"name":"lint"}}]}}]}}'
    ;;
(test)
    touch unexpected-test-call
    ;;
esac
"#
        ));
        let out = fixture
            .run_tests(&["--only-no-doc", "lint"])
            .output()
            .expect("trying the ambiguous selection");
        let output = combined(&out);
        assert!(!out.status.success(), "{output}");
        assert!(
            output.contains("ambiguous workspace target")
                && output.contains("lint")
                && output.contains("one")
                && output.contains("two"),
            "refusal must identify every matching owner\n{output}"
        );
        assert!(
            !fixture.path().join("unexpected-test-call").exists(),
            "ambiguous selection started Cargo test\n{output}"
        );
    }
}
