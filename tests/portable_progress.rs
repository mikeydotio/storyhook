//! SH-777: foreign gates report progress through the daemon's portable writer.
//!
//! The verifier hands every project gate `STORYHOOK_GATE_PROGRESS_WRITER`
//! beside the SH-665 receipt writer. These cases run the materialized
//! production bundle against a real foreign repository: the writer reaches the
//! gate through `merge-watch.sh`, each verb becomes exactly one checklist line,
//! a call it cannot record safely writes nothing, and nothing it writes can
//! touch the verifier's own recovery evidence. A gate that reports nothing
//! but prints keeps the verifier status quiet, as a real run shows. Under
//! the real gate lock, writer lines renew the silence ceiling and output
//! alone does not (SH-536, SH-713: the kill rule stays journal growth).

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{self, File, FileTimes};
use std::io::Write;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use serde_json::{Value, json};
use storyhook::daemon::verification::{VerificationActivity, journal_path};
use storyhook::daemon::verification_progress::PUBLISH_INTERVAL;
use storyhook::service::gate_progress::{self, ItemStatus};
use storyhook::service::{Clock, NewStoryInput, StoryService, VerificationQueue};
use storyhook_test_support::{ChildGuard, STORY_COMMAND_DEADLINE, ServiceFixture, load_grace};

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
    // The help topic states the same limit the writer enforces.
    let help = storyhook::help_topics::get_help_topic("project-settings").unwrap();
    let help = help.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        help.contains(&format!("longer than {LEG_LIMIT} bytes")),
        "{help}"
    );
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

/// How often a wait on the running gate looks again.
const GATE_POLL: Duration = Duration::from_millis(100);

/// Patience for the bundled verifier to reach its gate and for the gate to
/// print: `verify-pr.sh` re-execs under the gate lock and the lifecycle owner
/// first. Patience, never proof: it is graced by contention (SH-806).
const GATE_START_PATIENCE: Duration = Duration::from_secs(60);

/// How far past the publisher interval the journal is made to look quiet.
const PAST_THE_INTERVAL: i64 = 5;

