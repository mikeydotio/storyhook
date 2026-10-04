//! Durable invariants for append-only causal evidence.

use super::*;
use crate::store::StoreError;
use std::collections::BTreeSet;

impl AttributionRecord {
    /// Reject malformed observations before storage or interpretation.
    pub(crate) fn validate(&self) -> Result<(), StoreError> {
        let invalid =
            |why: &str| StoreError::Validation(format!("invalid attribution {}: {why}", self.id));
        if self.version != 1
            || self.revision < 0
            || self.submission.project.get() <= 0
            || [&self.id, &self.attempt, &self.submission.story_id]
                .into_iter()
                .any(|s| s.trim().is_empty())
            || chrono::DateTime::parse_from_rfc3339(&self.created_at).is_err()
            || self.held == self.retired.is_some()
            || self.retired.as_ref().is_some_and(|s| s.trim().is_empty())
        {
            return Err(invalid("identity, timestamp or hold state is inconsistent"));
        }
        let mut components = BTreeSet::new();
        if self.components.is_empty()
            || self.components.iter().any(|c| {
                !components.insert(&c.id)
                    || [&c.id, &c.check, &c.signature, &c.requirement, &c.log]
                        .into_iter()
                        .any(|s| s.trim().is_empty())
                    || matches!(
                        c.observed_cause,
                        FailureCause::CandidateCaused | FailureCause::SharedProject
                    )
            })
        {
            return Err(invalid(
                "failures must be distinct observations, not causal assertions",
            ));
        }
        let mut planned = BTreeSet::new();
        if self.plans.iter().any(|p| !components.contains(&p.component) || !planned.insert(&p.component)
            || [&p.base, &p.candidate_tree, &p.control_tree, &p.detector].into_iter().any(|s| s.trim().is_empty())
            || p.argv.is_empty() || p.argv.iter().any(|s| s.trim().is_empty())
            || matches!(&p.relation, DetectorRelation::Transplant { patch } | DetectorRelation::Ablation { patch } if patch.trim().is_empty())) {
            return Err(invalid("contrast plans need one known component and complete immutable inputs"));
        }
        let mut ids = BTreeSet::new();
        let mut executions = BTreeSet::new();
        let mut elapsed = 0u64;
        if let Some(preparation) = &self.preparation {
            if chrono::DateTime::parse_from_rfc3339(&preparation.started_at).is_err() {
                return Err(invalid("preparation reservation needs a valid timestamp"));
            }
            if let Some(result) = &preparation.completed {
                if result.log.trim().is_empty() || result.detail.trim().is_empty() {
                    return Err(invalid(
                        "completed preparation needs raw output and diagnostics",
                    ));
                }
                elapsed = result.milliseconds;
            }
        }
        for (index, probe) in self.probes.iter().enumerate() {
            if probe.id.trim().is_empty()
                || !ids.insert(&probe.id)
                || probe.plan >= self.plans.len()
                || chrono::DateTime::parse_from_rfc3339(&probe.started_at).is_err()
                || (probe.completed.is_none() && index + 1 != self.probes.len())
            {
                return Err(invalid(
                    "probe reservation is malformed, duplicate or out of order",
                ));
            }
            if let Some(result) = &probe.completed {
                if result.execution_id.trim().is_empty()
                    || !executions.insert(&result.execution_id)
                    || result.log.trim().is_empty()
                {
                    return Err(invalid(
                        "completed probes need distinct physical execution and output references",
                    ));
                }
                elapsed = elapsed
                    .checked_add(result.milliseconds)
                    .ok_or_else(|| invalid("diagnosis duration overflow"))?;
            }
        }
        if self.probes.len() > MAX_PROBES || self.diagnosis_ms < elapsed {
            return Err(invalid(
                "probe allowance or retained elapsed time is inconsistent",
            ));
        }
        for assessment in &self.assessments {
            if !components.contains(&assessment.component)
                || assessment.evidence_revision < 0
                || assessment.evidence_revision >= self.revision
                || assessment.detail.trim().is_empty()
                || assessment.probes.iter().collect::<BTreeSet<_>>().len()
                    != assessment.probes.len()
            {
                return Err(invalid("assessment references are inconsistent"));
            }
            let mut prefix = self.clone();
            prefix.assessments.clear();
            let count = if let Some(last) = assessment.probes.last() {
                self.probes
                    .iter()
                    .position(|p| &p.id == last)
                    .ok_or_else(|| invalid("assessment names an absent probe"))?
                    + 1
            } else {
                0
            };
            prefix.probes.truncate(count);
            let component = prefix
                .components
                .iter()
                .find(|c| c.id == assessment.component)
                .expect("checked component");
            let selected: Vec<_> = prefix
                .probes
                .iter()
                .filter(|p| prefix.plans[p.plan].component == component.id)
                .map(|p| p.id.clone())
                .collect();
            if selected != assessment.probes
                || super::contrast::classify_evidence(&prefix, component) != assessment.cause
            {
                return Err(invalid(
                    "assessment cause is not supported by its complete retained probe history",
                ));
            }
        }
        Ok(())
    }

    /// Whether the next revision preserves immutable observations and consumed allowance.
    pub(crate) fn preserved_by(&self, next: &Self) -> bool {
        let mut same_revision = next.clone();
        same_revision.revision = self.revision;
        if self.retired.is_some() {
            return self == &same_revision;
        }
        self.id == next.id
            && self.version == next.version
            && self.submission == next.submission
            && self.attempt == next.attempt
            && self.inputs == next.inputs
            && self.created_at == next.created_at
            && self.components == next.components
            && self.preparation.as_ref().is_none_or(|old| {
                next.preparation
                    .as_ref()
                    .is_some_and(|new| old.preserved_by(new))
            })
            && next.plans.starts_with(&self.plans)
            && next.assessments.starts_with(&self.assessments)
            && next.assessments[self.assessments.len()..]
                .iter()
                .all(|assessment| {
                    let probes: Vec<_> = self
                        .probes
                        .iter()
                        .filter(|probe| self.plans[probe.plan].component == assessment.component)
                        .collect();
                    assessment.evidence_revision == self.revision
                        && probes.iter().all(|probe| probe.completed.is_some())
                        && probes
                            .iter()
                            .map(|probe| &probe.id)
                            .eq(assessment.probes.iter())
                        && self
                            .components
                            .iter()
                            .find(|component| component.id == assessment.component)
                            .is_some_and(|component| {
                                super::contrast::classify_evidence(self, component)
                                    == assessment.cause
                            })
                })
            && next.diagnosis_ms >= self.diagnosis_ms
            && next.probes.len() >= self.probes.len()
            && self.probes.iter().zip(&next.probes).all(|(old, new)| {
                old.id == new.id
                    && old.plan == new.plan
                    && old.side == new.side
                    && old.started_at == new.started_at
                    && (old.completed.is_none() || old.completed == new.completed)
            })
    }
}
