//! Local measurement launch preparation, separate from daemon invocation.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::Command;

use crate::error::AppError;

/// Reserve one output directory and prepare the owned verifier operation.
/// No store is opened, no daemon is replaced, and no gate runs in this function.
pub fn command(checkout: &Path, commit: &str, output: &Path) -> Result<Command, AppError> {
    if !cfg!(target_os = "macos") {
        return Err(AppError::Validation(
            "gate-class measurement collection currently requires macOS".into(),
        ));
    }
    let prepare = || -> Result<Command, AppError> {
        let checkout = fs::canonicalize(checkout)?;
        if commit.is_empty() || commit.starts_with('-') {
            return Err(AppError::Validation("measurement requires a commit".into()));
        }
        let output = std::path::absolute(output)?;
        let parent = fs::canonicalize(output.parent().ok_or_else(|| {
            AppError::Validation("measurement output requires a parent directory".into())
        })?)?;
        if parent.starts_with(&checkout) {
            return Err(AppError::Validation(
                "measurement output must be outside the source checkout".into(),
            ));
        }
        if output.exists() {
            let metadata = fs::symlink_metadata(&output)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(AppError::Validation(
                    "measurement output must be a physical directory".into(),
                ));
            }
        } else {
            fs::create_dir(&output)?;
        }
        let output = fs::canonicalize(output)?;
        let request = serde_json::to_vec(&serde_json::json!({"version":1,
            "checkout":checkout, "commit":commit}))?;
        let marker = output.join("request.json");
        if marker.exists() {
            if fs::symlink_metadata(&marker)?.file_type().is_symlink()
                || fs::read(&marker)? != request
            {
                return Err(AppError::Validation(
                    "measurement output belongs to a different request".into(),
                ));
            }
        } else {
            if fs::read_dir(&output)?.next().is_some() {
                return Err(AppError::Validation(
                    "measurement output is not empty or owned".into(),
                ));
            }
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(marker)?;
            file.write_all(&request)?;
            file.sync_all()?;
        }
        let bundle = crate::daemon::verifier_bundle::materialize_measurement(&output)?;
        let mut command = Command::new("/bin/bash");
        command
            .arg(bundle.join("measure-gate-class.sh"))
            .arg(&checkout)
            .arg(commit)
            .arg(&output)
            .arg(std::env::current_exe()?)
            .current_dir(&checkout)
            .env_clear();
        // Keep tool discovery and the user's compiler/cache configuration, but
        // no dispatch identity, live store, credentials or inherited gate grant.
        for name in [
            "HOME",
            "XDG_STATE_HOME",
            "PATH",
            "USER",
            "LOGNAME",
            "TMPDIR",
            "TZ",
            "LANG",
            "LC_ALL",
            "LC_CTYPE",
            "LC_COLLATE",
            "LC_MESSAGES",
            "LC_MONETARY",
            "LC_NUMERIC",
            "LC_TIME",
            "CARGO_HOME",
            "RUSTUP_HOME",
            "DEVELOPER_DIR",
            "SDKROOT",
            "TOOLCHAINS",
            "STORYHOOK_PYTHON",
        ] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command.env("PYTHONDONTWRITEBYTECODE", "1");
        Ok(command)
    };
    prepare().map_err(|error| error.with_context("preparing verifier measurement"))
}