/// A second-precision RFC3339 stamp, the daemon clock's own grid.
fn stamp(at: chrono::DateTime<chrono::Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// SH-777 acceptance 3: a real uninstrumented foreign gate, run by the
/// bundled `verify-pr.sh`, prints while its journal stays quiet for longer
/// than the publisher interval, and the verifier status raises no warning.
/// The gate prints until told to stop, then stays alive and silent, so the
/// log is still when the status reads it. Only the journal's modification
/// time is moved: that is the one input the old status judged by, and moving
/// it back is how "quiet for longer than the interval" is shown without a
/// sixty-second test.
#[test]
fn an_uninstrumented_foreign_gate_that_prints_needs_no_attention() {
    let fixture = ForeignRepo::new();
    let service = ServiceFixture::new();
    let story = StoryService::new(&service.ctx())
        .create(&NewStoryInput {
            title: "Foreign gate output".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    StoryService::new(&service.ctx())
        .set_state(&story, "verifying", None, None, None)
        .unwrap();
    let candidate = VerificationQueue::new(service.store())
        .ordered_for(service.project())
        .unwrap()
        .remove(0);
    // Acquired a moment in the past, so the capture the verifier registers
    // with its own clock can never precede the attempt.
    let started = stamp(chrono::Utc::now() - chrono::Duration::seconds(PAST_THE_INTERVAL));
    let activity = VerificationActivity::new();
    let _guard = activity.acquire(&candidate, started.clone());
    let attempt = activity.active_for(service.project()).unwrap().attempt_id;
    let journal = journal_path(service.env(), &candidate);
    fs::create_dir_all(journal.parent().unwrap()).unwrap();
    fs::write(
        &journal,
        format!(
            "{}\n",
            json!({"kind":"run", "generation":candidate.verifying_generation.unwrap().get(), "attempt_id":attempt, "at":started})
        ),
    )
    .unwrap();

    let quiet = fixture.root.path().join("quiet");
    let release = fixture.root.path().join("release");
    let mut command = fixture.command("verify-pr.sh", &fixture.repo);
    command
        .env("STORYHOOK_GATE_PROGRESS", &journal)
        .env("STORYHOOK_VERIFICATION_ATTEMPT", &attempt)
        .env("STORYHOOK_VERIFIER_CLEANUP_GRACE_MS", "8000")
        .args([
            "--run-gate",
            "1",
            &fixture.tree,
            &fixture.base,
            &fixture.head,
            fixture.poller.to_str().unwrap(),
            "--",
            "bash",
            "-eu",
            "-c",
            r#"
n=0
while [ ! -e "$1" ]; do n=$((n + 1)); echo "compiling unit $n"; sleep 0.1; done
while [ ! -e "$2" ]; do sleep 0.1; done
"#,
            "foreign-gate",
            quiet.to_str().unwrap(),
            release.to_str().unwrap(),
        ]);
    let mut gate = ChildGuard::spawn_with_output(&mut command).unwrap();

    // The verifier registered its capture and the gate is printing into it.
    let log = load_grace::wait_for(
        load_grace::Patience::new(GATE_START_PATIENCE),
        GATE_POLL,
        || {
            format!(
                "the gate never printed: {}",
                fs::read_to_string(&journal).unwrap_or_default()
            )
        },
        || {
            let progress = gate_progress::fold(&fs::read_to_string(&journal).ok()?);
            let log = progress.output?.path;
            (fs::read_to_string(&log).ok()?.contains("compiling unit 2")).then_some(log)
        },
    );
    fs::write(&quiet, "").unwrap();
    // Still means two looks one poll apart see the same length.
    let mut previous = None;
    load_grace::wait_for(
        load_grace::Patience::new(GATE_START_PATIENCE),
        GATE_POLL,
        || "the gate never went quiet".into(),
        || {
            let length = fs::metadata(&log).ok()?.len();
            let still = previous == Some(length);
            previous = Some(length);
            still.then_some(())
        },
    );

    let now = chrono::DateTime::parse_from_rfc3339(&stamp(chrono::Utc::now()))
        .unwrap()
        .with_timezone(&chrono::Utc);
    let interval = PUBLISH_INTERVAL.as_secs() as i64;
    let quiet_since = now - chrono::Duration::seconds(interval + PAST_THE_INTERVAL);
    File::options()
        .write(true)
        .open(&journal)
        .unwrap()
        .set_times(FileTimes::new().set_modified(quiet_since.into()))
        .unwrap();
    let status_at = |at: chrono::DateTime<chrono::Utc>| {
        activity
            .status(&service.ctx().clock(Clock::Fixed(stamp(at))))
            .unwrap()
    };
    let status = status_at(now);
    assert_eq!(
        status.silence_seconds,
        Some((interval + PAST_THE_INTERVAL) as u64),
        "the journal is quiet beyond the interval: {status:?}"
    );
    assert!(
        status
            .output_silence_seconds
            .is_some_and(|age| age <= interval as u64),
        "{status:?}"
    );
    assert_eq!(status.warning, None, "a printing gate needs no attention");

    // The same gate, read once its output has been quiet as long, does.
    let later = status_at(now + chrono::Duration::seconds(interval + PAST_THE_INTERVAL));
    let warning = later.warning.clone().unwrap_or_default();
    assert!(
        warning.contains("no progress evidence") && warning.contains("no gate output"),
        "{later:?}"
    );

    fs::write(&release, "").unwrap();
    let result = gate
        .wait_with_output_within(load_grace::graced_now(STORY_COMMAND_DEADLINE), || {
            "the released gate did not finish".into()
        });
    let verdict: Value = serde_json::from_slice(&result.stdout)
        .unwrap_or_else(|error| panic!("{error}: {result:?}"));
    assert_eq!(verdict["result"], "gate-passed", "{result:?}");
    fixture.assert_restored();
}

/// The journal silence the watchdog cases allow before the gate lock stops
/// the gate: room for one python3 writer start on a loaded machine, graced by
/// contention when each case begins. The rule is the subject, not speed.
const WATCHDOG_CEILING: Duration = Duration::from_secs(4);

/// The gate lock's TERM-to-KILL grace in the watchdog cases, long enough for
/// merge-watch's own trap to restore the poller first.
const WATCHDOG_CLEANUP_GRACE: Duration = Duration::from_secs(10);

/// How often the startup feeder appends while the merge is being prepared.
const FEED_INTERVAL: Duration = Duration::from_millis(250);

/// A gate under the real gate lock with a small silence ceiling, the way
/// `verify-pr.sh` wraps it, run through the bundled `merge-watch.sh`.
///
/// Until the gate touches its `started` file (its first statement), a feeder
/// appends to the journal, so merge preparation under load never counts as
/// the gate's silence: from then on only the gate's own lines renew the
/// ceiling (the SH-643 lesson in `tests/machine_lock.rs`).
fn watched_gate(fixture: &ForeignRepo, ceiling: u64, body: &str) -> (Output, PathBuf) {
    let journal = journal(fixture);
    let started = fixture.root.path().join("started");
    let feeding = {
        let journal = journal.clone();
        let started = started.clone();
        std::thread::spawn(move || {
            while !started.exists() {
                let mut file = fs::OpenOptions::new().append(true).open(&journal).unwrap();
                file.write_all(
                    b"{\"kind\":\"item\",\"path\":\"release gate/fixture-feeder\",\"status\":\"running\"}\n",
                )
                .unwrap();
                std::thread::sleep(FEED_INTERVAL);
            }
        })
    };
    let mut command = fixture.command("machine-lock.sh", &fixture.repo);
    command.env("STORYHOOK_GATE_PROGRESS", &journal).args([
        "--max-idle",
        &ceiling.to_string(),
        "--termination-grace",
        &WATCHDOG_CLEANUP_GRACE.as_secs().to_string(),
        "gate",
        "--",
        "bash",
        fixture.bundle.join("merge-watch.sh").to_str().unwrap(),
        "--speculative-run",
        &fixture.tree,
        &fixture.base,
        &fixture.head,
        fixture.poller.to_str().unwrap(),
        "--",
        "bash",
        "-eu",
        "-c",
        body,
        "foreign-gate",
        started.to_str().unwrap(),
    ]);
    let result = output(&mut command);
    // A run that ended before its gate started leaves the feeder waiting.
    fs::write(&started, "").unwrap();
    feeding.join().unwrap();
    (result, journal)
}

/// [`WATCHDOG_CEILING`] graced by the contention when a case begins.
fn watchdog_ceiling() -> u64 {
    load_grace::graced_now(WATCHDOG_CEILING).as_secs().max(1)
}

#[test]
fn a_gate_that_reports_each_leg_outlives_the_silence_ceiling() {
    let fixture = ForeignRepo::new();
    let ceiling = watchdog_ceiling();
    // Two lines one pause apart per leg, a pause well inside the ceiling,
    // for longer than two whole ceilings of wall clock in total.
    let pause = (WATCHDOG_CEILING.as_secs() / 4).max(1);
    let legs = 2 * ceiling / pause + 1;
    let body = format!(
        r#"
: > "$1"
leg=0
while [ "$leg" -lt {legs} ]; do
  leg=$((leg + 1))
  "$STORYHOOK_GATE_PROGRESS_WRITER" leg start "leg-$leg"
  echo "building leg $leg"
  sleep {pause}
  "$STORYHOOK_GATE_PROGRESS_WRITER" leg pass "leg-$leg"
done
"#,
    );
    let (result, journal) = watched_gate(&fixture, ceiling, &body);
    assert_eq!(
        result.status.code(),
        Some(0),
        "writer lines must renew a {ceiling}s ceiling: {result:?}"
    );
    let progress = gate_progress::fold(&fs::read_to_string(&journal).unwrap());
    assert_eq!(
        progress.items[1].children.len() as u64,
        legs + 1,
        "every leg, and the feeder"
    );
    fixture.assert_restored();
}

#[test]
fn a_gate_that_only_prints_or_stays_silent_is_still_stopped() {
    for (case, body) in [
        (
            "prints",
            ": > \"$1\"\nwhile :; do echo 'still compiling'; sleep 0.2; done",
        ),
        ("silent", ": > \"$1\"\nexec sleep 600"),
    ] {
        let fixture = ForeignRepo::new();
        let ceiling = watchdog_ceiling();
        let (result, journal) = watched_gate(&fixture, ceiling, body);
        assert_eq!(
            result.status.code(),
            Some(124),
            "{case}: output is not progress; the {ceiling}s ceiling must stop the gate: {result:?}"
        );
        let text = fs::read_to_string(&journal).unwrap();
        assert!(
            text.lines()
                .last()
                .is_some_and(|line| line.contains(r#""path":"release gate","status":"failed""#)),
            "{case}: {text}"
        );
        assert_eq!(
            gate_progress::fold(&text).watchdog,
            Some(gate_progress::WatchdogStop {
                lock: "gate".into(),
                idle: ceiling,
                ceiling,
            }),
            "{case}: the stop names its cause for the verifier to report: {text}"
        );
        fixture.assert_restored();
    }
}
