//! Real native results matched to isolated durable records; no injected probe verdicts.
use super::*;
mod forgery;
use crate::service::{Ctx, NewStoryInput, StoryService, VerificationCandidate, VerificationQueue};
use crate::store::{
    GateAttempt, GateExecution, GateExecutionPurpose, GateFailedCase, GateSubmission, ProjectId,
    ReadOps, SqliteStore, Store, WriteOps,
};
use std::path::PathBuf;

pub(super) struct Evidence {
    fixture: storyhook_test_support::ServiceFixture,
    store: SqliteStore,
    candidate: VerificationCandidate,
}

impl Evidence {
    pub(super) fn new() -> Self {
        let fixture = storyhook_test_support::ServiceFixture::new();
        let store = SqliteStore::open(fixture.store().path()).unwrap();
        let ctx = Ctx::new(
            &store,
            ProjectId::new(fixture.project().get()),
            fixture.cwd(),
            Environment::at(fixture.cwd()),
        )
        .no_hooks(true);
        let stories = StoryService::new(&ctx);
        let id = stories
            .create(&NewStoryInput {
                title: "native proof boundary".into(),
                ..Default::default()
            })
            .unwrap()
            .id;
        stories
            .set_state(&id, "verifying", None, None, None)
            .unwrap();
        let candidate = VerificationQueue::new(&store).next().unwrap().unwrap();
        Self {
            fixture,
            store,
            candidate,
        }
    }
    pub(super) fn generation(&self) -> i64 {
        self.candidate.verifying_generation.unwrap().get()
    }

    fn retain(
        &self,
        native: &NativeRustComparison,
        original: &Path,
        mixed: bool,
    ) -> AttributionRecord {
        let at = "2026-10-05T00:00:00Z";
        let submission = GateSubmission {
            project: self.candidate.project,
            story_id: self.candidate.story_id.clone(),
            generation: self.candidate.verifying_generation,
            submitted_at: self.candidate.verifying_since.clone(),
        };
        let mut attempt = GateAttempt::new("attempt".into(), submission.clone(), at);
        attempt.control_revision = Some(0);
        attempt.verdict = Some("tests-failed".into());
        let mut gate = GateExecution::new("original".into(), at, original.display().to_string());
        gate.inputs = native.gate.clone();
        gate.submissions = vec![submission.clone()];
        gate.finished_at = Some(at.into());
        gate.milliseconds = Some(1);
        gate.verdict = Some("tests-failed".into());
        gate.journal_bound = true;
        gate.logs = vec![original.display().to_string()];
        gate.failed_cases = vec![GateFailedCase {
            path: "rust-suite".into(),
            name: Some("answer".into()),
            target: Some("contract".into()),
            identity: None,
            title_path: None,
        }];
        attempt.executions.push(gate);
        let mut record = AttributionRecord {
            version: 1,
            id: "attribution".into(),
            revision: 0,
            submission,
            attempt: attempt.id.clone(),
            inputs: native.gate.clone(),
            created_at: at.into(),
            components: vec![FailureComponent {
                id: "answer".into(),
                check: "rust:subject:contract:answer".into(),
                signature: native
                    .case
                    .original_failure(&fs::read(original).unwrap())
                    .unwrap(),
                requirement: "assertion must hold".into(),
                log: original.display().to_string(),
                observed_cause: FailureCause::Unknown,
            }],
            preparation: None,
            settlement: None,
            plans: vec![],
            probes: vec![],
            assessments: vec![],
            diagnosis_ms: 0,
            held: true,
            retired: None,
        };
        let mut unproved = record.components[0].clone();
        unproved.id = "unproved".into();
        unproved.check = "unproved-shared-check".into();
        if mixed {
            record.components.push(unproved);
        }
        self.store
            .write(|tx| tx.insert_attribution(&record))
            .unwrap();
        record.preparation = Some(DiagnosticPreparation {
            started_at: at.into(),
            completed: None,
        });
        self.save(&mut record);
        record.preparation.as_mut().unwrap().completed = Some(PreparationResult {
            milliseconds: 0,
            log: original.display().to_string(),
            detail: "native preparation".into(),
            cleanup_complete: true,
        });
        record.plans.push(native.plan("answer"));
        self.save(&mut record);
        let mut preparation =
            GateExecution::new("preparation".into(), at, original.display().to_string());
        preparation.purpose = GateExecutionPurpose::DiagnosisPreparation {
            attribution: record.id.clone(),
        };
        preparation.inputs = record.inputs.clone();
        preparation.submissions = vec![record.submission.clone()];
        preparation.finished_at = Some(at.into());
        preparation.milliseconds = Some(1);
        preparation.verdict = Some("passed".into());
        preparation.journal_bound = true;
        attempt.executions.push(preparation);
        for (index, (side, result)) in native.observations.iter().enumerate() {
            let id = format!("probe-{index}");
            record.probes.push(DiagnosticProbe {
                id: id.clone(),
                plan: 0,
                side: *side,
                started_at: at.into(),
                completed: None,
            });
            self.save(&mut record);
            let mut execution = GateExecution::new(
                result.execution_id.clone(),
                at,
                format!("{}/cost.ndjson", result.log),
            );
            execution.purpose = GateExecutionPurpose::Diagnosis {
                attribution: record.id.clone(),
                probe: id,
            };
            execution.inputs = record.inputs.clone();
            execution.submissions = vec![record.submission.clone()];
            execution.finished_at = Some(at.into());
            execution.milliseconds = Some(result.milliseconds);
            execution.verdict = Some(
                if *side == ProbeSide::Candidate {
                    "failed"
                } else {
                    "passed"
                }
                .into(),
            );
            execution.journal_bound = true;
            attempt.executions.push(execution);
            record.probes.last_mut().unwrap().completed = Some(result.clone());
            record.diagnosis_ms += result.milliseconds;
            self.save(&mut record);
        }
        self.store
            .write(|tx| tx.insert_gate_attempt(&attempt))
            .unwrap();
        record
    }
    fn save(&self, record: &mut AttributionRecord) {
        let expected = record.revision;
        record.revision += 1;
        assert!(
            self.store
                .write(|tx| tx.update_attribution(record, expected))
                .unwrap()
        );
    }
}

