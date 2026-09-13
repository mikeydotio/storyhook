//! SH-713: foreign gates retain their owned execution state without invented
//! test counts or output observations. Exercise the production journal fold
//! and comment renderer; no subprocess output or publisher behavior is mocked.

use storyhook::service::gate_progress::{ItemStatus, VerificationProgressView, fold, render};

const COMPLETED_PREFLIGHT: &str = concat!(
    "{\"kind\":\"item\",\"path\":\"PR metadata\",\"status\":\"passed\"}\n",
    "{\"kind\":\"item\",\"path\":\"merge candidate\",\"status\":\"passed\"}\n",
    "{\"kind\":\"item\",\"path\":\"HEAD identity\",\"status\":\"passed\"}\n",
);

fn running_comment(journal: &str, structured_age: Option<u64>) -> String {
    let progress = fold(journal);
    render(
        &VerificationProgressView::Running {
            progress: &progress,
            elapsed_seconds: Some(240),
            seconds_since_last_event: structured_age,
        },
        "2026-09-12T12:04:00Z",
    )
}

#[test]
fn completed_preflight_does_not_make_an_owned_running_attempt_look_finished() {
    let body = running_comment(COMPLETED_PREFLIGHT, Some(0));
    let header = body
        .lines()
        .find(|line| line.starts_with("Verification ("))
        .expect("running verification header");

    assert!(header.contains("running"), "{body}");
}

#[test]
fn an_uninstrumented_release_gate_reports_running_with_unavailable_counts() {
    let journal = format!(
        "{COMPLETED_PREFLIGHT}{{\"kind\":\"item\",\"path\":\"release gate\",\"status\":\"running\",\"at\":\"2026-09-12T12:00:00Z\"}}\n"
    );
    let body = running_comment(&journal, Some(0));

    assert!(
        body.contains("release gate — running; detailed counts unavailable"),
        "{body}"
    );
    let header = body
        .lines()
        .find(|line| line.starts_with("Verification ("))
        .expect("running verification header");
    assert!(
        !header.contains('/'),
        "unknown gate progress must not have an aggregate completion fraction: {body}"
    );
}

#[test]
fn completed_or_failed_children_do_not_complete_the_owned_gate() {
    for child_status in ["passed", "failed"] {
        let journal = format!(
            "{{\"kind\":\"item\",\"path\":\"release gate\",\"status\":\"running\",\"at\":\"2026-09-12T12:00:00Z\"}}\n\
             {{\"kind\":\"item\",\"path\":\"release gate/unit tests\",\"status\":\"{child_status}\"}}\n"
        );
        let progress = fold(&journal);
        assert_eq!(progress.items[0].effective_status(), ItemStatus::Running);
        let step = progress.current_step().expect("the gate remains active");
        assert_eq!(step.label, "release gate");
        assert_eq!(step.tests, None);
        let body = running_comment(&journal, Some(0));
        assert!(body.contains("- [ ] release gate ("), "{body}");
        assert!(!body.contains("detailed counts unavailable"), "{body}");
        assert!(body.contains("unit tests"), "{body}");
    }
}
