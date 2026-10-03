//! Durable evidence reads use the ordinary parser, dispatcher and wire renderer.
use storyhook::cli::{Invocation, VerifierAction, parse_invocation};
use storyhook::invoke::dispatch;
use storyhook::output::{Response, render_response};
use storyhook::store::{GateAttempt, GateSubmission, GlobalSeq, Store, WriteOps};
use storyhook_test_support::ServiceFixture;

#[test]
fn evidence_requires_exactly_one_story_id() {
    let parsed = parse_invocation(&["verifier".into(), "evidence".into(), "SH-1".into()]).unwrap();
    assert_eq!(
        parsed,
        Invocation::Verifier {
            action: VerifierAction::Evidence {
                story_id: "SH-1".into()
            }
        }
    );
    for words in [
        vec!["verifier", "evidence"],
        vec!["verifier", "evidence", "SH-1", "SH-2"],
    ] {
        assert!(
            parse_invocation(&words.into_iter().map(String::from).collect::<Vec<_>>()).is_err()
        );
    }
}

#[test]
fn evidence_is_readable_without_live_ownership_and_survives_wire_roundtrip() {
    let fixture = ServiceFixture::new();
    let at = "2026-10-03T00:00:00Z";
    let mut attempt = GateAttempt::new(
        "admission".into(),
        GateSubmission {
            project: fixture.project(),
            story_id: "SH-1".into(),
            generation: Some(GlobalSeq::new(1)),
            submitted_at: Some(at.into()),
        },
        at,
    );
    attempt.elapsed.observe(900_000, "2026-10-03T00:15:00Z");
    fixture
        .store()
        .write(|tx| tx.insert_gate_attempt(&attempt))
        .unwrap();
    let answer = dispatch(
        &fixture.ctx(),
        Invocation::Verifier {
            action: VerifierAction::Evidence {
                story_id: "SH-1".into(),
            },
        },
    )
    .unwrap();
    let roundtrip: Response =
        serde_json::from_str(&serde_json::to_string(&answer).unwrap()).unwrap();
    assert_eq!(
        render_response(&answer, true, false),
        render_response(&roundtrip, true, false)
    );
    let json: serde_json::Value =
        serde_json::from_str(&render_response(&answer, true, false)).unwrap();
    assert_eq!(
        json["evidence"]["attempts"][0]["elapsed"]["milliseconds"],
        900_000
    );
    assert_eq!(
        json["evidence"]["submissions"][0]["breaches"][0],
        "admission"
    );
    assert!(render_response(&answer, false, false).contains("process-budget-breach"));
}