pub(super) fn exercise(native: NativeRustComparison, f: &Fixture, evidence: Evidence, mixed: bool) {
    let original = f.directory.path().join("original-gate.log");
    let mut log = b"     Running tests/contract.rs (target/contract)\n".to_vec();
    log.extend(fs::read(Path::new(&native.observations[0].1.log).join("run.stdout")).unwrap());
    fs::write(&original, log).unwrap();
    let mut record = evidence.retain(&native, &original, mixed);
    let settled = native.settle().unwrap();
    record.diagnosis_ms = record.diagnosis_ms.max(settled.milliseconds());
    record.settlement = Some(DiagnosticSettlement {
        completed_at: record.created_at.clone(),
        milliseconds: settled.milliseconds(),
        detail: "actual native fixture settlement".into(),
        cleanup_complete: true,
    });
    evidence.save(&mut record);
    forgery::records(&evidence, &settled, &record);
    forgery::history(&evidence, &settled, &record);
    let result = evidence
        .store
        .read(|tx| settled.prove(tx, &evidence.candidate, &record.id, "original"));
    assert!(
        result.is_ok(),
        "real native contrast did not prove cause: {:?}",
        result.err()
    );
    let proof = evidence
        .store
        .read(|tx| settled.prove(tx, &evidence.candidate, &record.id, "original"))
        .unwrap();
    let ctx = Ctx::new(
        &evidence.store,
        evidence.candidate.project,
        evidence.fixture.cwd(),
        Environment::at(evidence.fixture.cwd()),
    )
    .no_hooks(true);
    let before = evidence
        .store
        .read(|tx| tx.gate_attempts(evidence.candidate.project))
        .unwrap();
    revocations(&evidence, &proof, &record, &ctx);
    let result = VerificationQueue::new(&evidence.store)
        .record_causal_return(&ctx, &evidence.candidate, &proof)
        .unwrap();
    assert!(result);
    let story = crate::store::StoryNo::parse_id("SH", &evidence.candidate.story_id).unwrap();
    let row = evidence
        .store
        .read(|tx| tx.story(evidence.candidate.project, story))
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "in-progress", "proved failure was not returned");
    assert!(
        !VerificationQueue::new(&evidence.store)
            .record_causal_return(&ctx, &evidence.candidate, &proof)
            .unwrap(),
        "duplicate proof returned a generation twice"
    );
    let retained = evidence
        .store
        .read(|tx| tx.attributions(evidence.candidate.project))
        .unwrap();
    let retained = retained.iter().find(|r| r.id == record.id).unwrap();
    assert_eq!(
        retained.held, mixed,
        "only mixed records keep their unproved hold"
    );
    assert_eq!(retained.retired.is_some(), !mixed);
    assert_eq!(retained.components, record.components);
    assert_eq!(retained.assessments.len(), 1);
    assert_eq!(retained.assessments[0].component, "answer");
    assert_eq!(
        evidence
            .store
            .read(|tx| tx.gate_attempts(evidence.candidate.project))
            .unwrap(),
        before,
        "causal return changed gate certification evidence"
    );
    // Keep the isolated service roots alive until the complete proof check returns.
    drop(evidence.fixture);
}

