//! Completed gate evidence remains distinct from permission to reuse its owner.

use super::*;
use crate::service::gate_command::GateCommand;

/// Resources whose post-command cleanup could not establish quiescence.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct VerificationCleanupFailure {
    /// The cleanup stage that refused.
    pub phase: String,
    /// Diagnosis including the originating OS failure.
    pub detail: String,
    /// Retained owner record when reported by the wrapper; never removal authority.
    pub owner: Option<String>,
    /// Retained shared verification checkout, if reported before capture failed.
    pub worktree: Option<String>,
    /// Cleanup uncertainty is permanent until recovery establishes safety.
    pub disposition: VerificationFailureDisposition,
}

/// A completed command or merge whose cleanup subsequently failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompletedVerification {
    /// The gate command reported failures.
    TestsFailed {
        /// Exact judged merge tree.
        tree: String,
        /// Full attempt log.
        log: String,
        /// Bounded failure summary.
        detail: String,
        /// Configured gate command.
        gate: String,
    },
    /// The gate command passed, but landing was not attempted.
    GatePassed {
        /// Exact judged merge tree.
        tree: String,
        /// Full attempt log.
        log: String,
        /// Execution diagnosis; no certification is implied.
        detail: String,
        /// Configured gate command.
        gate: String,
    },
    /// The exact head and tree passed, but cleanup withheld landing permission.
    Certified {
        /// Exact submitted head.
        head: String,
        /// Exact merged tree.
        tree: String,
        /// Guarded merge diagnosis.
        detail: String,
        /// Gate command that certified the tree.
        gate: String,
    },
}

/// Completion retires queue membership, but not an unacknowledged cleanup halt.
pub(super) fn retains_merged_incident(
    store: &impl Store,
    incident: &crate::store::VerificationIncident,
) -> Result<bool, AppError> {
    Ok(store.read(|tx| {
        let Some(row) = tx.story(incident.project, incident.story)? else { return Ok(false); };
        if crate::domain::is_human_only(&row.snapshot)
            || row.state != crate::domain::COMPLETION_STATE_SLUG
            || crate::service::verification::verifying_entry(tx, incident.project, incident.story)?.map(|(_, generation)| generation) != Some(incident.generation) {
            return Ok(false);
        }
        Ok(tx.events_for(incident.project, incident.story)?.iter().any(|event| {
            event.global_seq > incident.generation && matches!(event.known(),
                Some(crate::domain::StoryEvent::StoryCommentAdded { text, .. }) if text.starts_with(VERIFICATION_GREEN_PREFIX))
        }))
    })?)
}

/// Reject incomplete cleanup metadata rather than silently treating it as success.
pub(super) fn outcome(
    verdict: CompletedVerification,
    cleanup: VerificationCleanupFailure,
) -> VerificationOutcome {
    if cleanup.disposition != VerificationFailureDisposition::Permanent
        || [
            cleanup.phase.as_str(),
            cleanup.detail.as_str(),
            cleanup.owner.as_deref().unwrap_or_default(),
            cleanup.worktree.as_deref().unwrap_or_default(),
        ]
        .iter()
        .any(|text| text.trim().is_empty())
    {
        return VerificationOutcome::InfrastructureFailure {
            detail: format!("invalid completed-verification cleanup metadata: {cleanup:?}"),
            disposition: VerificationFailureDisposition::Permanent,
        };
    }
    VerificationOutcome::CleanupFailed { verdict, cleanup }
}

