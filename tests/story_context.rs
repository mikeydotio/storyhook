use assert_cmd::Command;
use predicates::prelude::*;
use storyhook_test_support::{TestEnv, scratch_dir};

/// Every `story` this file runs is the one THIS build produced, in the shared
/// test environment's private `HOME`, XDG directories and store — so nothing
/// here can reach the developer's own storyhook state, with or without a
/// wrapper script supplying one.
fn story(dir: &std::path::Path) -> Command {
    TestEnv::shared().story(dir)
}

#[test]
fn context_generates_markdown() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Build API"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Write docs"])
        .assert()
        .success();
    story(dir.path())
        .args(["prioritize", "SH-1", "high"])
        .assert()
        .success();

    story(dir.path())
        .args(["context"])
        .assert()
        .success()
        .stdout(predicate::str::contains("# Project Status"))
        .stdout(predicate::str::contains("2 open"))
        .stdout(predicate::str::contains("Ready to Work"))
        .stdout(predicate::str::contains("SH-1"));
}

#[test]
fn context_shows_blocked_stories() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Blocked task"])
        .assert()
        .success();
    story(dir.path())
        .args(["block", "SH-1", "external API"])
        .assert()
        .success();

    story(dir.path())
        .args(["context"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Blocked"))
        .stdout(predicate::str::contains("external API"));
}

#[test]
fn context_json_format() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path()).args(["new", "A task"]).assert().success();

    story(dir.path())
        .args(["context", "--format", "json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"open\": 1"))
        .stdout(predicate::str::contains("\"ready_count\": 1"));
}

#[test]
fn context_empty_project() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["context"])
        .assert()
        .success()
        .stdout(predicate::str::contains("0 open"));
}

#[test]
fn context_shows_type_distribution() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Fix login crash", "--type", "bug"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Add dashboard", "--type", "normal"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Default-typed task"])
        .assert()
        .success();

    story(dir.path())
        .args(["context"])
        .assert()
        .success()
        .stdout(predicate::str::contains("## Type Distribution"))
        .stdout(predicate::str::contains("- bug: 1"))
        .stdout(predicate::str::contains("- normal: 2"))
        .stdout(predicate::str::contains("- Default:").not());
}

#[test]
fn context_json_includes_type_distribution() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Fix login crash", "--type", "bug"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Default-typed task"])
        .assert()
        .success();

    story(dir.path())
        .args(["context", "--format", "json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"by_type\""))
        .stdout(predicate::str::contains("\"bug\": 1"))
        .stdout(predicate::str::contains("\"normal\": 1"))
        .stdout(predicate::str::contains("\"Default\"").not());
}

/// Runs `story <args>` against `project` and returns raw stdout, asserting
/// success. Unlike this file's `story()` helper, callers reach `project` via
/// its own [`storyhook_test_support::Project::story`], so every command below
/// shares the one isolated environment a `TestEnv`-built fixture was
/// registered under — mixing it with the bare `story()` helper would run
/// commands against two different stores.
fn stdout_of(project: &storyhook_test_support::Project<'_>, args: &[&str]) -> String {
    let out = project
        .story()
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("running `story {}`: {e}", args.join(" ")));
    assert!(
        out.status.success(),
        "`story {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap_or_else(|e| panic!("`story {}` printed non-UTF-8: {e}", args.join(" ")))
}

/// The global `--json` flag must not change *what* `story context --format
/// json` emits (SH-66, the sibling of the `export --json` defect W0b fixed).
///
/// `context`'s JSON body already is the machine-readable result, so the
/// envelope has nothing to add: wrapping it as an escaped string inside
/// `.message` forced any consumer to parse it twice. The contract is
/// byte-equality with the un-flagged form.
#[test]
fn context_json_flag_emits_the_document_itself() {
    let source = storyhook_test_support::TestEnv::shared()
        .project()
        .prefix("API")
        .build();
    source
        .story()
        .args(["new", "A task worth reporting"])
        .assert()
        .success();

    let flagged = stdout_of(&source, &["context", "--format", "json", "--json"]);
    let document: serde_json::Value = serde_json::from_str(&flagged)
        .expect("`story context --format json --json` must print JSON");
    assert!(
        document.get("total_stories").is_some() && document.get("ready_stories").is_some(),
        "`story context --format json --json` must print the context document, not an \
         envelope wrapping it: {flagged}"
    );

    assert_eq!(
        stdout_of(&source, &["context", "--format", "json"]),
        flagged,
        "`story context --format json --json` and `story context --format json` must \
         emit the same bytes"
    );
}

/// `load-context` is the same invocation under its other name; the fix must
/// not be format-parse-path-specific.
#[test]
fn load_context_json_flag_emits_the_document_itself() {
    let source = storyhook_test_support::TestEnv::shared().project().build();
    source
        .story()
        .args(["new", "A task worth reporting"])
        .assert()
        .success();

    let flagged = stdout_of(&source, &["load-context", "--format", "json", "--json"]);
    let document: serde_json::Value = serde_json::from_str(&flagged)
        .expect("`story load-context --format json --json` must print JSON");
    assert!(
        document.get("total_stories").is_some(),
        "must print the context document, not an envelope wrapping it: {flagged}"
    );
}

/// `--quiet` suppresses *success chatter*, and the JSON form of `context` has
/// none: its whole output is the report a caller asked for. Emitting nothing
/// for `story context --format json --quiet > report.json` writes an empty
/// report — the same silent-data-loss shape the double-encoding had. Mirrors
/// `export_is_not_suppressed_by_quiet`, the decision the export fix made for
/// the same question.
#[test]
fn context_json_is_not_suppressed_by_quiet() {
    let source = storyhook_test_support::TestEnv::shared().project().build();
    source
        .story()
        .args(["new", "A task worth reporting"])
        .assert()
        .success();

    let expected = stdout_of(&source, &["context", "--format", "json"]);
    for flags in [
        vec!["context", "--format", "json", "--quiet"],
        vec!["context", "--format", "json", "--quiet", "--json"],
    ] {
        assert_eq!(
            expected,
            stdout_of(&source, &flags),
            "`story {}` must still emit the context document",
            flags.join(" ")
        );
    }
}

/// The markdown form is unaffected: `--json` still wraps it as an ordinary
/// `.message` string (it isn't JSON to begin with), and `--quiet` still
/// suppresses it, exactly as before this fix.
#[test]
fn context_markdown_is_still_suppressed_by_quiet_and_wrapped_by_json() {
    let source = storyhook_test_support::TestEnv::shared().project().build();
    source
        .story()
        .args(["new", "A task worth reporting"])
        .assert()
        .success();

    let quiet = source
        .story()
        .args(["context", "--quiet"])
        .output()
        .expect("running context --quiet");
    assert!(quiet.status.success());
    assert!(
        quiet.stdout.is_empty(),
        "`story context --quiet` (markdown) must still emit nothing"
    );

    let wrapped = stdout_of(&source, &["context", "--json"]);
    let parsed: serde_json::Value =
        serde_json::from_str(&wrapped).expect("`story context --json` must print JSON");
    assert!(
        parsed.get("message").and_then(|m| m.as_str()).is_some(),
        "`story context --json` (markdown form) must still wrap its text in `.message`: {wrapped}"
    );
}
