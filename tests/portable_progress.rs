//! SH-777: foreign gates report progress through the daemon's portable writer.
//!
//! The verifier hands every project gate `STORYHOOK_GATE_PROGRESS_WRITER`
//! beside the SH-665 receipt writer. These cases run the materialized
//! production bundle against a real foreign repository: the writer reaches the
//! gate through `merge-watch.sh`, each verb becomes exactly one checklist line,
//! a call it cannot record safely writes nothing, and nothing it writes can
//! touch the verifier's own recovery evidence.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::{Value, json};
use storyhook::service::gate_progress::{self, ItemStatus};
use storyhook_test_support::{ChildGuard, STORY_COMMAND_DEADLINE, load_grace};

#[path = "support/foreign_repo.rs"]
mod foreign_repo;

use foreign_repo::{ForeignRepo, output, success};

/// The bundled writer's name, the one `merge-watch.sh` supplies.
const WRITER: &str = "gate-progress-writer.py";

/// The writer's documented bound on one leg, in UTF-8 bytes.
const LEG_LIMIT: usize = 256;

/// The lines the verifier has written by the time a gate starts: its run
/// record, a passed merge preflight, and its own running release gate.
fn verifier_prefix() -> String {
    [
        json!({"kind":"run", "generation":7, "attempt_id":"attempt", "at":"2026-01-01T00:00:00Z"}),
        json!({"kind":"item", "path":"merge preflight", "status":"passed", "at":"2026-01-01T00:00:00Z"}),
        json!({"kind":"item", "path":"release gate", "status":"running", "at":"2026-01-01T00:00:00Z"}),
    ]
    .iter()
    .map(|line| format!("{line}\n"))
    .collect()
}

/// The foreign project's journal, prepared the way the verifier prepares it.
fn journal(fixture: &ForeignRepo) -> PathBuf {
    let journal = fixture.root.path().join("progress.ndjson");
    fs::write(&journal, verifier_prefix()).unwrap();
    journal
}

/// One direct call of the bundled writer, with `journal` as the verifier's.
fn write(fixture: &ForeignRepo, journal: Option<&Path>, args: &[OsString]) -> Output {
    let mut command = fixture.scrubbed(fixture.bundle.join(WRITER), &fixture.repo);
    if let Some(journal) = journal {
        command.env("STORYHOOK_GATE_PROGRESS", journal);
    }
    output(command.args(args))
}

fn args(words: &[&str]) -> Vec<OsString> {
    words.iter().map(OsString::from).collect()
}

/// Every complete journal line after the verifier's prefix, parsed.
fn appended(journal: &Path) -> Vec<Value> {
    let text = fs::read_to_string(journal).unwrap();
    let rest = text
        .strip_prefix(&verifier_prefix())
        .expect("the writer only appends");
    assert!(rest.is_empty() || rest.ends_with('\n'), "{rest:?}");
    rest.lines()
        .map(|line| serde_json::from_str(line).expect("every line is one JSON object"))
        .collect()
}

fn is_second_stamp(value: &Value) -> bool {
    let stamp = value.as_str().unwrap_or_default();
    stamp.len() == "2026-01-01T00:00:00Z".len()
        && chrono::NaiveDateTime::parse_from_str(stamp, "%Y-%m-%dT%H:%M:%SZ").is_ok()
}

#[test]
fn merge_watch_hands_a_foreign_gate_the_bundled_writer_and_its_rows_fold_into_the_checklist() {
    let fixture = ForeignRepo::new();
    let journal = journal(&fixture);
    let writer = fixture.bundle.join(WRITER);
    let result = output(
        fixture
            .speculative_run(
                r#"
test "$STORYHOOK_GATE_PROGRESS_WRITER" = "$1"
test ! -e scripts
"$STORYHOOK_GATE_PROGRESS_WRITER" leg start build
"$STORYHOOK_GATE_PROGRESS_WRITER" leg pass build
"$STORYHOOK_GATE_PROGRESS_WRITER" leg start unit/Parser
"$STORYHOOK_GATE_PROGRESS_WRITER" case unit/Parser pass
"$STORYHOOK_GATE_PROGRESS_WRITER" case unit/Parser pass
"$STORYHOOK_GATE_PROGRESS_WRITER" case unit/Parser fail
"$STORYHOOK_GATE_PROGRESS_WRITER" leg fail unit/Parser
"$STORYHOOK_GATE_PROGRESS_WRITER" leg skip ui
"#,
                &[writer.to_str().unwrap()],
            )
            .env("STORYHOOK_GATE_PROGRESS", &journal)
            .env(
                "STORYHOOK_GATE_PROGRESS_WRITER",
                "/inherited/invalid/writer",
            ),
    );
    success(result);
    fixture.assert_restored();

    let progress = gate_progress::fold(&fs::read_to_string(&journal).unwrap());
    let labels: Vec<&str> = progress
        .items
        .iter()
        .map(|item| item.label.as_str())
        .collect();
    assert_eq!(labels, ["merge preflight", "release gate"], "{progress:?}");
    assert!(progress.reached_verification_gate());
    let gate = &progress.items[1];
    assert_eq!(
        gate.status,
        ItemStatus::Running,
        "the verifier owns this row"
    );
    let legs: Vec<(&str, ItemStatus)> = gate
        .children
        .iter()
        .map(|leg| (leg.label.as_str(), leg.effective_status()))
        .collect();
    assert_eq!(
        legs,
        [
            ("build", ItemStatus::Passed),
            ("unit", ItemStatus::Failed),
            ("ui", ItemStatus::Skipped)
        ]
    );
    let parser = &gate.children[1].children[0];
    assert_eq!(parser.label, "Parser");
    assert_eq!((parser.counts.passed, parser.counts.failed), (2, 1));
    // Once the gate reports legs, the checklist shows them instead of
    // "detailed counts unavailable".
    let body = gate_progress::render(
        &gate_progress::VerificationProgressView::Running {
            progress: &progress,
            elapsed_seconds: None,
            seconds_since_structured_progress: None,
            output: &storyhook::service::gate_output::OutputObservation::NotCapturing,
        },
        "2026-01-01T00:00:00Z",
    );
    assert!(!body.contains("detailed counts unavailable"), "{body}");
    assert!(body.contains("- [x] build"), "{body}");
}