/// Preserve a complete answer while retaining the independent capture failure.
pub(super) fn interrupted_outcome(
    stdout: &[u8],
    capture_detail: &str,
    checkout: &std::path::Path,
) -> Option<VerificationOutcome> {
    if let Ok(WireOutcome::RepairDeferred {
        recovery_id,
        reason,
        cleanup_failure,
    }) = serde_json::from_slice(stdout)
    {
        if recovery_id.trim().is_empty() {
            return None;
        }
        return Some(VerificationOutcome::InfrastructureFailure {
            detail: format!(
                "repair refusal disposition withheld after capture failure: {capture_detail}; recovery={recovery_id}; reason={reason:?}; registered source: {}; cleanup: {cleanup_failure:?}",
                checkout.display()
            ),
            disposition: VerificationFailureDisposition::Permanent,
        });
    }
    if let Ok(WireOutcome::ProjectFault {
        fault,
        cleanup_failure,
    }) = serde_json::from_slice(stdout)
    {
        fault.validate().ok()?;
        return Some(VerificationOutcome::InfrastructureFailure {
            detail: format!(
                "project fault repair withheld after capture failure: {capture_detail}; registered source: {}; cleanup: {cleanup_failure:?}; retained evidence: {fault:?}",
                checkout.display()
            ),
            disposition: VerificationFailureDisposition::Permanent,
        });
    }
    // This envelope also admits a bare gate-passed from an interrupted wrapper;
    // the ordinary wire parser still requires cleanup metadata for that result.
    #[derive(Deserialize)]
    struct Answer {
        result: String,
        gate: String,
        tree: String,
        detail: String,
        log: Option<String>,
        head: Option<String>,
        cleanup_failure: Option<VerificationCleanupFailure>,
    }
    let answer: Answer = serde_json::from_slice(stdout).ok()?;
    if answer.tree.trim().is_empty() || answer.detail.trim().is_empty() {
        return None;
    }
    let gate = GateCommand::parse(&answer.gate).ok()?;
    let verdict = match answer.result.as_str() {
        "tests-failed" | "gate-passed" => {
            let log = answer.log.filter(|log| !log.trim().is_empty())?;
            if answer.result == "tests-failed" {
                CompletedVerification::TestsFailed {
                    tree: answer.tree,
                    log,
                    detail: answer.detail,
                    gate: gate.display(),
                }
            } else {
                CompletedVerification::GatePassed {
                    tree: answer.tree,
                    log,
                    detail: answer.detail,
                    gate: gate.display(),
                }
            }
        }
        "certified" => CompletedVerification::Certified {
            head: answer.head.filter(|head| !head.trim().is_empty())?,
            tree: answer.tree,
            detail: answer.detail,
            gate: gate.display(),
        },
        _ => return None,
    };
    let capture_detail = format!(
        "verify-pr.sh capture failed after a completed answer: {capture_detail}. Registered source checkout: {}. Wrapper termination/reaping was attempted; shared owner recovery is still required.",
        checkout.display()
    );
    if let Some(cleanup) = answer.cleanup_failure {
        let VerificationOutcome::CleanupFailed {
            verdict,
            mut cleanup,
        } = outcome(verdict, cleanup)
        else {
            return None;
        };
        cleanup.detail.push_str(&format!("\n{capture_detail}"));
        return Some(VerificationOutcome::CleanupFailed { verdict, cleanup });
    }
    Some(VerificationOutcome::CleanupFailed {
        verdict,
        cleanup: VerificationCleanupFailure {
            phase: "daemon process capture".into(),
            detail: capture_detail,
            owner: None,
            worktree: None,
            disposition: VerificationFailureDisposition::Permanent,
        },
    })
}

