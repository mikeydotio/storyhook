//! A failed administrative check retains work without assigning repair ownership.
use super::*;
use crate::service::attribution::{AttributionRecord, FailureCause, FailureComponent};

impl<S: Store> VerificationQueue<'_, S> {
    /// Retain an unproved failure and its hold atomically, bound to the admitted generation.
    pub(crate) fn record_generation_held(
        &self,
        ctx: &Ctx<'_, S>,
        candidate: &VerificationCandidate,
        attempt_id: &str,
        check: &str,
        detail: &str,
    ) -> Result<GenerationWrite<()>, AppError> {
        if candidate.project != ctx.project() || check.trim().is_empty() || detail.trim().is_empty()
        {
            return Err(AppError::Validation(
                "attribution hold requires this project and a complete diagnostic".into(),
            ));
        }
        let now = ctx.now();
        Ok(ctx.write_stories(|tx| {
            let project = candidate.project;
            let prefix = project_prefix(tx, project)?;
            let (story, row) = resolve_story(tx, project, &prefix, &candidate.story_id)?;
            if !candidate_is_current(tx, &row, candidate)?
                || row.awaiting.is_some()
                || candidate.landing_pending
                || tx.landing_intents()?.iter().any(|i| i.project == project && i.story == story)
                || !tx.verification_enabled(project)?
            {
                return Ok(GenerationWrite::Superseded);
            }
            let attempts = tx.gate_attempts(project)?;
            let attempt = attempts.iter().rev().find(|a| a.submission.story_id == candidate.story_id);
            // Legacy submissions may be held, but the probe store still requires an exact generation.
            let Some(attempt) = attempt.filter(|a| a.id == attempt_id
                && a.submission.generation == candidate.verifying_generation
                && a.finished_at.is_none()) else {
                return Ok(GenerationWrite::Superseded);
            };
            if attempt.control_revision != Some(tx.verification_control_revision(project)?) {
                return Ok(GenerationWrite::Superseded);
            }
            // Repeated disposition is idempotent; different observations must remain separate.
            if tx.attributions(project)?.iter().any(|a| a.attempt == attempt_id
                && a.held && a.components.iter().any(|c| c.check == check && c.signature == detail)) {
                return Ok(GenerationWrite::Applied(()));
            }
            let id = uuid::Uuid::new_v4().to_string();
            let record = AttributionRecord {
                version: 1, id: id.clone(), revision: 0,
                submission: attempt.submission.clone(), attempt: attempt.id.clone(),
                inputs: attempt.executions.last().map(|e| e.inputs.clone()).unwrap_or_default(),
                created_at: now.clone(),
                components: vec![FailureComponent {
                    id: check.into(), check: check.into(), signature: detail.into(),
                    requirement: "Establish causal responsibility before assigning a repair".into(),
                    log: format!("story:{}:attribution:{id}", candidate.story_id),
                    observed_cause: FailureCause::Unknown,
                }],
                plans: vec![], probes: vec![], assessments: vec![], diagnosis_ms: 0,
                held: true, retired: None,
            };
            tx.insert_attribution(&record)?;
            let states = tx.state_map(project)?;
            append_and_fold(tx, project, story, &prefix, &states, ExpectedSeq::Exact(row.head_seq),
                &[StoryEvent::StoryCommentAdded { at: now.clone(), text: format!(
                    "CENTRAL VERIFICATION ATTRIBUTION HELD — {check}. Cause: unknown. Evidence: {id}. The story remains verifying; no repair is assigned. Inspect `story verifier evidence {} --json` and establish cause before retry or repair.\n\n{}",
                    candidate.story_id, crate::text_lint::quote_evidence(detail)) }], ctx.provenance())?;
            Ok(GenerationWrite::Applied(()))
        })?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{GateAttempt, GateSubmission};

    #[test]
    fn administrative_hold_rechecks_admission_and_policy_before_writing() {
        for change in [
            "none",
            "stop",
            "stop-start",
            "new-attempt",
            "finished",
            "legacy-epoch",
            "legacy-generation",
            "human-only",
            "resubmit",
            "blocked",
            "awaiting",
        ] {
            let fixture = storyhook_test_support::ServiceFixture::new();
            let store = crate::store::SqliteStore::open(fixture.store().path()).unwrap();
            let project = ProjectId::new(fixture.project().get());
            let ctx = Ctx::new(
                &store,
                project,
                fixture.cwd(),
                crate::env::Environment::at(fixture.cwd()),
            )
            .no_hooks(true);
            let stories = crate::service::StoryService::new(&ctx);
            let id = stories
                .create(&crate::service::NewStoryInput {
                    title: "administrative hold fence".into(),
                    state: (change == "legacy-generation").then(|| VERIFYING_STATE.into()),
                    ..Default::default()
                })
                .unwrap()
                .id;
            if change != "legacy-generation" {
                stories
                    .set_state(&id, VERIFYING_STATE, None, None, None)
                    .unwrap();
            }
            let queue = VerificationQueue::new(&store);
            let candidate = queue.next().unwrap().unwrap();
            assert_eq!(
                candidate.verifying_generation.is_none(),
                change == "legacy-generation"
            );
            let mut attempt = GateAttempt::new(
                "owned-attempt".into(),
                GateSubmission {
                    project,
                    story_id: id.clone(),
                    generation: candidate.verifying_generation,
                    submitted_at: candidate.verifying_since.clone(),
                },
                &ctx.now(),
            );
            attempt.control_revision = Some(0);
            if change == "legacy-epoch" {
                attempt.control_revision = None;
            }
            store.write(|tx| tx.insert_gate_attempt(&attempt)).unwrap();
            if change == "finished" {
                attempt.finished_at = Some(ctx.now());
                attempt.revision = 1;
                assert!(
                    store
                        .write(|tx| tx.update_gate_attempt(&attempt, 0))
                        .unwrap()
                );
            }
            match change {
                "stop" | "stop-start" => store
                    .write(|tx| {
                        tx.put_verification_enabled(project, false)?;
                        if change == "stop-start" {
                            tx.put_verification_enabled(project, true)?;
                        }
                        Ok(())
                    })
                    .unwrap(),
                "new-attempt" => {
                    attempt.id = "replacement-attempt".into();
                    store.write(|tx| tx.insert_gate_attempt(&attempt)).unwrap();
                }
                "human-only" => {
                    stories
                        .set_labels(&id, &["human-only".into()], &[])
                        .unwrap();
                }
                "resubmit" => {
                    stories
                        .set_state(&id, "in-progress", None, None, None)
                        .unwrap();
                    stories
                        .set_state(&id, VERIFYING_STATE, None, None, None)
                        .unwrap();
                }
                "blocked" => {
                    let blocker = stories
                        .create(&crate::service::NewStoryInput {
                            title: "real dependency".into(),
                            ..Default::default()
                        })
                        .unwrap()
                        .id;
                    crate::service::RelationService::new(&ctx)
                        .block_on(&id, &[blocker], None)
                        .unwrap();
                }
                "awaiting" => {
                    stories.set_awaiting(&id, "external work").unwrap();
                }
                _ => {}
            }
            let before = store
                .read(|tx| tx.events_for(project, StoryNo::parse_id("SH", &id).unwrap()))
                .unwrap();
            let result = queue
                .record_generation_held(
                    &ctx,
                    &candidate,
                    "owned-attempt",
                    "submission",
                    "input cannot be established",
                )
                .unwrap();
            let records = store.read(|tx| tx.attributions(project)).unwrap();
            if matches!(change, "none" | "legacy-generation") {
                assert_eq!(result, GenerationWrite::Applied(()), "{change}");
                assert_eq!(records.len(), 1);
                assert!(queue.next().unwrap().is_none());
                let after = store
                    .read(|tx| tx.events_for(project, StoryNo::parse_id("SH", &id).unwrap()))
                    .unwrap();
                assert_eq!(
                    queue
                        .record_generation_held(
                            &ctx,
                            &candidate,
                            "owned-attempt",
                            "submission",
                            "input cannot be established"
                        )
                        .unwrap(),
                    GenerationWrite::Applied(()),
                    "{change}"
                );
                assert_eq!(
                    store
                        .read(|tx| tx.events_for(project, StoryNo::parse_id("SH", &id).unwrap()))
                        .unwrap(),
                    after
                );
            } else {
                assert_eq!(result, GenerationWrite::Superseded, "{change}");
                assert!(records.is_empty(), "{change}");
                assert_eq!(
                    store
                        .read(|tx| tx.events_for(project, StoryNo::parse_id("SH", &id).unwrap()))
                        .unwrap(),
                    before,
                    "{change}"
                );
            }
            assert_eq!(
                store
                    .read(|tx| tx.gate_attempts(project))
                    .unwrap()
                    .last()
                    .unwrap(),
                &attempt
            );
        }
    }
}
