//! One verifier snapshot shared by CLI and dashboard consumers.

use super::*;
use crate::store::{VerificationRecovery, VerificationRecoveryOutcome};
use serde::{Deserialize, Serialize};

/// Project-scoped verifier facts; admission and infrastructure are independent.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerifierStatus {
    /// Project slug used by CLI selection and dashboard routes.
    pub project: String,
    /// Manual admission and live cancellation state.
    pub control: VerificationControlState,
    /// Current infrastructure failure, if any.
    pub incident: Option<VerificationIncident>,
    /// Display identity of the incident's first-hit story.
    pub first_hit_story: Option<String>,
    /// Seconds since the first failure.
    pub incident_age_seconds: Option<u64>,
    /// Retries after the initial failed attempt.
    pub retry_count: u32,
    /// Ordered verifying story identities, including any owned candidate.
    pub verifying: Vec<String>,
    /// Stories held by stopped admission or an infrastructure incident.
    pub held_stories: Vec<String>,
    /// Process-local ownership; never inferred from queue rank.
    pub active: Option<ActiveVerification>,
    /// Latest durable acknowledgement and recovery request.
    pub recovery: VerificationRecovery,
    /// Receipt of the command being answered; absent on ordinary status reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_receipt: Option<VerificationRecovery>,
    /// Timestamp selected from matching evidence, ownership acquisition, or queue entry.
    pub last_evidence_at: Option<String>,
    /// Seconds since matching progress, ownership acquisition, or unowned queue entry.
    pub silence_seconds: Option<u64>,
    /// Diagnostic when progress cannot be inspected.
    pub evidence_error: Option<String>,
    /// One concise actionable unhealthy-queue notice.
    pub warning: Option<String>,
}

impl VerificationActivity {
    /// Reads one consistent ownership/store snapshot and matching journal evidence.
    pub fn status(&self, ctx: &Ctx<'_, impl Store>) -> Result<VerifierStatus, AppError> {
        self.read_project(ctx.store(), ctx.project(), |tx, active, control| {
            snapshot(tx, ctx, active, control)
        })
        .map_err(Into::into)
    }
}

