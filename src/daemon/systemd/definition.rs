//! Round-trippable unit definitions. Metadata is never trusted independently
//! of the executable configuration it reconstructs.

use super::super::agent::ExecutionPath;
use crate::{env::Environment, error::AppError};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const HEADER: &str = "# StoryhookRegistrationV1=";

/// Fields sufficient to reconstruct the complete generated unit.
#[derive(Serialize, Deserialize)]
pub(super) struct Registration {
    /// Absolute installed executable.
    pub(super) exe: PathBuf,
    /// Canonical store served by this unit.
    pub(super) store: PathBuf,
    /// Resolved XDG state root.
    pub(super) state: PathBuf,
    /// Resolved XDG configuration root.
    pub(super) config: PathBuf,
    /// Per-store daemon log.
    pub(super) log: PathBuf,
    /// Validated executable search path.
    pub(super) path: String,
}

impl Registration {
    /// Captures resolved paths without inheriting arbitrary shell variables.
    pub(super) fn new(env: &Environment, exe: &Path, path: &ExecutionPath) -> Self {
        Self {
            exe: exe.to_owned(),
            store: env.store_path().to_owned(),
            state: env
                .state_home()
                .parent()
                .expect("state has parent")
                .to_owned(),
            config: env.config_home().to_owned(),
            log: env.daemon_log(),
            path: path.as_str().to_owned(),
        }
    }

    /// Renders a systemd 245-compatible service definition.
    pub(super) fn render(&self) -> Result<String, AppError> {
        let exe = quote_path(&self.exe, true)?;
        let store = quote_path(&self.store, true)?;
        let path = quote(&format!("PATH={}", self.path), false)?;
        let state = quote(&format!("XDG_STATE_HOME={}", absolute(&self.state)?), false)?;
        let config = quote(
            &format!("XDG_CONFIG_HOME={}", absolute(&self.config)?),
            false,
        )?;
        let raw_log = absolute(&self.log)?;
        if raw_log.chars().any(char::is_control) {
            return Err(AppError::Usage(
                "systemd log path contains a control character".into(),
            ));
        }
        let log = format!("append:{}", raw_log.replace('%', "%%"));
        let metadata = serde_json::to_string(self).map_err(|e| AppError::Storage(e.to_string()))?;
        Ok(format!(
            "{HEADER}{metadata}\n[Unit]\nDescription=Storyhook per-store daemon\n\n[Service]\nType=simple\nExecStart={exe} --store-path {store} daemon --serve --owner systemd\nExecStop={exe} --store-path {store} daemon stop\nEnvironment={path}\nEnvironment={state}\nEnvironment={config}\nNice=0\nCPUSchedulingPolicy=other\nIOSchedulingClass=best-effort\nIOSchedulingPriority=4\nCPUWeight=100\nIOWeight=100\nRestart=no\nTimeoutStopSec=infinity\nKillMode=mixed\nUMask=0077\nStandardOutput=null\nStandardError={log}\n\n[Install]\nWantedBy=default.target\n"
        ))
    }
}

fn absolute(path: &Path) -> Result<&str, AppError> {
    path.to_str().filter(|_| path.is_absolute()).ok_or_else(|| {
        AppError::Usage(format!(
            "systemd requires an absolute UTF-8 path: {}",
            path.display()
        ))
    })
}

fn quote_path(path: &Path, exec: bool) -> Result<String, AppError> {
    quote(absolute(path)?, exec)
}

/// systemd token quoting, not shell quoting. Environment has no dollar expansion.
fn quote(text: &str, exec: bool) -> Result<String, AppError> {
    if text.chars().any(|ch| ch.is_control()) {
        return Err(AppError::Usage(
            "systemd values cannot contain control characters".into(),
        ));
    }
    let escaped = text
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    Ok(format!(
        "\"{}\"",
        if exec {
            escaped.replace('$', "$$")
        } else {
            escaped
        }
    ))
}

/// Reads only definitions whose metadata reproduces their complete contents.
pub(super) fn parse(text: &str) -> Result<Registration, AppError> {
    let raw = text
        .lines()
        .next()
        .and_then(|line| line.strip_prefix(HEADER))
        .ok_or_else(|| {
            AppError::Storage(
                "not a storyhook-generated systemd unit; run `story daemon install`".into(),
            )
        })?;
    let registration: Registration = serde_json::from_str(raw)
        .map_err(|e| AppError::Storage(format!("invalid systemd registration: {e}")))?;
    ExecutionPath::parse(Some(std::ffi::OsStr::new(&registration.path)))
        .map_err(AppError::Storage)?;
    if registration.render()? != text {
        return Err(AppError::Storage(
            "systemd unit differs from its registration; run `story daemon install`".into(),
        ));
    }
    Ok(registration)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expansion_characters_are_literal_and_metadata_cannot_hide_an_edit() {
        let env = Environment::at("/home/a %b $c \"d\\e");
        let path =
            ExecutionPath::parse(Some(std::ffi::OsStr::new("/opt/%tools/$bin:/usr/bin"))).unwrap();
        let r = Registration::new(&env, Path::new("/opt/a $b %c \"d\\e"), &path);
        let text = r.render().unwrap();
        assert!(text.contains("$$b %%c \\\"d\\\\e"));
        assert!(text.contains("PATH=/opt/%%tools/$bin:/usr/bin"));
        assert!(text.contains("StandardError=append:/home/"));
        assert_eq!(parse(&text).unwrap().exe, r.exe);
        assert!(parse(&text.replace("Nice=0", "Nice=10")).is_err());
        assert!(quote("/opt/line\nbreak", true).is_err());
    }
}
