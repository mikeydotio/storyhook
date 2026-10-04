//! Read-only cost history. Shared physical work is never divided by members.
use crate::store::{GateAttempt, GateSubmission};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Cost retained for one submission across retries and intervening holds.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubmissionCost {
    /// Immutable submission identity, including its verifying generation.
    pub submission: GateSubmission,
    /// Admissions that contributed evidence, in admission order.
    pub attempts: Vec<String>,
    /// Last observation covered by this summary, not an assertion of closure.
    pub observed_through: Option<String>,
    /// Submission-to-last-observation wall time, including gaps and holds.
    pub wall_milliseconds: Option<u64>,
    /// Initial queue wait before the first admission; outside the budget.
    pub queue_milliseconds: Option<u64>,
    /// Sum of admission elapsed observations, not CPU time or submission wall time.
    pub admission_milliseconds: Option<u64>,
    /// Total physical gate time; unknown if any physical execution is incomplete.
    pub execution_milliseconds: Option<u64>,
    /// Sum of the completed physical execution durations that are known.
    pub known_execution_milliseconds: Option<u64>,
    /// Unique physical executions represented by this submission.
    pub executions: usize,
    /// Admission identities with sticky process-budget breaches.
    pub breaches: Vec<String>,
    /// Wall and queue intervals use UTC boundaries and are estimates.
    pub wall_estimated: bool,
}

/// Durable evidence returned by `story verifier evidence`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvidenceView {
    /// Evidence protocol version.
    pub version: u32,
    /// Requested story, including executions shared through batches.
    pub story_id: String,
    /// Unique admissions, retaining every physical execution and raw reference.
    pub attempts: Vec<GateAttempt>,
    /// Distinct submission generations; replacements do not erase history.
    pub submissions: Vec<SubmissionCost>,
}

impl EvidenceView {
    /// Selects a story's history without dividing or duplicating shared costs.
    pub fn new(story_id: &str, attempts: Vec<GateAttempt>) -> Self {
        let mut seen = BTreeSet::new();
        let attempts: Vec<_> = attempts
            .into_iter()
            .filter(|a| {
                let involved = a.submission.story_id == story_id
                    || a.executions
                        .iter()
                        .any(|e| e.submissions.iter().any(|s| s.story_id == story_id));
                involved && seen.insert(a.id.clone())
            })
            .collect();
        let mut identities = Vec::new();
        for attempt in &attempts {
            for identity in std::iter::once(&attempt.submission)
                .chain(attempt.executions.iter().flat_map(|e| &e.submissions))
            {
                if identity.story_id == story_id && !identities.contains(identity) {
                    identities.push(identity.clone());
                }
            }
        }
        let submissions = identities
            .into_iter()
            .map(|submission| summarize(submission, &attempts))
            .collect();
        Self {
            version: 1,
            story_id: story_id.into(),
            attempts,
            submissions,
        }
    }

    /// Renders separate test and budget outcomes, with unavailable values named.
    pub fn render(&self) -> String {
        let mut output = format!(
            "{} verification evidence (observation mode)\n\n| Admission | Generation | Elapsed ms | Budget | Gate result |\n|---|---|---:|---|---|\n",
            self.story_id
        );
        for attempt in &self.attempts {
            output.push_str(&format!(
                "| {} | {} | {}{} | {} | {} |\n",
                attempt.id,
                attempt
                    .submission
                    .generation
                    .map_or_else(|| "unknown".into(), |g| g.get().to_string()),
                attempt.elapsed.milliseconds,
                if attempt.elapsed.estimated {
                    " (estimated)"
                } else {
                    ""
                },
                attempt.budget_status(),
                attempt.verdict.as_deref().unwrap_or("unknown")
            ));
        }
        for submission in &self.submissions {
            let number =
                |value: Option<u64>| value.map_or_else(|| "unknown".into(), |n| n.to_string());
            output.push_str(&format!("\nGeneration {}: wall {} ms (UTC estimate), queue {} ms, admission cost {} ms, physical gate cost {} ms; {} breach(es). Observed through {}.\n",
                submission.submission.generation.map_or_else(|| "unknown".into(), |g| g.get().to_string()),
                number(submission.wall_milliseconds), number(submission.queue_milliseconds),
                number(submission.admission_milliseconds), number(submission.execution_milliseconds),
                submission.breaches.len(), submission.observed_through.as_deref().unwrap_or("unknown")));
        }
        if self.attempts.is_empty() {
            output.push_str("\nNo retained admission evidence.\n");
        }
        output
    }
}

