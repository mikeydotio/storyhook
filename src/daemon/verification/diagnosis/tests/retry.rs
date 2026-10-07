//! A completed retry hands observability to its durable attribution hold.
use super::*;
use crate::service::verification::GenerationWrite;
use crate::store::{GlobalSeq, StoryNo, VerificationFailureDisposition, VerificationIncident};

fn incident(b: &Board) -> VerificationIncident {
    let generation = b.candidate.verifying_generation.unwrap();
    VerificationIncident {
        incident_id: format!("{}:{}", b.candidate.project.get(), generation.get()),
        project: b.candidate.project,
        story: StoryNo::new(1),
        generation,
        disposition: VerificationFailureDisposition::Retryable,
        halted: false,
        attempts: 1,
        detail: "PR head convergence is pending".into(),
        first_failed_at: b.env.now(),
        last_failed_at: b.env.now(),
    }
}

fn retain_hold(b: &Board, owner: &VerificationGuard, native: bool) -> bool {
    if native {
        matches!(
            owner.diagnose_gate_failure(&b.ctx(), &b.candidate).unwrap(),
            RustDiagnosisResult::Held { .. }
        )
    } else {
        matches!(
            VerificationQueue::new(&b.store)
                .record_generation_held(
                    &b.ctx(),
                    &b.candidate,
                    &owner.active.attempt_id,
                    "unproved failed gate",
                    FailureCause::Unknown,
                    "causal evidence remains unavailable",
                )
                .unwrap(),
            GenerationWrite::Applied(())
        )
    }
}

#[test]
fn attribution_hold_retires_only_its_completed_retry_and_survives_replay() {
    for native in [false, true] {
        let b = Board::new();
        let owner = b.owner();
        b.fail(&owner);
        let retry = incident(&b);
        let attempts = b
            .store
            .read(|tx| tx.gate_attempts(b.candidate.project))
            .unwrap();
        for replay in [false, true] {
            b.store
                .write(|tx| tx.put_verification_incident(&retry))
                .unwrap();
            assert!(
                retain_hold(&b, &owner, native),
                "native={native}, replay={replay}"
            );
            let reopened = SqliteStore::open(b.store.path()).unwrap();
            assert!(
                reopened
                    .read(|tx| tx.verification_incident(b.candidate.project))
                    .unwrap()
                    .is_none()
            );
            let records = reopened
                .read(|tx| tx.attributions(b.candidate.project))
                .unwrap();
            assert_eq!(records.len(), 1);
            assert!(records[0].held);
            assert!(records[0].retired.is_none());
            assert!(records[0].probes.is_empty());
            assert_eq!(
                reopened
                    .read(|tx| tx.gate_attempts(b.candidate.project))
                    .unwrap(),
                attempts
            );
            let row = reopened
                .read(|tx| tx.story(b.candidate.project, StoryNo::new(1)))
                .unwrap()
                .unwrap();
            assert_eq!(row.state, "verifying");
            assert!(VerificationQueue::new(&reopened).next().unwrap().is_none());
        }
    }
}

#[test]
fn attribution_hold_preserves_cleanup_halts_and_other_incident_identities() {
    for native in [false, true] {
        for kind in ["cleanup", "exhausted", "other-story", "other-generation"] {
            let b = Board::new();
            let owner = b.owner();
            b.fail(&owner);
            let mut retained = incident(&b);
            match kind {
                "cleanup" => {
                    retained.disposition = VerificationFailureDisposition::Permanent;
                    retained.halted = true;
                    retained.detail = "owned gate cleanup remains uncertain".into();
                }
                "exhausted" => retained.halted = true,
                "other-story" => {
                    StoryService::new(&b.ctx())
                        .create(&NewStoryInput {
                            title: "unrelated incident owner".into(),
                            ..Default::default()
                        })
                        .unwrap();
                    retained.story = StoryNo::new(2);
                }
                "other-generation" => {
                    retained.generation = GlobalSeq::new(retained.generation.get() + 1)
                }
                _ => unreachable!(),
            }
            b.store
                .write(|tx| tx.put_verification_incident(&retained))
                .unwrap();
            assert!(
                retain_hold(&b, &owner, native),
                "native={native}, kind={kind}"
            );
            assert_eq!(
                b.store
                    .read(|tx| tx.verification_incident(b.candidate.project))
                    .unwrap(),
                Some(retained),
                "native={native}, kind={kind}"
            );
        }
    }
}

#[test]
fn superseded_attribution_cannot_clear_a_retry_incident() {
    for native in [false, true] {
        for change in ["stop-start", "resubmit"] {
            let b = Board::new();
            let owner = b.owner();
            b.fail(&owner);
            let retry = incident(&b);
            b.store
                .write(|tx| tx.put_verification_incident(&retry))
                .unwrap();
            if change == "stop-start" {
                b.store
                    .write(|tx| {
                        tx.put_verification_enabled(b.candidate.project, false)?;
                        tx.put_verification_enabled(b.candidate.project, true)
                    })
                    .unwrap();
            } else {
                let ctx = b.ctx();
                let stories = StoryService::new(&ctx);
                stories
                    .set_state(&b.candidate.story_id, "in-progress", None, None, None)
                    .unwrap();
                stories
                    .set_state(&b.candidate.story_id, "verifying", None, None, None)
                    .unwrap();
            }
            // The operator action may itself reconcile the old incident; preserve
            // exactly what remains when stale diagnosis attempts its write.
            let before = b
                .store
                .read(|tx| tx.verification_incident(b.candidate.project))
                .unwrap();
            assert!(
                !retain_hold(&b, &owner, native),
                "native={native}, change={change}"
            );
            assert_eq!(
                b.store
                    .read(|tx| tx.verification_incident(b.candidate.project))
                    .unwrap(),
                before
            );
            assert!(
                b.store
                    .read(|tx| tx.attributions(b.candidate.project))
                    .unwrap()
                    .is_empty()
            );
        }
    }
}
