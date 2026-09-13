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
        command
            .envs(self.env.child_vars())
            .arg(script)
            .arg(operation);
        let result = crate::process::run_captured_with_input(
            command,
            file,
            Duration::from_secs(if operation == "resume" { 125 } else { 45 }),
        )
        .map_err(|e| AppError::Storage(format!("continuation {operation}: {}", e.detail())))?;
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
