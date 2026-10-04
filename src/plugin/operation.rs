//! Durable evidence for synchronous provider mutations (SH-821).
//!
//! The scope lives on the request thread, like activity context. The provider
//! runner records every call within it, including rollback, without another
//! provider transport. No environment variable can arm this scope. A refused
//! plugin guard never enters it. An inherited home lock outlives a killed
//! parent while a provider child still owns effects.

use std::cell::RefCell;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::process::Command;

use fs4::FileExt;
use serde::{Deserialize, Serialize};

use super::provider_cli::ProviderError;
use super::receipt::Actor;
use super::registration::Previous;
use super::{PluginTarget, home_dir};
use crate::error::AppError;
use crate::process::Captured;

#[cfg(test)]
mod tests;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Incomplete,
    Succeeded,
    Failed,
}

#[derive(Serialize, Deserialize)]
struct Step {
    action: String,
    started_at: String,
    completed_at: Option<String>,
    result: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Record {
    version: u32,
    id: String,
    target: String,
    verb: String,
    started_at: String,
    completed_at: Option<String>,
    home: PathBuf,
    pid: u32,
    actor: Actor,
    previous: Previous,
    intended_source: Option<String>,
    steps: Vec<Step>,
    outcome: Outcome,
    error: Option<String>,
}

struct Active {
    lock: File,
    path: PathBuf,
    record: Record,
}

thread_local! {
    static ACTIVE: RefCell<Option<Active>> = const { RefCell::new(None) };
}

struct Scope;

impl Drop for Scope {
    fn drop(&mut self) {
        // Do not unlock explicitly: a surviving provider may still hold an
        // inherited descriptor for this same open file description.
        ACTIVE.with(|active| {
            active.borrow_mut().take();
        });
    }
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
}

fn evidence_error(path: &Path, error: impl std::fmt::Display) -> AppError {
    AppError::Storage(format!(
        "plugin operation evidence at `{}`: {error}",
        path.display()
    ))
}

fn path(target: PluginTarget) -> Result<PathBuf, AppError> {
    Ok(super::data_dir()?
        .join("provider-installs")
        .join(format!("{}-operations", target.install_token()))
        .join("current.json"))
}

fn publish(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    let parent = path.parent().expect("operation files have a parent");
    let write = || -> std::io::Result<()> {
        fs::create_dir_all(parent)?;
        let mut staged = tempfile::NamedTempFile::new_in(parent)?;
        staged.write_all(bytes)?;
        staged.as_file().sync_all()?;
        staged.persist(path).map_err(|error| error.error)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    };
    write().map_err(|error| evidence_error(path, error))
}

impl Active {
    fn persist(&self) -> Result<(), AppError> {
        let bytes = serde_json::to_vec_pretty(&self.record)
            .map_err(|error| evidence_error(&self.path, error))?;
        publish(&self.path, &bytes)
    }

    fn begin(target: PluginTarget, verb: &str, intended: Option<&str>) -> Result<Self, AppError> {
        let home = home_dir()?.canonicalize()?;
        let directory = home.join(".local/state/storyhook/provider-locks");
        fs::create_dir_all(&directory)?;
        let lock_path = directory.join(format!("{}.lock", target.install_token()));
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|error| evidence_error(&lock_path, error))?;
        lock.try_lock_exclusive().map_err(|error| {
            if error.kind() == ErrorKind::WouldBlock {
                AppError::Storage(format!(
                    "another plugin operation owns {} for this HOME; retry when it finishes",
                    target.install_token()
                ))
            } else {
                evidence_error(&lock_path, error)
            }
        })?;
        let path = path(target)?;
        match fs::read(&path) {
            Ok(bytes) => {
                // Archive even malformed evidence verbatim; its absence from
                // the parser must never become permission to discard it.
                let archive = path
                    .parent()
                    .unwrap()
                    .join("history")
                    .join(format!("{}.json", uuid::Uuid::new_v4()));
                publish(&archive, &bytes)?;
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(evidence_error(&path, error)),
        }
        let active = Self {
            lock,
            path,
            record: Record {
                version: 1,
                id: uuid::Uuid::new_v4().to_string(),
                target: target.install_token().into(),
                verb: verb.into(),
                started_at: now(),
                completed_at: None,
                home,
                pid: std::process::id(),
                actor: Actor::observe(),
                previous: super::registration::snapshot(target),
                intended_source: intended.map(str::to_string),
                steps: Vec::new(),
                outcome: Outcome::Incomplete,
                error: None,
            },
        };
        active.persist()?;
        Ok(active)
    }
}

/// Runs one complete mutation with evidence and provider-home exclusion.
pub(super) fn run<T>(
    target: PluginTarget,
    verb: &str,
    intended: Option<&str>,
    work: impl FnOnce() -> Result<T, AppError>,
) -> Result<T, AppError> {
    if ACTIVE.with(|active| active.borrow().is_some()) {
        return Err(AppError::Storage("nested plugin operation refused".into()));
    }
    let active = Active::begin(target, verb, intended)?;
    ACTIVE.with(|slot| *slot.borrow_mut() = Some(active));
    let _scope = Scope;
    let result = work();
    let saved = ACTIVE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let active = slot.as_mut().expect("operation scope is active");
        active.record.outcome = if result.is_ok() {
            Outcome::Succeeded
        } else {
            Outcome::Failed
        };
        active.record.error = result.as_ref().err().map(ToString::to_string);
        active.record.completed_at = Some(now());
        active.persist()
    });
    combine(result, saved)
}

