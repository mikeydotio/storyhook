//! SH-777: the verifier status reads the owned gate's raw output as activity.
//!
//! An uninstrumented project gate writes no journal line after the verifier's
//! own "release gate running", so journal age alone called every healthy run
//! "no progress evidence" after one publisher interval. These cases drive the
//! real status snapshot over the SH-713 fixture: the journal is always quiet,
//! and only the log and the clock change.

use super::output_reporting::{LATER, NOW, OutputFixture, RECENT, START, set_modified};
use super::*;
use std::fs::{self, File};
use std::io::Write;

use storyhook::daemon::verification::status::VerifierStatus;

/// The journal-only warning's own prefix, which operators and the dashboard
/// fixture (`e2e/specs/verification-control.spec.ts`) search for.
const WARNING_PREFIX: &str = "verifier has no progress evidence for";

fn status_at(run: &OutputFixture, now: &str) -> VerifierStatus {
    run.activity
        .status(&run.fixture.ctx().clock(Clock::Fixed(now.into())))
        .unwrap()
}

fn seconds_after(start: &str, seconds: i64) -> String {
    (chrono::DateTime::parse_from_rfc3339(start).unwrap() + chrono::Duration::seconds(seconds))
        .with_timezone(&chrono::Utc)
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn append(path: &Path, bytes: &[u8], at: &str) {
    File::options()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
    set_modified(path, at);
}

#[test]
fn a_gate_that_prints_needs_no_attention_while_its_journal_is_quiet() {
    let run = OutputFixture::new();
    let status = status_at(&run, NOW);
    assert_eq!(status.silence_seconds, Some(600), "the journal stays quiet");
    assert_eq!(status.output_silence_seconds, Some(1), "{status:?}");
    assert_eq!(status.warning, None);
    assert!(
        status.render_human().contains("Last gate output: 1s ago"),
        "{}",
        status.render_human()
    );
    let wire = serde_json::to_value(&status).unwrap();
    assert_eq!(wire["output_silence_seconds"], 1);
    let decoded: VerifierStatus = serde_json::from_value(wire).unwrap();
    assert_eq!(decoded.output_silence_seconds, Some(1));
}

#[test]
fn quiet_output_beyond_the_publisher_interval_still_needs_attention() {
    let interval = storyhook::daemon::verification_progress::PUBLISH_INTERVAL.as_secs() as i64;
    for (quiet, overdue) in [(interval, false), (interval + 1, true)] {
        let run = OutputFixture::new();
        set_modified(&run.log, &seconds_after(NOW, -quiet));
        let status = status_at(&run, NOW);
        assert_eq!(status.output_silence_seconds, Some(quiet as u64));
        assert_eq!(status.warning.is_some(), overdue, "{quiet}s: {status:?}");
        if let Some(warning) = status.warning {
            assert!(warning.contains(WARNING_PREFIX), "{warning}");
            assert!(
                warning.contains(&format!("for 600s and no gate output for {quiet}s")),
                "the warning names both quiet sources: {warning}"
            );
        }
    }
}

#[test]
fn a_status_read_between_publisher_ticks_never_moves_the_publisher_baseline() {
    let run = OutputFixture::new();
    // The publisher binds the log at 00:10:00.
    run.publish(NOW);
    // The gate prints at 00:13:30; status reads at 00:14:00, before the next tick.
    append(&run.log, b"; later output", "2026-01-01T00:13:30Z");
    let status = status_at(&run, LATER);
    assert_eq!(status.silence_seconds, Some(840));
    assert_eq!(status.output_silence_seconds, Some(30));
    assert_eq!(status.warning, None);
    // Had status committed its reading, the publisher would find no growth
    // at 00:18:00 and report 4m 30s of silence from 00:13:30.
    let body = run.publish("2026-01-01T00:18:00Z");
    assert!(
        !body.contains("No stdout/stderr output observed"),
        "the publisher must still see the growth itself: {body}"
    );
}

#[test]
fn a_finished_gate_capture_is_history_and_leaves_the_journal_rule() {
    let run = OutputFixture::new();
    let mut records = run.records();
    records[2]["status"] = "passed".into();
    records.push(serde_json::json!({"kind":"item", "path":"land pull request", "status":"running", "at":START}));
    run.write_journal(records);
    let status = status_at(&run, NOW);
    assert_eq!(status.output_silence_seconds, None);
    assert!(
        status
            .warning
            .as_deref()
            .is_some_and(|warning| warning.ends_with(&format!(
                "{WARNING_PREFIX} 600s; story verifier status; story daemon logs"
            ))),
        "{status:?}"
    );
    assert!(
        serde_json::to_value(&status)
            .unwrap()
            .get("output_silence_seconds")
            .is_none(),
        "an absent observation is absent on the wire"
    );
}

#[test]
fn output_that_cannot_be_bound_to_this_attempt_never_quiets_the_warning() {
    for damage in [
        "legacy-run",
        "foreign-output",
        "missing-log",
        "replaced-log",
    ] {
        let run = OutputFixture::new();
        let mut rows = run.records();
        match damage {
            "legacy-run" => {
                rows[0].as_object_mut().unwrap().remove("attempt_id");
            }
            "foreign-output" => {
                rows[1]["attempt_id"] = uuid::Uuid::new_v4().to_string().into();
            }
            "missing-log" => fs::remove_file(&run.log).unwrap(),
            _ => {
                fs::rename(&run.log, run.log.with_extension("previous")).unwrap();
                fs::write(&run.log, "replacement output").unwrap();
                set_modified(&run.log, RECENT);
            }
        }
        run.write_journal(rows);
        let status = status_at(&run, NOW);
        assert_eq!(status.output_silence_seconds, None, "{damage}");
        assert_eq!(
            status.evidence_error, None,
            "{damage}: unusable output is not a journal fault"
        );
        assert!(
            status
                .warning
                .as_deref()
                .is_some_and(|warning| warning.contains(WARNING_PREFIX)),
            "{damage}: {status:?}"
        );
    }
}