pub(super) fn record<S: Store>(
    queue: &VerificationQueue<'_, S>,
    ctx: &Ctx<'_, S>,
    candidate: &VerificationCandidate,
    pull_request: &str,
    verdict: CompletedVerification,
    cleanup: VerificationCleanupFailure,
) -> Result<GenerationWrite<TickResult>, AppError> {
    let comment = match verdict {
        CompletedVerification::TestsFailed {
            tree,
            log,
            detail,
            gate,
        } => format!(
            "CENTRAL VERIFICATION RED — merge tree `{tree}` failed `{gate}`. Full log: `{log}`. Cleanup must finish before remediation dispatch.\n\n{}",
            crate::text_lint::quote_evidence(&detail)
        ),
        CompletedVerification::GatePassed {
            tree,
            log,
            detail,
            gate,
        } => format!(
            "CENTRAL VERIFICATION GATE PASSED — command `{gate}` exited successfully on merge tree `{tree}`. Full log: `{log}`. This records execution only. No pull request was landed.\n\n{}",
            crate::text_lint::quote_evidence(&detail)
        ),
        CompletedVerification::Certified {
            head,
            tree,
            detail,
            gate,
        } => format!(
            "CENTRAL VERIFICATION CERTIFIED — head `{head}` and merge tree `{tree}` passed `{gate}`. Cleanup must finish before landing pull request {pull_request}.\n\n{}",
            crate::text_lint::quote_evidence(&detail)
        ),
    };
    let diagnosis = format!(
        "{}: {}\nOwner retained: {}\nWorktree retained: {}\nQuiescence/recovery was not established; do not clear ownership or remove evidence without recovery checks.",
        cleanup.phase,
        cleanup.detail,
        cleanup
            .owner
            .as_deref()
            .unwrap_or("path not reported by wrapper"),
        cleanup
            .worktree
            .as_deref()
            .unwrap_or("path not reported by wrapper")
    );
    match queue.record_generation_completed(ctx, candidate, &comment, Some(&diagnosis))? {
        GenerationWrite::Applied(Some(incident)) => {
            fire_verification_halted(ctx, candidate, &incident)?;
            Ok(GenerationWrite::Applied(TickResult::Halted))
        }
        GenerationWrite::Applied(None) => Err(AppError::Storage(
            "cleanup failure transaction omitted its incident".into(),
        )),
        GenerationWrite::Superseded => Ok(GenerationWrite::Superseded),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn project_fault_wire_preserves_evidence_but_never_bypasses_cleanup() {
        let wire = serde_json::json!({"result":"project-fault", "fault": {
            "code":"missing-certification", "locus":".storyhook.toml#verify.gate",
            "tree":"a".repeat(40), "base":"b".repeat(40), "head":"c".repeat(40),
            "head_tree":"d".repeat(40),
            "gate":"make test", "log":"/logs/attempt", "execution":"/executions/attempt.json",
            "execution_status":0, "receipt":"missing", "detail":"missing receipt evidence"}});
        let parsed: WireOutcome = serde_json::from_value(wire.clone()).unwrap();
        assert!(matches!(
            parsed.into_outcome(),
            VerificationOutcome::ProjectFault { .. }
        ));
        let interrupted = interrupted_outcome(
            wire.to_string().as_bytes(),
            "capture timed out",
            Path::new("/source"),
        )
        .expect("retain the complete fault after capture failure");
        assert!(
            matches!(interrupted, VerificationOutcome::InfrastructureFailure { detail, .. }
            if detail.contains("capture timed out") && detail.contains("missing receipt evidence"))
        );
        let mut unsafe_wire = wire.clone();
        unsafe_wire["cleanup_failure"] = serde_json::json!({
            "phase":"owner cleanup", "detail":"survivors", "owner":"owner", "worktree":"worktree",
            "disposition":"permanent"});
        let parsed: WireOutcome = serde_json::from_value(unsafe_wire).unwrap();
        assert!(
            matches!(parsed.into_outcome(), VerificationOutcome::InfrastructureFailure { detail, .. }
            if detail.contains("survivors") && detail.contains("missing receipt evidence"))
        );
        let mut invalid = wire;
        invalid["fault"]["execution_status"] = 1.into();
        let parsed: WireOutcome = serde_json::from_value(invalid).unwrap();
        assert!(matches!(
            parsed.into_outcome(),
            VerificationOutcome::InfrastructureFailure { .. }
        ));
    }

    #[test]
    fn certify_boundary_refuses_unguarded_merged_results_even_after_capture_failure() {
        let wire = serde_json::json!({"result":"merged","tree":"tree","detail":"claimed merge"});
        assert!(serde_json::from_value::<WireOutcome>(wire.clone()).is_err());
        assert!(
            interrupted_outcome(
                wire.to_string().as_bytes(),
                "cancelled",
                Path::new("/source")
            )
            .is_none()
        );
    }

    #[test]
    fn interrupted_capture_rejects_partial_or_malformed_completion() {
        let original = serde_json::json!({"result":"tests-failed", "gate":"make test", "tree":"tree", "log":"log", "detail":"failure"});
        for key in ["tree", "log", "detail", "gate"] {
            for absent in [false, true] {
                let mut partial = original.clone();
                if absent {
                    partial.as_object_mut().unwrap().remove(key);
                } else {
                    partial[key] = "".into();
                }
                assert!(
                    interrupted_outcome(
                        partial.to_string().as_bytes(),
                        "cancelled",
                        Path::new("/source")
                    )
                    .is_none()
                );
            }
        }
        for bytes in [b"{".as_slice(), b"{} {}", b"null"] {
            assert!(interrupted_outcome(bytes, "cancelled", Path::new("/source")).is_none());
        }
        let mut malformed = original;
        malformed["cleanup_failure"] =
            serde_json::json!({"phase":"owner", "detail":"failed", "disposition":"permanent"});
        assert!(
            interrupted_outcome(
                malformed.to_string().as_bytes(),
                "cancelled",
                Path::new("/source")
            )
            .is_none()
        );
    }

    #[test]
    fn wire_preserves_each_completed_result_and_cleanup_diagnosis() {
        for result in ["tests-failed", "gate-passed", "certified"] {
            let wire = serde_json::json!({
                "result": result, "gate":"make test", "head": "head", "tree": "tree", "log": "/tmp/log", "detail": "named result",
                "cleanup_failure": { "phase": "restoration", "detail": "live writers",
                    "owner": "/tmp/owner", "worktree": "/tmp/verifier", "disposition": "permanent" }
            });
            let outcome = serde_json::from_value::<WireOutcome>(wire)
                .unwrap()
                .into_outcome();
            let VerificationOutcome::CleanupFailed { verdict, cleanup } = outcome else {
                panic!("lost cleanup or completed result: {outcome:?}");
            };
            assert_eq!(cleanup.detail, "live writers");
            assert!(matches!(
                (result, verdict),
                ("tests-failed", CompletedVerification::TestsFailed { .. })
                    | ("gate-passed", CompletedVerification::GatePassed { .. })
                    | ("certified", CompletedVerification::Certified { .. })
            ));
        }
    }

    #[test]
    fn wire_keeps_clean_results_compatible_and_refuses_incomplete_cleanup() {
        let red = serde_json::json!({"result":"tests-failed", "gate":"make test", "tree":"tree", "log":"log", "detail":"red"});
        assert!(matches!(
            serde_json::from_value::<WireOutcome>(red.clone())
                .unwrap()
                .into_outcome(),
            VerificationOutcome::TestsFailed { .. }
        ));
        let mut missing = red.clone();
        missing["cleanup_failure"] = serde_json::json!({"detail":"refused"});
        assert!(serde_json::from_value::<WireOutcome>(missing).is_err());
        let mut passed = red;
        passed["result"] = "gate-passed".into();
        assert!(serde_json::from_value::<WireOutcome>(passed).is_err());
    }
}