fn combine<T>(result: Result<T, AppError>, saved: Result<(), AppError>) -> Result<T, AppError> {
    match (result, saved) {
        (result, Ok(())) => result,
        (Ok(_), Err(error)) => Err(error),
        (Err(original), Err(error)) => Err(AppError::Storage(format!(
            "{original}\nAND failed to record operation evidence: {error}"
        ))),
    }
}

fn start(action: &str) -> Result<Option<usize>, AppError> {
    ACTIVE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(active) = slot.as_mut() else {
            return Ok(None);
        };
        let index = active.record.steps.len();
        active.record.steps.push(Step {
            action: action.into(),
            started_at: now(),
            completed_at: None,
            result: None,
        });
        active.persist()?;
        Ok(Some(index))
    })
}

fn end(index: Option<usize>, result: String) -> Result<(), AppError> {
    ACTIVE.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let (Some(active), Some(index)) = (slot.as_mut(), index) {
            active.record.steps[index].result = Some(result);
            active.record.steps[index].completed_at = Some(now());
            active.persist()?;
        }
        Ok(())
    })
}

/// Records a filesystem phase, including its failure, before the next effect.
pub(super) fn step<T>(
    action: &str,
    work: impl FnOnce() -> Result<T, AppError>,
) -> Result<T, AppError> {
    let index = start(action)?;
    let result = work();
    let detail = result
        .as_ref()
        .map(|_| "succeeded".into())
        .unwrap_or_else(ToString::to_string);
    combine(result, end(index, detail))
}

/// Adds provider execution evidence without changing its result classification.
pub(super) fn provider(
    target: PluginTarget,
    args: &[&str],
    work: impl FnOnce() -> Result<Captured, ProviderError>,
) -> Result<Captured, ProviderError> {
    let action = format!("{} {}", target.install_token(), args.join(" "));
    let index = start(&action).map_err(ProviderError::Failed)?;
    let result = work();
    let detail = match &result {
        Ok(out) => out.status.to_string(),
        Err(error) => error.to_string(),
    };
    if let Err(error) = end(index, detail) {
        return Err(ProviderError::Failed(match result {
            Ok(out) if !out.status.success() => AppError::Storage(format!(
                "{action}: {}\n{}\nAND {error}",
                out.status,
                super::combined_output(&out)
            )),
            Ok(_) => error,
            Err(original) => AppError::Storage(format!("{original}\nAND {error}")),
        }));
    }
    result
}

/// Keeps provider-home exclusion alive in a provider orphaned by a parent exit.
pub(super) fn inherit_lock(command: &mut Command) {
    ACTIVE.with(|slot| {
        if let Some(active) = slot.borrow().as_ref() {
            crate::service::workspace_lock::inherit_descriptor(active.lock.as_fd(), command);
        }
    });
}

/// Returns only findings; completed operations do not explain a later loss.
pub(crate) fn finding(target: PluginTarget) -> Result<Option<String>, AppError> {
    let path = path(target)?;
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(evidence_error(&path, error)),
    };
    let record: Record =
        serde_json::from_slice(&bytes).map_err(|error| evidence_error(&path, error))?;
    let completed = record.completed_at.is_some();
    let valid_outcome = match record.outcome {
        Outcome::Succeeded => {
            completed
                && record.error.is_none()
                && record
                    .steps
                    .iter()
                    .all(|step| step.completed_at.is_some() && step.result.is_some())
        }
        Outcome::Failed => completed && record.error.is_some(),
        Outcome::Incomplete => !completed && record.error.is_none(),
    };
    if record.version != 1
        || record.target != target.install_token()
        || record.home != home_dir()?.canonicalize()?
        || uuid::Uuid::parse_str(&record.id).is_err()
        || !matches!(record.verb.as_str(), "install" | "uninstall" | "reinstall")
        || !valid_outcome
    {
        return Err(evidence_error(
            &path,
            "unsupported version, mismatched provider/HOME, or inconsistent operation state",
        ));
    }
    let state = match record.outcome {
        Outcome::Succeeded => return Ok(None),
        Outcome::Incomplete => "INCOMPLETE PLUGIN OPERATION",
        Outcome::Failed => "FAILED PLUGIN OPERATION",
    };
    let phase = record
        .steps
        .last()
        .map(|step| {
            format!(
                "{} ({})",
                step.action,
                step.result.as_deref().unwrap_or("completion not recorded")
            )
        })
        .unwrap_or_else(|| "no effect started".into());
    let retry = if record.verb == "reinstall" {
        "reinstall".into()
    } else {
        format!("{} {}", record.verb, record.target)
    };
    Ok(Some(format!(
        "{state}: {} {} started at {} by `{}` (PID {}, build {}, override {}); last phase: {phase}; {}evidence: {} — run `story plugin {retry}` to retry",
        record.verb,
        record.target,
        record.started_at,
        record.actor.exe_display(),
        record.pid,
        record
            .actor
            .build
            .map_or("unknown", super::guard::Build::token),
        record.actor.override_set,
        record
            .error
            .map(|error| format!("{error}; "))
            .unwrap_or_default(),
        path.display()
    )))
}
