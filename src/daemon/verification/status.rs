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
    /// Retained infrastructure failure, if any; consult `incident_is_current`
    /// before treating it as a current blocker.
    pub incident: Option<VerificationIncident>,
    /// Whether the retained incident currently blocks work rather than describes
    /// an earlier failure of the authenticated retry. Legacy payloads fail closed.
    #[serde(default = "incident_current_default")]
    pub incident_is_current: bool,
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
    /// Project fault work, independent of infrastructure admission control.
    #[serde(default)]
    pub project_recoveries: Vec<crate::service::project_recovery::RecoveryStatus>,
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
            snapshot(tx, ctx, active, control).map(|(status, _)| status)
        })
        .map_err(Into::into)
    }
}

pub(crate) fn snapshot(
    tx: &impl ReadOps,
    ctx: &Ctx<'_, impl Store>,
    active: Option<&ActiveVerification>,
    control: VerificationControlState,
) -> Result<
    (
        VerifierStatus,
        Vec<crate::daemon::verification_progress::StoryVerificationStatus>,
    ),
    crate::store::StoreError,
> {
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
    let evidence = super::evidence::AttemptEvidence::read(&ordered, active, ctx.env());
    let incident_is_current = evidence.incident_is_current(&ordered, active, incident.as_ref());
    let statuses = crate::daemon::verification_progress::status_snapshot_with_evidence(
        &ordered,
        active,
        incident.as_ref(),
        &now,
        &evidence,
        incident_is_current,
    );
    let stopped = control != VerificationControlState::Running;
    let held_stories = if stopped || incident_is_current {
        verifying.clone()
    } else {
        Vec::new()
    };
    let mut evidence_error = evidence.error;
    let mut last_evidence_at = evidence.last_evidence_at;
    let silence_seconds = if active.is_some() {
        last_evidence_at
            .as_deref()
            .and_then(|at| elapsed_secs(at, &now))
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
    let warning = if let Some(i) = incident
        .as_ref()
        .filter(|i| incident_is_current && i.halted)
    {
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
    Ok((
        VerifierStatus {
            project: project.slug,
            control,
            incident,
            incident_is_current,
            first_hit_story,
            incident_age_seconds,
            retry_count,
            verifying,
            held_stories,
            active: active.cloned(),
            recovery,
            project_recoveries: crate::service::project_recovery::status_snapshot(
                tx,
                ctx.project(),
            )?,
            command_receipt: None,
            last_evidence_at,
            silence_seconds,
            evidence_error,
            warning,
        },
        statuses,
    ))
}

fn incident_current_default() -> bool {
    true
}

impl VerifierStatus {
    /// Human description rendered in the client's timezone.
    pub fn render_human(&self) -> String {
        let mut text = format!(
            "queue {}: {} verifying\n",
            if self.incident_is_current && self.incident.as_ref().is_some_and(|i| i.halted) {
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
            if self.incident_is_current {
                text.push_str(&format!("Incident {}: {}; first hit {}; age {}s; {} attempts ({} retries); unacknowledged\n{}\nHeld: {}\n",
                i.incident_id, if i.halted { "HALTED" } else { "retrying" }, self.first_hit_story.as_deref().unwrap_or("unknown"),
                self.incident_age_seconds.map_or_else(|| "unknown".into(), |n| n.to_string()), i.attempts, self.retry_count, i.detail, self.held_stories.join(", ")));
            } else {
                text.push_str(&format!("Previous infrastructure failure; current retry running. Incident {}; first hit {}; {} attempts ({} retries).\n{}\n",
                    i.incident_id, self.first_hit_story.as_deref().unwrap_or("unknown"), i.attempts, self.retry_count, i.detail));
            }
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
        for recovery in &self.project_recoveries {
            text.push_str(&format!("Project recovery {}: {} at {}; {}\nAffected: {}; assessor {}; repair {}; completed attempts {}/{}\nNext: {}\nInspect: story verifier repair show {} --json\n",
                recovery.id, recovery.fault, recovery.locus, recovery.phase,
                recovery.affected_stories.join(", "), recovery.assessment_owner,
                recovery.repair_story.as_deref().unwrap_or("undecided"), recovery.completed_attempts, recovery.attempt_limit,
                recovery.next_action, recovery.id));
            if let Some(link) = &recovery.repair_link {
                text.push_str(&format!("Repair PR: {link}\n"));
            }
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
