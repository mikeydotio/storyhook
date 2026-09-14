//! One owned journal observation shared by verifier status projections.

use std::io::Read;

use super::{ActiveVerification, journal_path};
use crate::env::Environment;
use crate::service::VerificationCandidate;
use crate::service::gate_progress::{self, GateProgress};
use crate::store::{StoryNo, VerificationFailureDisposition, VerificationIncident};

/// One read of the active attempt's journal, including its freshness and diagnostics.
#[derive(Default)]
pub(crate) struct AttemptEvidence {
    /// Folded progress identifying the owned generation and attempt.
    pub(crate) progress: Option<GateProgress>,
    /// Matching journal modification time, or ownership time before evidence exists.
    pub(crate) last_evidence_at: Option<String>,
    /// Context explaining why matching journal evidence could not be inspected.
    pub(crate) error: Option<String>,
}

impl AttemptEvidence {
    /// Reads and folds one owned journal; metadata comes from the same open handle.
    pub(crate) fn read(
        ordered: &[VerificationCandidate],
        active: Option<&ActiveVerification>,
        env: &Environment,
    ) -> Self {
        let Some(active) = active else {
            return Self::default();
        };
        let Some(candidate) = owned_candidate(ordered, active) else {
            return Self {
                last_evidence_at: Some(active.started_at.clone()),
                error: Some(format!(
                    "owned generation {:?} for project {} story {} is no longer in the verifying queue",
                    active.generation, active.project, active.story_id
                )),
                ..Self::default()
            };
        };
        let path = journal_path(env, candidate);
        let mut file = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Self {
                    last_evidence_at: Some(active.started_at.clone()),
                    ..Self::default()
                };
            }
            Err(error) => {
                return Self {
                    error: Some(format!("cannot open {}: {error}", path.display())),
                    ..Self::default()
                };
            }
        };
        let mut text = String::new();
        let modified = match file
            .read_to_string(&mut text)
            .and_then(|_| file.metadata()?.modified())
        {
            Ok(modified) => modified,
            Err(error) => {
                return Self {
                    error: Some(format!("cannot inspect {}: {error}", path.display())),
                    ..Self::default()
                };
            }
        };
        let progress = gate_progress::fold(&text);
        if !crate::daemon::verification_progress::identifies_active_attempt(&progress, active) {
            return Self {
                last_evidence_at: Some(active.started_at.clone()),
                error: Some(format!(
                    "journal {} does not identify the active generation and attempt {} for project {} story {}",
                    path.display(),
                    active.attempt_id,
                    active.project,
                    active.story_id
                )),
                ..Self::default()
            };
        }
        match crate::service::gate_output::metadata_time(modified) {
            Ok(modified) => Self {
                progress: Some(progress),
                last_evidence_at: Some(modified.to_rfc3339()),
                error: None,
            },
            Err(error) => Self {
                error: Some(format!(
                    "cannot inspect {} timestamp: {error}",
                    path.display()
                )),
                ..Self::default()
            },
        }
    }

    /// Distinguishes current infrastructure authority from an authenticated retry's history.
    pub(crate) fn incident_is_current(
        &self,
        ordered: &[VerificationCandidate],
        active: Option<&ActiveVerification>,
        incident: Option<&VerificationIncident>,
    ) -> bool {
        let Some(incident) = incident else {
            return false;
        };
        let Some(active) = active else {
            return true;
        };
        let Some(candidate) = owned_candidate(ordered, active) else {
            return true;
        };
        let same_story = candidate
            .story_id
            .rsplit_once('-')
            .is_some_and(|(prefix, _)| {
                StoryNo::parse_id(prefix, &candidate.story_id)
                    .is_ok_and(|story| story == incident.story)
            });
        let same_failure = active.retry_origin.as_ref().is_some_and(|origin| {
            origin.incident_id == incident.incident_id && origin.attempts == incident.attempts
        });
        let reached_gate = self.progress.as_ref().is_some_and(|progress| {
            progress.run.as_ref().is_some_and(|run| {
                run.attempt_id.as_deref() == Some(active.attempt_id.as_str())
                    && Some(run.generation) == active.generation.map(|generation| generation.get())
            }) && progress.reached_verification_gate()
        });
        !(incident.disposition == VerificationFailureDisposition::Retryable
            && !incident.halted
            && incident.project == active.project
            && Some(incident.generation) == active.generation
            && same_story
            && same_failure
            && reached_gate)
    }
}

