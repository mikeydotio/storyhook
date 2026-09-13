//! SH-713: a journal-only age cannot establish absence of raw process output.

use storyhook::service::gate_output::OutputObservation;
use storyhook::service::gate_progress::{VerificationProgressView, fold, render};

#[test]
fn stale_structured_progress_does_not_claim_unobserved_stdout_is_silent() {
    let progress = fold(
        r#"{"kind":"item","path":"release gate","status":"running","at":"2026-09-13T04:00:00Z"}"#,
    );
    let text = render(
        &VerificationProgressView::Running {
            progress: &progress,
            elapsed_seconds: Some(600),
            seconds_since_structured_progress: Some(600),
            output: &storyhook::service::gate_output::OutputObservation::Unavailable(
                "no authenticated reference".into(),
            ),
        },
        "2026-09-13T04:10:00Z",
    );
    assert!(!text.contains("NO GATE OUTPUT"), "{text}");
    assert!(text.contains("No structured progress for 10m"), "{text}");
    assert!(text.contains("Output observation unavailable"), "{text}");
}

#[test]
fn structured_and_output_reporting_thresholds_are_independent() {
    let progress = fold(r#"{"kind":"item","path":"release gate","status":"running"}"#);
    for (structured, raw, structured_warning, raw_warning) in [
        (180, 180, false, false),
        (181, 0, true, false),
        (0, 181, false, true),
        (181, 181, true, true),
    ] {
        let text = render(
            &VerificationProgressView::Running {
                progress: &progress,
                elapsed_seconds: Some(600),
                seconds_since_structured_progress: Some(structured),
                output: &OutputObservation::Observed(raw),
            },
            "2026-09-13T04:10:00Z",
        );
        assert_eq!(
            text.contains("No structured progress"),
            structured_warning,
            "{text}"
        );
        assert_eq!(
            text.contains("No stdout/stderr output observed"),
            raw_warning,
            "{text}"
        );
        assert!(!text.contains("NO GATE OUTPUT"), "{text}");
    }
}

#[test]
fn output_registration_encodes_paths_and_is_inert_without_an_attempt() {
    use std::process::Command;
    let dir = storyhook_test_support::scratch_dir();
    let log = dir.path().join("log with \"quotes\" and\na newline");
    let journal = dir.path().join("progress.ndjson");
    std::fs::write(&log, "").unwrap();
    let helper = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/gate-progress.sh");
    for attempt in [None, Some("owned-attempt")] {
        let mut command = Command::new("bash");
        command
            .args([
                "-c",
                ". \"$1\"; gate_progress_emit_output \"$2\"",
                "registration-probe",
            ])
            .arg(&helper)
            .arg(&log)
            .env("STORYHOOK_GATE_PROGRESS", &journal)
            .env_remove("STORYHOOK_VERIFICATION_ATTEMPT");
        if let Some(attempt) = attempt {
            command.env("STORYHOOK_VERIFICATION_ATTEMPT", attempt);
        }
        let result = command.output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        if attempt.is_none() {
            assert!(!journal.exists());
        } else {
            let content = std::fs::read_to_string(&journal).unwrap();
            assert_eq!(content.lines().count(), 1, "{content}");
            let output = fold(&content).output.unwrap();
            assert_eq!(output.path, log);
            assert_eq!(output.attempt_id, "owned-attempt");
        }
    }
    assert_eq!(std::fs::metadata(log).unwrap().len(), 0);
}