pub(crate) fn snapshot(
    tx: &impl ReadOps,
    ctx: &Ctx<'_, impl Store>,
    active: Option<&ActiveVerification>,
    control: VerificationControlState,
) -> Result<VerifierStatus, crate::store::StoreError> {
    use crate::service::engine::elapsed_secs;
    let now = ctx.now();
    let project = tx
        .project(ctx.project())?
        .ok_or_else(|| AppError::NotFound(format!("project {}", ctx.project())))?;
    let ordered = crate::service::verification::ordered_candidates_for(tx, ctx.project())?;
    let incident = tx.verification_incident(ctx.project())?;
    let recovery = tx.verification_recovery(ctx.project())?;
    let first_hit_story = incident.as_ref().map(|i| i.story.to_id(&project.prefix));
    let incident_age_seconds = incident
        .as_ref()
        .and_then(|i| elapsed_secs(&i.first_failed_at, &now));
    let retry_count = incident
        .as_ref()
        .map_or(0, |i| i.attempts.saturating_sub(1));
    let verifying: Vec<String> = ordered.iter().map(|c| c.story_id.clone()).collect();
    let stopped = control != VerificationControlState::Running;
    let held_stories = if stopped || incident.is_some() {
        verifying.clone()
    } else {
        Vec::new()
    };
    let mut evidence_error = None;
    let mut last_evidence_at = None;
    let silence_seconds = if let Some(active) = active {
        let candidate = ordered
            .iter()
            .find(|c| c.story_id == active.story_id && c.verifying_generation == active.generation);
        match candidate {
            Some(candidate) => {
                let path = journal_path(ctx.env(), candidate);
                match std::fs::File::open(&path) {
                    Ok(mut file) => {
                        use std::io::Read;
                        let mut text = String::new();
                        match file
                            .read_to_string(&mut text)
                            .and_then(|_| file.metadata()?.modified())
                        {
                            Ok(modified) => {
                                let progress = crate::service::gate_progress::fold(&text);
                                if progress.run.is_some_and(|r| {
                                    Some(r.generation) == active.generation.map(|g| g.get())
                                }) {
                                    let modified: chrono::DateTime<chrono::Utc> = modified.into();
                                    let at = modified.to_rfc3339();
                                    last_evidence_at = Some(at.clone());
                                    elapsed_secs(&at, &now)
                                } else {
                                    evidence_error = Some(
                                        "journal does not identify the active generation".into(),
                                    );
                                    last_evidence_at = Some(active.started_at.clone());
                                    elapsed_secs(&active.started_at, &now)
                                }
                            }
                            Err(error) => {
                                evidence_error =
                                    Some(format!("cannot inspect {}: {error}", path.display()));
                                None
                            }
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        last_evidence_at = Some(active.started_at.clone());
                        elapsed_secs(&active.started_at, &now)
                    }
                    Err(error) => {
                        evidence_error = Some(format!("cannot open {}: {error}", path.display()));
                        None
                    }
                }
            }
            None => {
                evidence_error =
                    Some("owned generation is no longer in the verifying queue".into());
                last_evidence_at = Some(active.started_at.clone());
                elapsed_secs(&active.started_at, &now)
            }
        }
    } else {
        ordered
            .iter()
            .filter_map(|c| c.verifying_since.as_deref())
            .min()
            .or_else(|| {
                recovery
                    .request
                    .as_ref()
                    .filter(|r| r.outcome == VerificationRecoveryOutcome::Scheduled)
                    .map(|r| r.requested_at.as_str())
            })
            .inspect(|at| last_evidence_at = Some((*at).into()))
            .and_then(|at| elapsed_secs(at, &now))
    };
    if silence_seconds.is_none()
        && (active.is_some() || !ordered.is_empty())
        && evidence_error.is_none()
    {
        evidence_error =
            Some("evidence age unavailable (missing timestamp or clock moved backwards)".into());
    }
    let warning = if let Some(i) = incident.as_ref().filter(|i| i.halted) {
        Some(format!(
            "{} verifier HALTED: incident {}, {} held; story verifier ack {}; story daemon logs",
            project.slug,
            i.incident_id,
            held_stories.len(),
            i.incident_id
        ))
    } else if stopped && !verifying.is_empty() {
        Some(format!(
            "{} verifier {:?}: {} verifying; story verifier start; story verifier status",
            project.slug,
            control,
            verifying.len()
        ))
    } else if let Some(error) = &evidence_error {
        Some(format!(
            "{} verifier evidence unavailable: {}; story verifier status; story daemon logs",
            project.slug,
            error.replace(['\n', '\r'], " ")
        ))
    } else if let Some(seconds) = silence_seconds
        .filter(|s| *s > super::super::verification_progress::PUBLISH_INTERVAL.as_secs())
    {
        Some(format!(
            "{} verifier has no progress evidence for {seconds}s; story verifier status; story daemon logs",
            project.slug
        ))
    } else {
        None
    };
    Ok(VerifierStatus {
        project: project.slug,
        control,
        incident,
        first_hit_story,
        incident_age_seconds,
        retry_count,
        verifying,
        held_stories,
        active: active.cloned(),
        recovery,
        command_receipt: None,
        last_evidence_at,
        silence_seconds,
        evidence_error,
        warning,
    })
}

impl VerifierStatus {
    /// Human description rendered in the client's timezone.
    pub fn render_human(&self) -> String {
        let mut text = format!(
            "queue {}: {} verifying\n",
            if self.incident.as_ref().is_some_and(|i| i.halted) {
                "halted"
            } else {
                match self.control {
                    VerificationControlState::Running => "running",
                    VerificationControlState::Draining => "draining",
                    VerificationControlState::Stopping => "stopping",
                    VerificationControlState::Stopped => "stopped",
                }
            },
            self.verifying.len()
        );
        if let Some(i) = &self.incident {
            text.push_str(&format!("Incident {}: {}; first hit {}; age {}s; {} attempts ({} retries); unacknowledged\n{}\nHeld: {}\n",
                i.incident_id, if i.halted { "HALTED" } else { "retrying" }, self.first_hit_story.as_deref().unwrap_or("unknown"),
                self.incident_age_seconds.map_or_else(|| "unknown".into(), |n| n.to_string()), i.attempts, self.retry_count, i.detail, self.held_stories.join(", ")));
        }
        if let Some(active) = &self.active {
            text.push_str(&format!(
                "Attempt {}: gate on {} since {}\n",
                active.attempt_id,
                active.story_id,
                crate::local_time::stamp(&active.started_at)
            ));
        }
        if let Some(at) = &self.last_evidence_at {
            text.push_str(&format!(
                "Last evidence: {} ({}s ago)\n",
                crate::local_time::stamp(at),
                self.silence_seconds
                    .map_or_else(|| "unknown".into(), |s| s.to_string())
            ));
        }
        let receipt = self.command_receipt.as_ref().unwrap_or(&self.recovery);
        if let Some(ack) = &receipt.acknowledgement {
            text.push_str(&format!(
                "Acknowledged {} at {}; admission {}\n",
                ack.incident.incident_id,
                crate::local_time::stamp(&ack.at),
                if ack.enabled {
                    "enabled"
                } else {
                    "left stopped; story verifier start"
                }
            ));
        }
        if let Some(request) = &receipt.request {
            let state = match &request.outcome {
                VerificationRecoveryOutcome::Scheduled => "scheduled".into(),
                VerificationRecoveryOutcome::Admitted => "admitted".into(),
                VerificationRecoveryOutcome::Settled { reason, detail } => {
                    format!("{reason}: {detail}")
                }
            };
            text.push_str(&format!("Recovery request {}: {state}\n", request.id));
            if let Some(admission) = &request.admission {
                text.push_str(&format!(
                    "Caused attempt {} on {} at {}\n",
                    admission.attempt_id,
                    admission.story_id,
                    crate::local_time::stamp(&admission.started_at)
                ));
            }
        }
        if let Some(warning) = &self.warning {
            text.push_str(&format!("warning: {warning}\n"));
        }
        text
    }
}
