//! Conservative comparison of exact, completed physical probes.

use super::*;
use std::collections::BTreeSet;

/// Classify retained observations without granting a repair or certification capability.
#[must_use]
pub fn classify(record: &AttributionRecord, component: &FailureComponent) -> FailureCause {
    if !record.held || record.retired.is_some() {
        return FailureCause::Unknown;
    }
    classify_evidence(record, component)
}

pub(super) fn classify_evidence(
    record: &AttributionRecord,
    component: &FailureComponent,
) -> FailureCause {
    if !record.components.contains(component) {
        return FailureCause::Unknown;
    }
    match component.observed_cause {
        FailureCause::HostExternal | FailureCause::Integration => return component.observed_cause,
        // An assertion of responsibility is not a direct observation.
        FailureCause::CandidateCaused | FailureCause::SharedProject => {
            return FailureCause::Unknown;
        }
        FailureCause::Unknown => {}
    }
    if record.submission.generation.is_none()
        || record.has_unsettled_diagnosis()
        || [
            &record.inputs.head,
            &record.inputs.base,
            &record.inputs.tree,
        ]
        .into_iter()
        .any(|v| v.as_deref().is_none_or(|s| !oid(s)))
        || component.signature.trim().is_empty()
        || record.probes.len() > MAX_PROBES
    {
        return FailureCause::Unknown;
    }
    let mut ids = BTreeSet::new();
    let mut executions = BTreeSet::new();
    if record.probes.iter().any(|p| {
        !ids.insert(&p.id)
            || p.completed
                .as_ref()
                .is_some_and(|r| !executions.insert(&r.execution_id))
    }) {
        return FailureCause::Unknown;
    }
    let plans: Vec<_> = record
        .plans
        .iter()
        .enumerate()
        .filter(|(_, p)| p.component == component.id)
        .collect();
    let [(index, plan)] = plans.as_slice() else {
        return FailureCause::Unknown;
    };
    if Some(&plan.candidate_tree) != record.inputs.tree.as_ref()
        || Some(&plan.base) != record.inputs.base.as_ref()
        || !oid(&plan.control_tree)
        || plan.control_tree == plan.candidate_tree
        || plan.detector.trim().is_empty()
        || plan.argv.is_empty()
        || plan.argv.iter().any(|s| s.trim().is_empty())
        || matches!(&plan.relation, DetectorRelation::Transplant { patch } | DetectorRelation::Ablation { patch } if patch.trim().is_empty())
    {
        return FailureCause::Unknown;
    }
    let probes: Vec<_> = record.probes.iter().filter(|p| p.plan == *index).collect();
    // Include every probe for the component; never cherry-pick a favorable retry.
    if probes.len() < 4 || probes.len() % 4 != 0 {
        return FailureCause::Unknown;
    }
    let order = [
        ProbeSide::Candidate,
        ProbeSide::Control,
        ProbeSide::Control,
        ProbeSide::Candidate,
    ];
    let mut environment: Option<&ProbeEnvironment> = None;
    let mut controls_pass = true;
    let mut controls_fail = true;
    for (index, probe) in probes.iter().enumerate() {
        if probe.side != order[index % 4] {
            return FailureCause::Unknown;
        }
        let Some(result) = &probe.completed else {
            return FailureCause::Unknown;
        };
        let tree = if probe.side == ProbeSide::Candidate {
            &plan.candidate_tree
        } else {
            &plan.control_tree
        };
        if &result.tree != tree
            || result.detector != plan.detector
            || result.executions != 1
            || !result.cleanup_complete
            || result.log.trim().is_empty()
            || result.execution_id.trim().is_empty()
        {
            return FailureCause::Unknown;
        }
        let Some(observed) = &result.environment else {
            return FailureCause::Unknown;
        };
        if !observed.supported
            || [
                &observed.toolchain,
                &observed.fixtures,
                &observed.resource_policy,
                &observed.grant,
            ]
            .into_iter()
            .any(|s| s.trim().is_empty())
        {
            return FailureCause::Unknown;
        }
        if let Some(expected) = environment {
            if observed.toolchain != expected.toolchain
                || observed.fixtures != expected.fixtures
                || observed.resource_policy != expected.resource_policy
            {
                return FailureCause::Unknown;
            }
        } else {
            environment = Some(observed);
        }
        let matching_failure = matches!(&result.outcome, ProbeOutcome::Failed { signature } if signature == &component.signature);
        if probe.side == ProbeSide::Candidate {
            if !matching_failure {
                return FailureCause::Unknown;
            }
        } else {
            controls_pass &= result.outcome == ProbeOutcome::Passed;
            controls_fail &= matching_failure;
        }
    }
    if controls_pass {
        FailureCause::CandidateCaused
    } else if controls_fail {
        FailureCause::SharedProject
    } else {
        FailureCause::Unknown
    }
}

fn oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|c| c.is_ascii_hexdigit())
}