#[test]
fn each_verb_appends_exactly_its_one_checklist_line() {
    let fixture = ForeignRepo::new();
    let journal = journal(&fixture);
    let cases: [(&[&str], Value); 6] = [
        (
            &["leg", "start", "build"],
            json!({"kind":"item", "path":"release gate/build", "status":"running"}),
        ),
        (
            &["leg", "pass", "build"],
            json!({"kind":"item", "path":"release gate/build", "status":"passed"}),
        ),
        (
            &["leg", "fail", "unit/Parser"],
            json!({"kind":"item", "path":"release gate/unit/Parser", "status":"failed"}),
        ),
        (
            &["leg", "skip", "ui"],
            json!({"kind":"item", "path":"release gate/ui", "status":"skipped"}),
        ),
        (
            &["case", "unit", "pass"],
            json!({"kind":"case", "path":"release gate/unit", "outcome":"pass"}),
        ),
        (
            &["case", "unit", "fail"],
            json!({"kind":"case", "path":"release gate/unit", "outcome":"fail"}),
        ),
    ];
    for (index, (words, expected)) in cases.iter().enumerate() {
        let result = write(&fixture, Some(&journal), &args(words));
        assert_eq!(result.status.code(), Some(0), "{words:?}: {result:?}");
        assert!(
            result.stdout.is_empty() && result.stderr.is_empty(),
            "{result:?}"
        );
        let mut line = appended(&journal).remove(index);
        if expected["kind"] == "item" {
            assert!(is_second_stamp(&line["at"]), "{line}");
            line.as_object_mut().unwrap().remove("at");
        }
        assert_eq!(&line, expected, "{words:?}");
    }
    // Quotes, backslashes and non-ASCII letters survive as JSON escapes.
    let name = "quo\"te\\back ünï/段";
    success(write(
        &fixture,
        Some(&journal),
        &args(&["leg", "start", name]),
    ));
    let line = appended(&journal).pop().unwrap();
    assert_eq!(line["path"], format!("release gate/{name}"));
    let raw = fs::read(&journal).unwrap();
    assert!(
        raw.is_ascii(),
        "the journal stays ASCII whatever a leg is named"
    );
}

#[test]
fn a_call_the_writer_cannot_record_safely_writes_nothing() {
    let fixture = ForeignRepo::new();
    let journal = journal(&fixture);
    let before = fs::read(&journal).unwrap();
    let long = "a".repeat(LEG_LIMIT + 1);
    let wide = "é".repeat(LEG_LIMIT / 2 + 1);
    let refused: Vec<(&str, Vec<OsString>)> = vec![
        ("no verb", args(&[])),
        ("short leg call", args(&["leg", "start"])),
        ("extra word", args(&["leg", "start", "build", "now"])),
        ("unknown verb", args(&["note", "start", "build"])),
        ("unknown status", args(&["leg", "begin", "build"])),
        ("unknown outcome", args(&["case", "unit", "skip"])),
        ("empty leg", args(&["leg", "start", ""])),
        ("leading slash", args(&["leg", "start", "/build"])),
        ("trailing slash", args(&["leg", "start", "build/"])),
        ("double slash", args(&["case", "unit//Parser", "pass"])),
        ("tab", args(&["leg", "start", "a\tb"])),
        ("newline", args(&["leg", "start", "a\nb"])),
        ("escape", args(&["leg", "start", "a\u{1b}[31mb"])),
        ("delete", args(&["leg", "start", "a\u{7f}b"])),
        ("too long", args(&["leg", "start", &long])),
        ("too long in bytes", args(&["leg", "start", &wide])),
        (
            "invalid UTF-8",
            vec![
                OsString::from("leg"),
                OsString::from("start"),
                OsString::from_vec(b"a\xffb".to_vec()),
            ],
        ),
    ];
    for (why, call) in &refused {
        for journal in [Some(journal.as_path()), None] {
            let result = write(&fixture, journal, call);
            assert_eq!(result.status.code(), Some(2), "{why}: {result:?}");
            assert!(
                String::from_utf8_lossy(&result.stderr).starts_with("gate-progress-writer: "),
                "{why}: {result:?}"
            );
        }
        assert_eq!(fs::read(&journal).unwrap(), before, "{why}");
    }
    // The limit itself is accepted, in bytes as well as in characters.
    for leg in ["a".repeat(LEG_LIMIT), "é".repeat(LEG_LIMIT / 2)] {
        success(write(
            &fixture,
            Some(&journal),
            &args(&["leg", "start", &leg]),
        ));
    }
    assert_eq!(appended(&journal).len(), 2);
}