fn revocations(
    evidence: &Evidence,
    proof: &CausalReturnEvidence,
    record: &AttributionRecord,
    ctx: &Ctx<'_, SqliteStore>,
) {
    use crate::domain::StoryEvent;
    use crate::store::{ExpectedSeq, StoreError, StoryNo};
    let blocker = StoryService::new(ctx)
        .create(&NewStoryInput {
            title: "real dependency".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    for change in [
        "stop",
        "stop-start",
        "finished",
        "new-attempt",
        "revision",
        "exhausted",
        "human-only",
        "human-only-removed",
        "awaiting",
        "resubmit",
        "checkout",
        "blocked",
    ] {
        let result = ctx.write_stories(|tx| {
            let project = evidence.candidate.project;
            let story = StoryNo::parse_id("SH", &evidence.candidate.story_id)?;
            match change {
                "stop" | "stop-start" => {
                    tx.put_verification_enabled(project, false)?;
                    if change == "stop-start" {
                        tx.put_verification_enabled(project, true)?;
                    }
                }
                "checkout" => {
                    tx.set_checkout_path(project, Some(Path::new("/foreign-checkout")))?
                }
                "finished" | "new-attempt" => {
                    let mut attempt = tx.gate_attempts(project)?.pop().unwrap();
                    if change == "new-attempt" {
                        let replacement = GateAttempt::new(
                            "replacement-attempt".into(),
                            attempt.submission.clone(),
                            &ctx.now(),
                        );
                        tx.insert_gate_attempt(&replacement)?;
                    } else {
                        let revision = attempt.revision;
                        attempt.revision += 1;
                        attempt.finished_at = Some(ctx.now());
                        assert!(tx.update_gate_attempt(&attempt, revision)?);
                    }
                }
                "revision" | "exhausted" => {
                    let mut changed = record.clone();
                    changed.revision += 1;
                    if change == "exhausted" {
                        changed.diagnosis_ms = MAX_DIAGNOSIS_MS;
                    }
                    assert!(tx.update_attribution(&changed, record.revision)?);
                }
                _ => {
                    let at = ctx.now();
                    let events = match change {
                        "blocked" => vec![StoryEvent::StoryRelationshipAdded {
                            at,
                            other_id: blocker.clone(),
                            relation: "blocked-by".into(),
                        }],
                        "human-only" => vec![StoryEvent::StoryLabelsSet {
                            at,
                            labels: vec!["human-only".into()],
                        }],
                        "human-only-removed" => vec![
                            StoryEvent::StoryLabelsSet {
                                at: at.clone(),
                                labels: vec!["human-only".into()],
                            },
                            StoryEvent::StoryLabelsSet { at, labels: vec![] },
                        ],
                        "awaiting" => vec![StoryEvent::StoryAwaitingSet {
                            at,
                            awaiting: "real external hold".into(),
                        }],
                        "resubmit" => vec![
                            StoryEvent::StoryStateChanged {
                                at: at.clone(),
                                state: "in-progress".into(),
                            },
                            StoryEvent::StoryStateChanged {
                                at,
                                state: "verifying".into(),
                            },
                        ],
                        _ => unreachable!(),
                    };
                    crate::service::append_and_fold(
                        tx,
                        project,
                        story,
                        "SH",
                        &tx.state_map(project)?,
                        ExpectedSeq::Exact(tx.story(project, story)?.unwrap().head_seq),
                        &events,
                        ctx.provenance(),
                    )?;
                }
            }
            assert!(
                !proof.validate(tx, &evidence.candidate)?,
                "stale proof accepted after {change}"
            );
            Err::<(), _>(StoreError::Validation(
                "rollback isolated revocation".into(),
            ))
        });
        let error = result.unwrap_err().to_string();
        assert!(
            error.contains("rollback isolated revocation"),
            "{change}: {error}"
        );
        assert!(
            evidence
                .store
                .read(|tx| proof.validate(tx, &evidence.candidate))
                .unwrap()
        );
    }
    for path in [
        PathBuf::from(&record.components[0].log),
        Path::new(&record.probes[0].completed.as_ref().unwrap().log).join("run.stdout"),
    ] {
        let original = fs::read(&path).unwrap();
        fs::write(&path, "tampered retained output").unwrap();
        assert!(
            evidence
                .store
                .read(|tx| proof.validate(tx, &evidence.candidate))
                .is_err()
        );
        fs::write(path, original).unwrap();
    }
    assert!(!proof.diagnosis().contains("unproved-shared-check"));
}
