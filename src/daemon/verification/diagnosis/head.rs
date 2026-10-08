//! Metadata refresh cannot substitute for the native causal capability.
use super::*;

impl ShellVerificationActuator {
    pub(in crate::daemon::verification) fn diagnosis_head(
        &self,
        candidate: &VerificationCandidate,
    ) -> Result<String, AppError> {
        let link = candidate
            .pull_request
            .as_ref()
            .map_err(|problem| AppError::Validation(problem.message()))?;
        if let Some(problem) = checkout_repository_problem(&self.env, &candidate.checkout, link) {
            return Err(AppError::Validation(problem));
        }
        let mut command = Command::new("bash");
        apply_verification_allowlist(&mut command);
        command
            .arg(self.verifier_script()?)
            .arg("--diagnosis-head")
            .arg(&link.url)
            .current_dir(&candidate.checkout)
            .envs(self.env.child_vars())
            .env("STORY_BIN", self.story_binary())
            .env("STORYHOOK_GITHUB_AUTHORITY", &candidate.checkout)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GH_PROMPT_DISABLED", "1")
            .stdin(Stdio::null());
        let output = self.run_control_command(
            command,
            "verifier",
            &verification_request_id(candidate),
            candidate.project,
            "causal return head refresh",
        )?;
        if !output.status.success() {
            return Err(AppError::Storage(format!(
                "causal return head refresh: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        current_head(&output.stdout)
    }
}

fn current_head(bytes: &[u8]) -> Result<String, AppError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Head {
        state: String,
        is_draft: bool,
        is_cross_repository: bool,
        head_ref_oid: String,
    }
    let head: Head = serde_json::from_slice(bytes)
        .map_err(|e| AppError::Validation(format!("causal return head metadata: {e}")))?;
    if head.state != "OPEN" || head.is_draft || head.is_cross_repository {
        return Err(AppError::Validation(
            "causal return requires the same open, ready, same-repository PR".into(),
        ));
    }
    crate::service::trial_merge::require_pinned(&head.head_ref_oid, "causal return head")?;
    Ok(head.head_ref_oid)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_must_name_one_open_ready_pinned_source() {
        let valid = serde_json::json!({"state":"OPEN", "isDraft":false, "isCrossRepository":false, "headRefOid":"a".repeat(40)});
        assert_eq!(
            current_head(&serde_json::to_vec(&valid).unwrap()).unwrap(),
            "a".repeat(40)
        );
        for (key, value) in [
            ("state", serde_json::json!("MERGED")),
            ("isDraft", serde_json::json!(true)),
            ("isCrossRepository", serde_json::json!(true)),
            ("headRefOid", serde_json::json!("main")),
        ] {
            let mut changed = valid.clone();
            changed[key] = value;
            assert!(current_head(&serde_json::to_vec(&changed).unwrap()).is_err());
        }
        assert!(current_head(b"{}").is_err());
    }
}
