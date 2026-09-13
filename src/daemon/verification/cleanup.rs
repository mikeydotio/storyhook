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
    /// The guarded merge already landed before outer cleanup failed.
    Merged {
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
        if row.state != crate::domain::COMPLETION_STATE_SLUG
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
    gate: &GateCommand,
    capture_detail: &str,
    checkout: &std::path::Path,
) -> Option<VerificationOutcome> {
    // This envelope also admits a bare gate-passed from an interrupted wrapper;
    // the ordinary wire parser still requires cleanup metadata for that result.
    #[derive(Deserialize)]
    struct Answer {
        result: String,
        tree: String,
        detail: String,
        log: Option<String>,
        cleanup_failure: Option<VerificationCleanupFailure>,
    }
    let answer: Answer = serde_json::from_slice(stdout).ok()?;
    if answer.tree.trim().is_empty() || answer.detail.trim().is_empty() {
        return None;
    }
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
        "merged" => CompletedVerification::Merged {
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
    let (comment, merged) = match verdict {
        CompletedVerification::TestsFailed {
            tree,
            log,
            detail,
            gate,
        } => (
            format!(
                "CENTRAL VERIFICATION RED — merge tree `{tree}` failed `{gate}`. Full log: `{log}`. Cleanup must finish before remediation dispatch.\n\n{detail}"
            ),
            false,
        ),
        CompletedVerification::GatePassed {
            tree,
            log,
            detail,
            gate,
        } => (
            format!(
                "CENTRAL VERIFICATION GATE PASSED — command `{gate}` exited successfully on merge tree `{tree}`. Full log: `{log}`. This records execution only; no pull request was landed.\n\n{detail}"
            ),
            false,
        ),
        CompletedVerification::Merged { tree, detail, gate } => (
            format!(
                "{VERIFICATION_GREEN_PREFIX} merge tree `{tree}` passed `{gate}` and pull request {pull_request} landed. {detail}"
            ),
            true,
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
    match queue.record_generation_completed(
        ctx,
        candidate,
        merged.then_some(pull_request),
        &comment,
        Some(&diagnosis),
    )? {
        GenerationWrite::Applied(Some(incident)) => {
            fire_verification_halted(ctx, candidate, &incident);
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
    fn interrupted_capture_rejects_partial_or_malformed_completion() {
        let gate = GateCommand::parse("make test").unwrap();
        let original = serde_json::json!({"result":"tests-failed", "tree":"tree", "log":"log", "detail":"failure"});
        for key in ["tree", "log", "detail"] {
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
                        &gate,
                        "cancelled",
                        Path::new("/source")
                    )
                    .is_none()
                );
            }
        }
        for bytes in [b"{".as_slice(), b"{} {}", b"null"] {
            assert!(interrupted_outcome(bytes, &gate, "cancelled", Path::new("/source")).is_none());
        }
        let mut malformed = original;
        malformed["cleanup_failure"] =
            serde_json::json!({"phase":"owner", "detail":"failed", "disposition":"permanent"});
        assert!(
            interrupted_outcome(
                malformed.to_string().as_bytes(),
                &gate,
                "cancelled",
                Path::new("/source")
            )
            .is_none()
        );
    }

    #[test]
    fn wire_preserves_each_completed_result_and_cleanup_diagnosis() {
        for result in ["tests-failed", "gate-passed", "merged"] {
            let wire = serde_json::json!({
                "result": result, "tree": "tree", "log": "/tmp/log", "detail": "named result",
                "cleanup_failure": { "phase": "restoration", "detail": "live writers",
                    "owner": "/tmp/owner", "worktree": "/tmp/verifier", "disposition": "permanent" }
            });
            let outcome = serde_json::from_value::<WireOutcome>(wire)
                .unwrap()
                .into_outcome(
                    &crate::service::gate_command::GateCommand::parse("make test").unwrap(),
                );
            let VerificationOutcome::CleanupFailed { verdict, cleanup } = outcome else {
                panic!("lost cleanup or completed result: {outcome:?}");
            };
            assert_eq!(cleanup.detail, "live writers");
            assert!(matches!(
                (result, verdict),
                ("tests-failed", CompletedVerification::TestsFailed { .. })
                    | ("gate-passed", CompletedVerification::GatePassed { .. })
                    | ("merged", CompletedVerification::Merged { .. })
            ));
        }
    }

    #[test]
    fn wire_keeps_clean_results_compatible_and_refuses_incomplete_cleanup() {
        let gate = crate::service::gate_command::GateCommand::parse("make test").unwrap();
        let red = serde_json::json!({"result":"tests-failed", "tree":"tree", "log":"log", "detail":"red"});
        assert!(matches!(
            serde_json::from_value::<WireOutcome>(red.clone())
                .unwrap()
                .into_outcome(&gate),
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