fn owned_candidate<'a>(
    ordered: &'a [VerificationCandidate],
    active: &ActiveVerification,
) -> Option<&'a VerificationCandidate> {
    ordered.iter().find(|candidate| {
        candidate.project == active.project
            && candidate.story_id == active.story_id
            && candidate.verifying_generation.is_some()
            && candidate.verifying_generation == active.generation
    })
}

#[cfg(test)]
mod tests {
    use super::super::VerificationRetryOrigin;
    use super::*;
    use crate::domain::Priority;
    use crate::service::VerificationProblem;
    use crate::store::{GlobalSeq, ProjectId, StoryNo};

    const STARTED: &str = "2026-01-01T00:00:00Z";

    fn fixture() -> (
        VerificationCandidate,
        ActiveVerification,
        VerificationIncident,
    ) {
        let candidate = VerificationCandidate {
            project: ProjectId::new(1),
            project_slug: "fixture".into(),
            story_id: "SH-1".into(),
            title: "Retry evidence".into(),
            priority: Priority::Low,
            created_at: STARTED.into(),
            verifying_since: Some(STARTED.into()),
            verifying_generation: Some(GlobalSeq::new(7)),
            blocking_revision: None,
            blocked_by: Vec::new(),
            landing_pending: false,
            checkout: "/tmp/unused-retry-checkout".into(),
            cleanup_lease: None,
            pull_request: Err(VerificationProblem::MissingPullRequest),
        };
        let active = ActiveVerification {
            attempt_id: "attempt-now".into(),
            project: candidate.project,
            story_id: candidate.story_id.clone(),
            generation: candidate.verifying_generation,
            started_at: STARTED.into(),
            retry_origin: Some(VerificationRetryOrigin {
                incident_id: "incident-first".into(),
                attempts: 1,
            }),
        };
        let incident = VerificationIncident {
            incident_id: "incident-first".into(),
            project: candidate.project,
            story: StoryNo::new(1),
            generation: GlobalSeq::new(7),
            disposition: VerificationFailureDisposition::Retryable,
            halted: false,
            attempts: 1,
            detail: "head ref did not converge".into(),
            first_failed_at: STARTED.into(),
            last_failed_at: STARTED.into(),
        };
        (candidate, active, incident)
    }

    fn journal(attempt: Option<&str>) -> String {
        let mut run = serde_json::json!({"kind":"run", "generation":7, "at":STARTED});
        if let Some(attempt) = attempt {
            run["attempt_id"] = attempt.into();
        }
        format!(
            "{run}\n{}\n{}\n",
            serde_json::json!({"kind":"item", "path":"merge preflight", "status":"passed", "at":STARTED}),
            serde_json::json!({"kind":"item", "path":"release gate", "status":"running", "at":STARTED})
        )
    }

    #[test]
    fn absent_and_unowned_journals_preserve_ownership_freshness() {
        let root = storyhook_test_support::scratch_dir();
        let env = Environment::at(root.path());
        let (candidate, active, _) = fixture();
        let ordered = [candidate];
        let absent = AttemptEvidence::read(&ordered, Some(&active), &env);
        assert_eq!(absent.last_evidence_at.as_deref(), Some(STARTED));
        assert!(absent.error.is_none());
        assert!(absent.progress.is_none());

        let withdrawn = AttemptEvidence::read(&[], Some(&active), &env);
        assert_eq!(withdrawn.last_evidence_at.as_deref(), Some(STARTED));
        assert!(withdrawn.error.unwrap().contains("verifying queue"));

        let unowned = AttemptEvidence::read(&ordered, None, &env);
        assert!(unowned.last_evidence_at.is_none());
        assert!(unowned.error.is_none());
        assert!(unowned.progress.is_none());
    }

