//! SH-899: explicit JSON input retains the legacy field-update contract.

use serde_json::Value;
use storyhook::cli::{Invocation, parse_invocation, split_global_flags};
use storyhook_test_support::TestEnv;

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_string()).collect()
}

fn parse(args: &[&str]) -> (bool, Invocation) {
    let (flags, rest) = split_global_flags(&argv(args)).unwrap();
    (flags.json, parse_invocation(&rest).unwrap())
}

#[test]
fn explicit_json_and_output_selection_are_independent() {
    for args in [
        vec![
            "--json",
            "set",
            "SH-1",
            "--input-json",
            r#"{"title":"new"}"#,
        ],
        vec![
            "set",
            "SH-1",
            "--input-json",
            r#"{"title":"new"}"#,
            "--json",
        ],
    ] {
        let (output, invocation) = parse(&args);
        assert!(output);
        assert!(
            matches!(invocation, Invocation::SetFields { json: Some(raw), .. }
                        if raw == r#"{"title":"new"}"#)
        );
    }
    let (output, _) = parse(&["set", "SH-1", "--input-json", r#"{"title":"new"}"#]);
    assert!(!output);
}

#[test]
fn explicit_input_protects_malformed_flag_shaped_and_empty_values() {
    for input in ["", "--json", "--quiet", "[]", "{broken", "null"] {
        let (output, invocation) = parse(&["set", "SH-1", "--input-json", input]);
        assert!(!output);
        assert!(
            matches!(invocation, Invocation::SetFields { json: Some(raw), .. } if raw == input)
        );
    }
}

#[test]
fn new_flag_spelling_in_a_field_value_remains_literal() {
    let (_, invocation) = parse(&[
        "set",
        "SH-1",
        "--title",
        "--input-json",
        "--priority",
        "high",
    ]);
    assert!(
        matches!(invocation, Invocation::SetFields { title: Some(title), json: None, .. }
                    if title == "--input-json")
    );
}

#[test]
fn duplicate_json_sources_fail_before_invocation_dispatch() {
    for args in [
        vec!["set", "SH-1", "--input-json", "{}", "--json", "{}"],
        vec!["set", "SH-1", "--json", "{}", "--input-json", ""],
        vec!["set", "SH-1", "--input-json", "{broken", "--json", "{}"],
        vec!["set", "SH-1", "--input-json", "{}", "--json", ""],
        vec!["set", "SH-1", "--json", "", "--input-json", "{}"],
        vec!["set", "SH-1", "--input-json", "{}", "--input-json", "{}"],
    ] {
        let result = split_global_flags(&argv(&args)).and_then(|(_, rest)| parse_invocation(&rest));
        assert!(result.is_err(), "conflicting inputs accepted: {args:?}");
        let diagnostic = result.unwrap_err().to_string();
        assert!(
            diagnostic.contains("--input-json") && diagnostic.contains("--json"),
            "{diagnostic}"
        );
    }
}

#[test]
fn legacy_json_parsing_and_last_input_behavior_are_unchanged() {
    let (output, invocation) = parse(&[
        "--json",
        "set",
        "SH-1",
        "--json",
        r#"{"title":"first"}"#,
        "--json",
        r#"{"title":"last"}"#,
    ]);
    assert!(output);
    assert!(
        matches!(invocation, Invocation::SetFields { json: Some(raw), .. }
                    if raw == r#"{"title":"last"}"#)
    );
    let (_, invocation) = parse(&["set", "SH-1", "--title", r#"{"title":"literal"}"#]);
    assert!(
        matches!(invocation, Invocation::SetFields { title: Some(title), json: None, .. }
                    if title == r#"{"title":"literal"}"#)
    );
}

#[test]
fn explicit_and_legacy_json_updates_have_the_same_output_and_effects() {
    let env = TestEnv::shared();
    let project = env.project().build();
    project.run(&["new", "before"]).success();
    let patch = r#"{"title":"after","priority":"high","complexity":"medium"}"#;
    let new = project
        .run(&["--json", "set", "SH-1", "--input-json", patch])
        .success();
    let legacy = project
        .run(&["--json", "set", "SH-1", "--json", patch])
        .success();
    assert_eq!(new.get_output().stdout, legacy.get_output().stdout);
    assert!(new.get_output().stderr.is_empty());
    let shown = project.run(&["show", "SH-1", "--json"]).success();
    let value: Value = serde_json::from_slice(&shown.get_output().stdout).unwrap();
    assert_eq!(value["story"]["story"]["title"], "after");
    assert_eq!(value["story"]["story"]["priority"], "high");
    assert_eq!(value["story"]["story"]["complexity_assessed"], true);
}

#[test]
fn invalid_explicit_json_is_atomic_including_other_requested_edits() {
    let env = TestEnv::shared();
    let project = env.project().build();
    project.run(&["new", "before"]).success();
    let before = project
        .run(&["show", "SH-1", "--json"])
        .success()
        .get_output()
        .stdout
        .clone();
    for input in [
        "",
        "{broken",
        "[]",
        "null",
        "--json",
        r#"{"title":"partial","unknown_field":1}"#,
    ] {
        project
            .run(&[
                "--json",
                "set",
                "SH-1",
                "--title",
                "must not land",
                "--input-json",
                input,
            ])
            .code(2);
        let after = project
            .run(&["show", "SH-1", "--json"])
            .success()
            .get_output()
            .stdout
            .clone();
        assert_eq!(before, after, "partial mutation for {input:?}");
    }
}

#[test]
fn conflicting_inputs_and_empty_objects_leave_stored_story_unchanged() {
    let env = TestEnv::shared();
    let project = env.project().build();
    project.run(&["new", "before"]).success();
    let before = project
        .run(&["show", "SH-1", "--json"])
        .success()
        .get_output()
        .stdout
        .clone();
    for args in [
        vec![
            "set",
            "SH-1",
            "--input-json",
            r#"{"title":"partial"}"#,
            "--json",
            "{}",
        ],
        vec![
            "set",
            "SH-1",
            "--json",
            "",
            "--input-json",
            r#"{"title":"partial"}"#,
        ],
        vec!["set", "SH-1", "--input-json", "{}"],
    ] {
        project.run(&args).code(2);
        let after = project
            .run(&["show", "SH-1", "--json"])
            .success()
            .get_output()
            .stdout
            .clone();
        assert_eq!(before, after, "partial mutation for {args:?}");
    }
}
