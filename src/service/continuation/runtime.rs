//! Installed helper discovery and bounded JSON subprocess transport.
use super::ContinuationRuntime;
use crate::api::dispatch::{DispatchAgent, resolve_dispatch_script};
use crate::env::{Environment, spawn_env::apply_dispatch_allowlist};
use crate::error::AppError;
use serde_json::Value;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// The helper's 30 s operation budget leaves one third for interpreter startup and exit.
const OBSERVATION_TIMEOUT: Duration = Duration::from_secs(45);
/// Resume pools dispatch and observations into 150 s, with the same one-third margin.
const RESUME_TIMEOUT: Duration = Duration::from_secs(225);

fn operation_timeout(operation: &str) -> Duration {
    match operation {
        "resume" => RESUME_TIMEOUT,
        _ => OBSERVATION_TIMEOUT,
    }
}

/// Production provider adapter. An explicit script path supports isolated process fixtures.
pub struct PythonRuntime {
    script: Option<PathBuf>,
    env: Environment,
}
impl PythonRuntime {
    /// Resolve the installed helper on each call; never assume checkout and install match.
    pub fn installed(env: Environment) -> Self {
        Self { script: None, env }
    }
    /// Use a private test helper with the production transport and process lifetime rules.
    pub fn at(script: &Path, env: Environment) -> Self {
        Self {
            script: Some(script.into()),
            env,
        }
    }
    fn script(&self, provider: &str) -> Result<PathBuf, AppError> {
        if let Some(script) = &self.script {
            return Ok(script.clone());
        }
        let agent = match provider {
            "codex" => DispatchAgent::Codex,
            "claude" => DispatchAgent::Claude,
            _ => return Err(AppError::Validation("unknown continuation provider".into())),
        };
        let dispatch = resolve_dispatch_script(agent).map_err(AppError::Storage)?;
        let root = dispatch
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| AppError::Storage("installed dispatch has no plugin root".into()))?;
        let helper = root.join("lib/continuation_runtime.py");
        if !helper.is_file() {
            return Err(AppError::Storage(format!(
                "installed plugin lacks continuation runtime capability: {}",
                helper.display()
            )));
        }
        Ok(helper)
    }
}
impl ContinuationRuntime for PythonRuntime {
    fn call(&self, operation: &str, input: &Value) -> Result<Value, AppError> {
        let provider = input["provider"]
            .as_str()
            .or_else(|| input["capture"]["provider"].as_str())
            .ok_or_else(|| AppError::Validation("continuation call has no provider".into()))?;
        let script = self.script(provider)?;
        let mut file = tempfile::tempfile()?;
        file.write_all(&serde_json::to_vec(input)?)?;
        file.seek(SeekFrom::Start(0))?;
        let mut command = Command::new("python3");
        apply_dispatch_allowlist(&mut command);
        // The helper resumes through `story.sh`, which runs `${STORY_BIN:-story}`.
        // The allowlist admits every `STORY_*` name, so without this pin an
        // inherited value, not this process, would choose that binary.
        command
            .envs(self.env.child_vars())
            .env(
                "STORY_BIN",
                std::env::current_exe().map_err(|e| AppError::Storage(e.to_string()))?,
            )
            .arg(script)
            .arg(operation);
        let result =
            crate::process::run_captured_with_input(command, file, operation_timeout(operation))
                .map_err(|e| {
                    AppError::Storage(format!("continuation {operation}: {}", e.detail()))
                })?;
        if !result.status.success() {
            return Err(AppError::Storage(format!(
                "continuation {operation} failed: {}",
                String::from_utf8_lossy(&result.stderr)
            )));
        }
        let answer: Value = serde_json::from_slice(&result.stdout).map_err(|e| {
            AppError::Validation(format!("invalid continuation {operation} response: {e}"))
        })?;
        if answer["ok"] != true {
            return Err(AppError::Validation(format!(
                "continuation {operation} refused: {}",
                answer["detail"]
            )));
        }
        Ok(answer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_probe_budgets_fit_continuation_outer_limits() {
        let ordinary = crate::process::plugin_probe_budget();
        let source = include_str!("../../../plugins/story/lib/continuation_runtime.py");
        let seconds = source
            .lines()
            .find_map(|line| line.strip_prefix("RESUME_BUDGET_SECONDS = "))
            .expect("continuation runtime must declare its resume operation budget");
        let resume = Duration::from_secs(seconds.trim().parse().expect("whole seconds"));
        assert!(resume > ordinary, "resume must also allow guarded dispatch");
        for operation in [
            "capture",
            "observe",
            "register",
            "resume-preflight",
            "resume",
        ] {
            let budget = if operation == "resume" {
                resume
            } else {
                ordinary
            };
            let bound = operation_timeout(operation);
            assert!(
                budget * 3 <= bound * 2,
                "{operation}: helper budget {budget:?} exceeds two thirds of {bound:?}"
            );
        }
    }

    #[test]
    fn only_resume_receives_the_dispatch_outer_limit() {
        assert_eq!(operation_timeout("resume"), Duration::from_secs(225));
        for operation in [
            "capture",
            "observe",
            "register",
            "resume-preflight",
            "unknown",
        ] {
            assert_eq!(operation_timeout(operation), Duration::from_secs(45));
        }
    }
}