    #[test]
    fn one_read_binds_progress_and_timestamp_to_current_owner() {
        let root = storyhook_test_support::scratch_dir();
        let env = Environment::at(root.path());
        let (candidate, active, incident) = fixture();
        let path = journal_path(&env, &candidate);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let ordered = [candidate];
        for attempt in [Some("attempt-now"), None, Some("attempt-before")] {
            std::fs::write(&path, journal(attempt)).unwrap();
            let evidence = AttemptEvidence::read(&ordered, Some(&active), &env);
            if attempt == Some("attempt-before") {
                assert!(evidence.progress.is_none());
                assert!(
                    evidence
                        .error
                        .unwrap()
                        .contains("active generation and attempt")
                );
                assert_eq!(evidence.last_evidence_at.as_deref(), Some(STARTED));
            } else {
                let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
                let expected = crate::service::gate_output::metadata_time(modified)
                    .unwrap()
                    .to_rfc3339();
                assert_eq!(
                    evidence.last_evidence_at.as_deref(),
                    Some(expected.as_str())
                );
                assert!(evidence.progress.is_some());
                assert!(evidence.error.is_none());
                assert_eq!(
                    evidence.incident_is_current(&ordered, Some(&active), Some(&incident)),
                    attempt.is_none()
                );
            }
        }
    }

    #[test]
    fn unreadable_journal_has_context_and_no_freshness() {
        let root = storyhook_test_support::scratch_dir();
        let env = Environment::at(root.path());
        let (candidate, active, _) = fixture();
        let path = journal_path(&env, &candidate);
        std::fs::create_dir_all(&path).unwrap();
        let evidence = AttemptEvidence::read(&[candidate], Some(&active), &env);
        assert!(
            evidence
                .error
                .unwrap()
                .contains(&path.display().to_string())
        );
        assert!(evidence.last_evidence_at.is_none());
        assert!(evidence.progress.is_none());
    }

    #[test]
    fn only_unchanged_retry_incident_for_exact_owned_submission_is_history() {
        let (candidate, active, incident) = fixture();
        let evidence = AttemptEvidence {
            progress: Some(gate_progress::fold(&journal(Some("attempt-now")))),
            ..AttemptEvidence::default()
        };
        assert!(!evidence.incident_is_current(
            std::slice::from_ref(&candidate),
            Some(&active),
            Some(&incident)
        ));
        assert!(!evidence.incident_is_current(&[], None, None));
        assert!(evidence.incident_is_current(&[], Some(&active), Some(&incident)));
        assert!(evidence.incident_is_current(
            std::slice::from_ref(&candidate),
            None,
            Some(&incident)
        ));
        for mismatch in [
            "candidate-project",
            "candidate-story",
            "candidate-generation",
            "incident-project",
            "incident-story",
            "incident-generation",
            "new-failure",
            "new-incident",
            "halted",
            "permanent",
            "no-origin",
            "no-generation",
            "wrong-attempt",
            "no-progress",
        ] {
            let mut candidate = candidate.clone();
            let mut active = active.clone();
            let mut incident = incident.clone();
            let mut evidence = AttemptEvidence {
                progress: evidence.progress.clone(),
                ..AttemptEvidence::default()
            };
            match mismatch {
                "candidate-project" => candidate.project = ProjectId::new(2),
                "candidate-story" => candidate.story_id = "SH-2".into(),
                "candidate-generation" => candidate.verifying_generation = Some(GlobalSeq::new(8)),
                "incident-project" => incident.project = ProjectId::new(2),
                "incident-story" => incident.story = StoryNo::new(2),
                "incident-generation" => incident.generation = GlobalSeq::new(8),
                "new-failure" => incident.attempts += 1,
                "new-incident" => incident.incident_id = "replacement-incident".into(),
                "halted" => incident.halted = true,
                "permanent" => incident.disposition = VerificationFailureDisposition::Permanent,
                "no-origin" => active.retry_origin = None,
                "no-generation" => {
                    active.generation = None;
                    candidate.verifying_generation = None;
                }
                "wrong-attempt" => active.attempt_id = "different-attempt".into(),
                "no-progress" => evidence.progress = None,
                _ => unreachable!(),
            }
            assert!(
                evidence.incident_is_current(&[candidate], Some(&active), Some(&incident)),
                "{mismatch}"
            );
        }
    }
}
