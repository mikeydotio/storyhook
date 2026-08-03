use assert_cmd::Command;
use predicates::prelude::*;
use storyhook_test_support::{TestEnv, scratch_dir};

/// Every `story` invocation in this file runs in the shared test
/// environment's private HOME/XDG directories, so nothing here can reach the
/// developer's own storyhook state.
fn story(dir: &std::path::Path) -> Command {
    TestEnv::shared().story(dir)
}

#[test]
fn export_and_import_roundtrip() {
    let dir = TestEnv::shared().project().build();
    story(dir.path())
        .args(["new", "Task one"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Task two"])
        .assert()
        .success();
    story(dir.path())
        .args(["prioritize", "SH-1", "high"])
        .assert()
        .success();
    story(dir.path())
        .args(["label", "SH-1", "backend"])
        .assert()
        .success();
    story(dir.path())
        .args(["move", "SH-2", "done"])
        .assert()
        .success();

    // Export
    let output = story(dir.path()).args(["export"]).assert().success();
    let export_json = String::from_utf8(output.get_output().stdout.clone()).unwrap();

    // Import into new directory
    let dir2 = scratch_dir();
    let export_file = dir2.path().join("export.json");
    std::fs::write(&export_file, &export_json).unwrap();

    story(dir2.path())
        .args(["import-project", export_file.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("imported project with 2 stories"));

    // Verify open story preserved
    story(dir2.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Task one"))
        .stdout(predicate::str::contains("priority: high"))
        .stdout(predicate::str::contains("labels: backend"));

    // Verify archived story preserved
    story(dir2.path())
        .args(["show", "SH-2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Task two"))
        .stdout(predicate::str::contains("done"));

    // Verify next ID works correctly
    story(dir2.path())
        .args(["new", "Task three"])
        .assert()
        .success()
        .stdout(predicate::str::contains("SH-3"));
}

#[test]
fn export_import_roundtrips_description() {
    let dir = TestEnv::shared().project().build();
    story(dir.path())
        .args([
            "new",
            "Described story",
            "--description",
            "What this story is about",
        ])
        .assert()
        .success();

    let output = story(dir.path()).args(["export"]).assert().success();
    let export_json = String::from_utf8(output.get_output().stdout.clone()).unwrap();

    let dir2 = scratch_dir();
    let export_file = dir2.path().join("export.json");
    std::fs::write(&export_file, &export_json).unwrap();

    story(dir2.path())
        .args(["import-project", export_file.to_str().unwrap()])
        .assert()
        .success();

    story(dir2.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "description: What this story is about",
        ));
}

#[test]
fn export_json_output() {
    let dir = TestEnv::shared().project().build();
    story(dir.path())
        .args(["new", "A story"])
        .assert()
        .success();

    let output = story(dir.path()).args(["export"]).assert().success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    // Export output is raw JSON (not wrapped in envelope)
    assert!(stdout.contains("\"schema\""));
    assert!(stdout.contains("\"stories\""));
}

/// The global `--json` flag must not change *what* `story export` emits.
///
/// `export`'s response body already is the machine-readable result, so the
/// envelope has nothing to add: wrapping it as an escaped string inside
/// `.message` made the only documented consumer — `story import-project` —
/// reject its own export. The contract is byte-equality with the un-flagged
/// form, proved by feeding the flagged output straight back in.
#[test]
fn export_json_flag_emits_the_document_itself() {
    let env = TestEnv::shared();
    let source = env.project().prefix("API").build();
    story(source.path())
        .args(["new", "A story worth backing up"])
        .assert()
        .success();

    let out = story(source.path())
        .args(["export", "--json"])
        .output()
        .expect("running export --json");
    assert!(
        out.status.success(),
        "`story export --json` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let flagged = String::from_utf8(out.stdout).expect("the export document must be UTF-8");

    let document: serde_json::Value =
        serde_json::from_str(&flagged).expect("`story export --json` must print JSON");
    assert!(
        document.get("schema").is_some() && document.get("stories").is_some(),
        "`story export --json` must print the export document, not an envelope \
         wrapping it: {flagged}"
    );

    assert_eq!(
        export_of(source.path()),
        flagged,
        "`story export --json` and `story export` must emit the same bytes"
    );

    // The failure this test exists to prevent: the flagged form was not
    // importable at all (exit 5, "missing field `schema`").
    assert_eq!(
        flagged,
        reimport(&flagged),
        "the `--json` export document must round-trip through `story import-project`"
    );
}

/// `--quiet` suppresses *success chatter*, and `export` has none: its whole
/// output is the backup a caller asked for. Emitting nothing for
/// `story export --quiet > backup.json` writes an empty backup, which is the
/// same silent-data-loss shape as the double-encoding this pins the fix for.
#[test]
fn export_is_not_suppressed_by_quiet() {
    let env = TestEnv::shared();
    let source = env.project().build();
    story(source.path())
        .args(["new", "A story worth backing up"])
        .assert()
        .success();

    let expected = export_of(source.path());
    for flags in [
        vec!["export", "--quiet"],
        vec!["export", "--quiet", "--json"],
    ] {
        let out = story(source.path())
            .args(&flags)
            .output()
            .expect("running export");
        assert!(out.status.success(), "`story {}` failed", flags.join(" "));
        assert_eq!(
            expected,
            String::from_utf8(out.stdout).expect("the export document must be UTF-8"),
            "`story {}` must still emit the export document",
            flags.join(" ")
        );
    }
}

#[test]
fn export_preserves_custom_prefix() {
    let dir = TestEnv::shared().project().prefix("API").build();
    story(dir.path())
        .args(["new", "Endpoint"])
        .assert()
        .success();

    let output = story(dir.path()).args(["export"]).assert().success();
    let export_json = String::from_utf8(output.get_output().stdout.clone()).unwrap();

    let dir2 = scratch_dir();
    let export_file = dir2.path().join("export.json");
    std::fs::write(&export_file, &export_json).unwrap();

    story(dir2.path())
        .args(["import-project", export_file.to_str().unwrap()])
        .assert()
        .success();

    // New stories should use the imported prefix
    story(dir2.path())
        .args(["new", "Another endpoint"])
        .assert()
        .success()
        .stdout(predicate::str::contains("API-2"));
}

#[test]
fn export_and_import_roundtrip_with_types() {
    let dir = TestEnv::shared().project().build();

    // Define a custom type (not one of the defaults)
    story(dir.path())
        .args(["type", "add", "hotfix"])
        .assert()
        .success();

    // Create a story with that type
    story(dir.path())
        .args(["new", "Fix crash on login", "--type", "hotfix"])
        .assert()
        .success();

    // Verify the type is set on the story
    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("type: hotfix"));

    // Export
    let output = story(dir.path()).args(["export"]).assert().success();
    let export_json = String::from_utf8(output.get_output().stdout.clone()).unwrap();

    // Verify the export JSON contains the types field
    assert!(export_json.contains("\"types\""));
    assert!(export_json.contains("\"hotfix\""));

    // Import into a new directory
    let dir2 = scratch_dir();
    let export_file = dir2.path().join("export.json");
    std::fs::write(&export_file, &export_json).unwrap();

    story(dir2.path())
        .args(["import-project", export_file.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("imported project with 1 stories"));

    // Verify the story has the correct type in the imported project
    story(dir2.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Fix crash on login"))
        .stdout(predicate::str::contains("type: hotfix"));

    // Verify types.toml was restored with the custom type:
    // doctor should not flag "hotfix" as unknown
    story(dir2.path()).args(["doctor"]).assert().success();
}

/// Export → import into a fresh project → export again must produce **byte-
/// identical** JSON, and must stay identical when the round-trip is repeated.
///
/// This is the oracle W3's legacy importer is measured against. `story migrate`
/// has to move a `.storyhook/` directory into the global store without losing
/// anything, and "nothing was lost" is only checkable against a representation
/// that survives a round-trip unchanged. If this test is red, the export
/// document is not a faithful description of a project and cannot be used to
/// verify a migration.
///
/// The fixture carries the things a round-trip most easily drops: a custom id
/// prefix, a custom state and type (which live in separate config files, not in
/// the story records), a member, relations in both directions, a comment, an
/// assignee, an archived story (which lives in SQLite rather than in a JSONL
/// file) and a soft-deleted one.
///
/// Byte equality is asserted directly, with no redaction. That is not an
/// accident of the fixture: every timestamp in the document comes from the
/// stored event log, and import replays those events verbatim rather than
/// re-stamping them. A future change that makes this test need a timestamp
/// filter has changed that property, and the failure is the point.
#[test]
fn export_import_export_is_byte_identical() {
    let env = TestEnv::shared();
    let source = env.project().prefix("API").build();

    story(source.path())
        .args(["member", "add", "Ada Lovelace <ada@example.com>"])
        .assert()
        .success();
    story(source.path())
        .args([
            "type",
            "add",
            "spike",
            "--description",
            "A timeboxed investigation",
        ])
        .assert()
        .success();
    story(source.path())
        .args([
            "state",
            "add",
            "review",
            "--super",
            "OPEN",
            "--description",
            "Awaiting code review",
        ])
        .assert()
        .success();

    story(source.path())
        .args([
            "new",
            "Parent story",
            "--type",
            "normal",
            "--priority",
            "high",
            "--labels",
            "backend,api",
        ])
        .assert()
        .success();
    story(source.path())
        .args(["new", "Child story", "--type", "spike", "--priority", "low"])
        .assert()
        .success();
    story(source.path())
        .args(["new", "Finished story", "--type", "bug"])
        .assert()
        .success();
    story(source.path())
        .args(["new", "Abandoned story", "--type", "chore"])
        .assert()
        .success();

    story(source.path())
        .args(["relate", "API-1", "parent-of", "API-2"])
        .assert()
        .success();
    story(source.path())
        .args(["relate", "API-1", "blocks", "API-3"])
        .assert()
        .success();
    story(source.path())
        .args(["comment", "API-1", "Settled the shape of the trait."])
        .assert()
        .success();
    story(source.path())
        .args(["assign", "API-1", "ada-lovelace"])
        .assert()
        .success();
    story(source.path())
        .args(["move", "API-2", "review"])
        .assert()
        .success();
    // Archived: moves out of the JSONL directory and into the archive database.
    story(source.path())
        .args(["move", "API-3", "done"])
        .assert()
        .success();
    story(source.path())
        .args(["delete", "API-4", "superseded by the global store"])
        .assert()
        .success();

    let first = export_of(source.path());
    assert!(
        first.contains("\"spike\"") && first.contains("\"review\"") && first.contains("API-3"),
        "fixture: the export must actually carry the custom type, the custom state \
         and the archived story, or this test proves nothing: {first}"
    );

    let round_one = reimport(&first);
    assert_eq!(
        first, round_one,
        "one export/import round trip must be lossless and byte-identical"
    );

    // Twice, because a round trip that is merely *stable* after the first pass
    // can still have dropped something on that first pass.
    let round_two = reimport(&round_one);
    assert_eq!(
        first, round_two,
        "the round trip must stay byte-identical when repeated"
    );
}

/// Imports `document` into a brand-new project and exports it again.
fn reimport(document: &str) -> String {
    let dir = scratch_dir();
    let file = dir.path().join("export.json");
    std::fs::write(&file, document).expect("writing the export document");
    story(dir.path())
        .args(["import-project", file.to_str().unwrap()])
        .assert()
        .success();
    export_of(dir.path())
}

/// The `story export` document for the project at `root`.
fn export_of(root: &std::path::Path) -> String {
    let out = story(root)
        .args(["export"])
        .output()
        .expect("running export");
    assert!(
        out.status.success(),
        "`story export` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("the export document must be UTF-8")
}
