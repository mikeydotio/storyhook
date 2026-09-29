//! The production batch operations: the story helper for member submission,
//! `verify-batch.sh` for the batch pull request, and `verify-pr.sh` for the
//! gate (SH-831).

use super::*;

impl ShellVerificationActuator {
    /// The `verify-batch.sh` this actuator spawns: the injected one, or the
    /// bundle projected from this binary (never the checkout's, SH-654).
    fn batch_script(&self) -> Result<std::path::PathBuf, AppError> {
        if let Some(path) = &self.batch_script {
            return Ok(path.clone());
        }
        Ok(crate::daemon::verifier_bundle::materialize(&self.env)?
            .join(crate::daemon::verifier_bundle::BATCH_SCRIPT))
    }

    /// Runs one `verify-batch.sh` verb from the head's lease repository, where
    /// the batch's merge commits live, with the registered checkout as the
    /// GitHub authority, and answers its JSON document.
    fn run_batch_script(
        &self,
        head: &VerificationCandidate,
        verb: &str,
        args: &[&str],
        cancellation: &Cancellation,
    ) -> Result<serde_json::Value, AppError> {
        let _log = self.log_scope(head);
        let repository = head.cleanup_lease.as_ref().map_or_else(
            || head.checkout.clone(),
            |lease| lease.repository_path.clone(),
        );
        let mut command = Command::new("bash");
        apply_verification_allowlist(&mut command);
        command
            .arg(self.batch_script()?)
            .arg(verb)
            .args(args)
            .current_dir(&repository)
            .env("STORY_BIN", self.story_binary())
            .env("STORYHOOK_GITHUB_AUTHORITY", &head.checkout)
            .envs(self.env.child_vars())
            .env("STORYHOOK_GATE_PROGRESS", journal_path(&self.env, head))
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GH_PROMPT_DISABLED", "1")
            .stdin(Stdio::null());
        let workspace = self.activity.workspace_for(head.project);
        let output = self.run_control_owned(
            command,
            "verifier-batch",
            &verification_request_id(head),
            &format!("verify-batch.sh {verb}"),
            ControlOwner {
                workspace: workspace.as_deref(),
                cancellation,
            },
        )?;
        let answer: serde_json::Value = serde_json::from_slice(&output.stdout).map_err(|_| {
            AppError::Storage(format!(
                "verify-batch.sh {verb} returned invalid JSON: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        })?;
        match answer.get("ok").and_then(serde_json::Value::as_bool) {
            Some(true) if output.status.success() => Ok(answer),
            Some(true) => Err(AppError::Storage(format!(
                "verify-batch.sh {verb} claimed success but exited {}",
                output.status
            ))),
            _ => Err(AppError::Storage(format!(
                "verify-batch.sh {verb} refused ({}): {}",
                answer
                    .get("reason")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("no reason"),
                answer
                    .get("display")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("no diagnosis")
            ))),
        }
    }
}

impl BatchActuator for ShellVerificationActuator {
    fn submit_member(
        &self,
        member: &VerificationCandidate,
        owner: MemberOwner<'_>,
        cancellation: &Cancellation,
    ) -> Result<SubmittedPullRequest, SubmissionFailure> {
        self.submit_owned(
            member,
            ControlOwner {
                workspace: Some(owner.0),
                cancellation,
            },
        )
    }

    fn publish(
        &self,
        head: &VerificationCandidate,
        publication: &BatchPublication,
        cancellation: &Cancellation,
    ) -> Result<BatchPullRequest, AppError> {
        #[derive(Deserialize)]
        struct Published {
            url: String,
            number: u64,
            head_oid: String,
        }
        let answer = self.run_batch_script(
            head,
            "publish",
            &[
                &publication.branch,
                &publication.tip,
                &publication.base,
                &publication.title,
                &publication.body,
            ],
            cancellation,
        )?;
        let published: Published = serde_json::from_value(answer).map_err(|error| {
            AppError::Storage(format!(
                "verify-batch.sh publish answered no receipt: {error}"
            ))
        })?;
        if published.head_oid != publication.tip {
            return Err(AppError::Storage(format!(
                "verify-batch.sh publish reported head {}, not the batch tip {}",
                published.head_oid, publication.tip
            )));
        }
        if parse_pr_url(&published.url)?.number != published.number {
            return Err(AppError::Storage(format!(
                "verify-batch.sh publish reported {} as number {}",
                published.url, published.number
            )));
        }
        Ok(BatchPullRequest {
            url: published.url,
            number: published.number,
        })
    }

    fn gate(
        &self,
        head: &VerificationCandidate,
        pull_request: &PrLink,
        cancellation: &Cancellation,
    ) -> VerificationOutcome {
        self.run_verify_pr(head, pull_request, cancellation, RepairAdmission::Withheld)
    }

    fn retire(
        &self,
        head: &VerificationCandidate,
        batch: &VerificationBatch,
        comment: &str,
    ) -> Result<BatchRetirement, AppError> {
        let url = batch
            .pull_request
            .as_ref()
            .map_or("-", |pull_request| pull_request.url.as_str());
        let answer = self.run_batch_script(
            head,
            "retire",
            &[&batch.branch, url, comment],
            // Retirement runs after the batch ended, never under its
            // cancellation; the control timeout still bounds it.
            &Cancellation::default(),
        )?;
        serde_json::from_value(answer).map_err(|error| {
            AppError::Storage(format!(
                "verify-batch.sh retire answered no receipt: {error}"
            ))
        })
    }

    fn base_policy(
        &self,
        head: &VerificationCandidate,
        base: &str,
        cancellation: &Cancellation,
    ) -> Result<bool, AppError> {
        let answer = self.run_batch_script(head, "base-policy", &[base], cancellation)?;
        answer
            .get("signatures_required")
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| {
                AppError::Storage(format!(
                    "verify-batch.sh base-policy answered no signatures_required: {answer}"
                ))
            })
    }

    fn reap_member(
        &self,
        member: &VerificationCandidate,
        owner: MemberOwner<'_>,
        cancellation: &Cancellation,
    ) -> Result<(), AppError> {
        self.reap_owned(
            member,
            ControlOwner {
                workspace: Some(owner.0),
                cancellation,
            },
        )
    }

    fn prune_members(
        &self,
        head: &VerificationCandidate,
        members: &[MemberBranch],
    ) -> Result<Vec<MemberPrune>, AppError> {
        let args: Vec<&str> = members
            .iter()
            .flat_map(|member| {
                [
                    member.pull_request.as_str(),
                    member.branch.as_str(),
                    member.head.as_str(),
                ]
            })
            .collect();
        // After the batch ended: never under its cancellation; the control
        // timeout still bounds it.
        let answer =
            self.run_batch_script(head, "prune-members", &args, &Cancellation::default())?;
        serde_json::from_value(answer.get("members").cloned().unwrap_or_default()).map_err(
            |error| {
                AppError::Storage(format!(
                    "verify-batch.sh prune-members answered no member results: {error}"
                ))
            },
        )
    }
}
