//! Recovery subprocesses carry explicit story workspace ownership.

use super::*;
use crate::service::workspace_lock::WorkspaceLock;

/// Ownership belongs to this control operation, not a project verifier lookup.
#[derive(Clone, Copy)]
pub(crate) struct ControlOwner<'a> {
    /// Story workspace descriptor inherited by every guarded child.
    pub workspace: Option<&'a WorkspaceLock>,
    /// Cancellation for this operation only.
    pub cancellation: &'a Cancellation,
}

impl ShellVerificationActuator {
    pub(super) fn run_control_owned(
        &self,
        mut command: Command,
        role: &str,
        request_id: &str,
        operation: &str,
        owner: ControlOwner<'_>,
    ) -> Result<Captured, AppError> {
        command.env_remove("STORY_WORKSPACE_LOCK_FD");
        if let Some(workspace) = owner.workspace {
            workspace.dispatch_command(&mut command);
        }
        run_captured_cancellable(
            command,
            self.control_timeout,
            TerminationPolicy::TerminateThenKill {
                grace: self.termination_grace,
            },
            owner.cancellation,
            |pid| {
                self.owned_processes
                    .register(role, pid, Some(request_id))
                    .map_err(|error| error.to_string())
            },
        )
        .map_err(|error| match error {
            CaptureError::Timeout(_) => AppError::Storage(format!(
                "{operation} did not finish within {:?}; its process group was terminated",
                self.control_timeout
            )),
            other => AppError::Storage(format!("could not run {operation}: {}", other.detail())),
        })
    }

    /// Notify under explicit ownership without consulting the verifier slot.
    pub(crate) fn notify_owned(
        &self,
        candidate: &VerificationCandidate,
        message: &str,
        workspace: Option<&WorkspaceLock>,
        cancellation: &Cancellation,
    ) -> Result<NotifyDelivery, AppError> {
        match self.helper(
            candidate,
            "notify",
            Some(message),
            ControlOwner {
                workspace,
                cancellation,
            },
        )? {
            HelperAnswer::Ok => Ok(NotifyDelivery::Delivered),
            HelperAnswer::Refused { reason, display } => match agent_presence(reason.as_deref()) {
                AgentPresence::Absent => Ok(NotifyDelivery::AgentAbsent {
                    reason: reason.unwrap_or_default(),
                    detail: display,
                }),
                AgentPresence::NotAbsent => Err(AppError::Storage(display)),
            },
        }
    }

    /// Dispatch with the shared managed argv and explicit workspace/cancellation.
    pub(crate) fn dispatch_owned(
        &self,
        candidate: &VerificationCandidate,
        plan: &ResumePlan,
        resume: bool,
        owner: ControlOwner<'_>,
    ) -> Result<(), AppError> {
        let outcome = self.dispatch_outcome_owned(candidate, plan, resume, owner)?;
        match outcome.state {
            DispatchOutcomeState::Ok => Ok(()),
            DispatchOutcomeState::Refused => Err(AppError::Storage(
                outcome
                    .payload
                    .get("display")
                    .or_else(|| outcome.payload.get("reason"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(if resume {
                        "story helper `dispatch --resume` refused without diagnostics"
                    } else {
                        "story helper `dispatch` refused without diagnostics"
                    })
                    .to_string(),
            )),
        }
    }
    /// Preserve typed refusal evidence for the recovery delivery budget.
    pub(crate) fn dispatch_outcome_owned(
        &self,
        candidate: &VerificationCandidate,
        plan: &ResumePlan,
        resume: bool,
        owner: ControlOwner<'_>,
    ) -> Result<crate::service::engine::DispatchOutcome, AppError> {
        let script = self.helper_path()?;
        let options = DispatchOptions {
            model: plan.model.clone(),
            effort: plan.effort.clone(),
            fast: plan.fast,
            resume,
        };
        run_shell_dispatch_cancellable(
            &script,
            &candidate.project_slug,
            &candidate.story_id,
            plan.agent,
            true,
            plan.full_auto,
            &options,
            &self.env,
            Some(owner.cancellation),
            owner.workspace,
        )
    }
}