fn summarize(submission: GateSubmission, all: &[GateAttempt]) -> SubmissionCost {
    let attempts: Vec<_> = all
        .iter()
        .filter(|a| {
            a.submission == submission
                || a.executions
                    .iter()
                    .any(|e| e.submissions.contains(&submission))
        })
        .collect();
    let observed_through = attempts
        .iter()
        .map(|a| a.elapsed.checkpoint_at.as_str())
        .filter_map(|s| {
            chrono::DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|at| (at, s))
        })
        .max_by_key(|(at, _)| *at)
        .map(|(_, s)| s.to_string());
    let wall_milliseconds = submission
        .submitted_at
        .as_deref()
        .zip(observed_through.as_deref())
        .and_then(|(start, end)| super::utc_milliseconds(start, end));
    let queue_milliseconds = submission
        .submitted_at
        .as_deref()
        .zip(attempts.first())
        .and_then(|(start, a)| super::utc_milliseconds(start, &a.admitted_at));
    let admission_milliseconds = attempts
        .iter()
        .try_fold(0_u64, |sum, a| sum.checked_add(a.elapsed.milliseconds));
    let mut seen = BTreeSet::new();
    let executions: Vec<_> = attempts
        .iter()
        .flat_map(|a| {
            a.executions.iter().filter(|e| {
                e.submissions.contains(&submission)
                    || (e.submissions.is_empty() && a.submission == submission)
            })
        })
        .filter(|e| seen.insert(&e.id))
        .collect();
    let execution_milliseconds = executions
        .iter()
        .try_fold(0_u64, |sum, e| sum.checked_add(e.milliseconds?));
    let known_execution_milliseconds = executions
        .iter()
        .filter_map(|e| e.milliseconds)
        .try_fold(0_u64, u64::checked_add);
    SubmissionCost {
        submission,
        attempts: attempts.iter().map(|a| a.id.clone()).collect(),
        observed_through,
        wall_milliseconds,
        queue_milliseconds,
        admission_milliseconds,
        execution_milliseconds,
        known_execution_milliseconds,
        executions: executions.len(),
        breaches: attempts
            .iter()
            .filter(|a| a.elapsed.breached_at.is_some())
            .map(|a| a.id.clone())
            .collect(),
        wall_estimated: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{GateExecution, GlobalSeq, ProjectId};

    fn attempt(id: &str, start: &str, end: &str, ms: u64) -> GateAttempt {
        let mut a = GateAttempt::new(
            id.into(),
            GateSubmission {
                project: ProjectId::new(1),
                story_id: "SH-1".into(),
                generation: Some(GlobalSeq::new(7)),
                submitted_at: Some("2026-10-03T00:00:00Z".into()),
            },
            start,
        );
        a.elapsed.observe(ms, end);
        a.finished_at = Some(end.into());
        a
    }

    #[test]
    fn retries_keep_the_breach_and_wall_time_includes_gaps_without_summing_child_spans() {
        let a = attempt("a", "2026-10-03T00:01:00Z", "2026-10-03T00:16:00Z", 900_000);
        let mut b = attempt("b", "2026-10-03T00:20:00Z", "2026-10-03T00:21:00Z", 60_000);
        b.verdict = Some("certified".into());
        let view = EvidenceView::new("SH-1", vec![a.clone(), a, b]);
        assert_eq!(view.attempts.len(), 2);
        let cost = &view.submissions[0];
        assert_eq!(cost.wall_milliseconds, Some(1_260_000));
        assert_eq!(cost.queue_milliseconds, Some(60_000));
        assert_eq!(cost.admission_milliseconds, Some(960_000));
        assert_eq!(cost.breaches, ["a"]);
        assert!(view.render().contains("process-budget-breach"));
    }

    #[test]
    fn shared_execution_is_referenced_at_full_cost_and_missing_duration_is_unknown() {
        let mut a = attempt("a", "2026-10-03T00:01:00Z", "2026-10-03T00:16:00Z", 900_000);
        let mut other = a.submission.clone();
        other.story_id = "SH-2".into();
        let mut gate = GateExecution::new("shared".into(), &a.admitted_at, "/tmp/gate".into());
        gate.submissions = vec![a.submission.clone(), other];
        gate.milliseconds = Some(900_000);
        a.executions.push(gate.clone());
        for story in ["SH-1", "SH-2"] {
            let view = EvidenceView::new(story, vec![a.clone()]);
            assert_eq!(view.submissions[0].execution_milliseconds, Some(900_000));
        }
        gate.id = "unfinished".into();
        gate.milliseconds = None;
        a.executions.push(gate);
        let view = EvidenceView::new("SH-2", vec![a]);
        assert_eq!(view.submissions[0].execution_milliseconds, None);
        assert_eq!(
            view.submissions[0].known_execution_milliseconds,
            Some(900_000)
        );
        assert!(view.render().contains("physical gate cost unknown"));
    }
}