#[test]
fn the_writer_is_inert_outside_verification_and_loud_when_its_journal_is_unusable() {
    let fixture = ForeignRepo::new();
    let call = args(&["leg", "start", "build"]);
    let entries = |dir: &Path| fs::read_dir(dir).unwrap().count();
    let before = entries(&fixture.repo);
    let result = write(&fixture, None, &call);
    assert_eq!(result.status.code(), Some(0), "{result:?}");
    assert!(result.stderr.is_empty(), "{result:?}");
    assert_eq!(
        entries(&fixture.repo),
        before,
        "an inert call creates nothing"
    );

    let missing = fixture.root.path().join("never prepared.ndjson");
    let directory = fixture.root.path().join("a directory");
    fs::create_dir(&directory).unwrap();
    let read_only = fixture.root.path().join("read only.ndjson");
    fs::write(&read_only, "").unwrap();
    fs::set_permissions(&read_only, fs::Permissions::from_mode(0o444)).unwrap();
    for journal in [&missing, &directory, &read_only] {
        let result = write(&fixture, Some(journal), &call);
        assert_eq!(result.status.code(), Some(1), "{journal:?}: {result:?}");
        assert!(
            String::from_utf8_lossy(&result.stderr).contains(&journal.display().to_string()),
            "the refusal names the journal: {result:?}"
        );
    }
    assert!(!missing.exists(), "the writer never creates a journal");
    assert_eq!(fs::read(&read_only).unwrap(), b"");
}

#[test]
fn legs_reported_in_parallel_append_whole_lines() {
    const WRITERS: usize = 20;
    let fixture = ForeignRepo::new();
    let journal = journal(&fixture);
    // Long legs, so an interleaved write could not hide inside a short line.
    let legs: Vec<String> = (0..WRITERS)
        .map(|n| format!("parallel-{n:02}-{}", "x".repeat(200)))
        .collect();
    let mut children: Vec<ChildGuard> = legs
        .iter()
        .map(|leg| {
            let mut command = fixture.scrubbed(fixture.bundle.join(WRITER), &fixture.repo);
            command
                .env("STORYHOOK_GATE_PROGRESS", &journal)
                .args(["leg", "start", leg]);
            ChildGuard::spawn_with_output(&mut command).unwrap()
        })
        .collect();
    for child in &mut children {
        let result = child
            .wait_with_output_within(load_grace::graced_now(STORY_COMMAND_DEADLINE), || {
                "a parallel writer did not finish".into()
            });
        assert_eq!(result.status.code(), Some(0), "{result:?}");
    }
    let lines = appended(&journal);
    assert_eq!(lines.len(), WRITERS);
    let written: BTreeSet<String> = lines
        .iter()
        .map(|line| line["path"].as_str().unwrap().to_owned())
        .collect();
    let expected: BTreeSet<String> = legs
        .iter()
        .map(|leg| format!("release gate/{leg}"))
        .collect();
    assert_eq!(written, expected);
}

#[test]
fn no_leg_name_reaches_the_verifiers_own_rows() {
    let fixture = ForeignRepo::new();
    let journal = journal(&fixture);
    for leg in ["merge preflight", "release gate", "release gate/build"] {
        for status in ["start", "fail"] {
            success(write(
                &fixture,
                Some(&journal),
                &args(&["leg", status, leg]),
            ));
        }
        success(write(
            &fixture,
            Some(&journal),
            &args(&["case", leg, "fail"]),
        ));
    }
    let progress = gate_progress::fold(&fs::read_to_string(&journal).unwrap());
    assert!(
        progress.reached_verification_gate(),
        "writer rows can neither forge nor damage recovery evidence"
    );
    let labels: Vec<&str> = progress
        .items
        .iter()
        .map(|item| item.label.as_str())
        .collect();
    assert_eq!(labels, ["merge preflight", "release gate"]);
    assert_eq!(progress.items[0].status, ItemStatus::Passed);
    assert_eq!(progress.items[1].status, ItemStatus::Running);
}
