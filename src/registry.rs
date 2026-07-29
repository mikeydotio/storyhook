//! # QUARANTINED — legacy-web only, deleted at W5
//!
//! This module is part of the pre-rearchitecture storage path. Story data lives
//! in a single global store now ([`crate::store`]), and no `story` command
//! reaches this code. It survives for exactly one reason: the web dashboard
//! (`src/web.rs`) still reads `.storyhook/` directories directly, and the wave
//! that promotes the daemon is what moves it onto the store and deletes this.
//!
//! **Do not add callers.** `tests/invoker_seam.rs::the_legacy_path_is_reachable_
//! only_from_the_web_dashboard` fails if any module other than `web.rs` reaches
//! it, so an accidental dependency is a failing test rather than a surprise a
//! wave later.
//!
//! The multi-repo web dashboard's registry of repos (issue #20).
//!
//! Unlike everything else in storyhook — which is strictly repo-local under
//! `<repo>/.storyhook/` — the registry is the one piece of *global* state:
//! it lives at `~/.storyhook/registry.toml` and lists every repo the web
//! dashboard has been told about, so a single long-lived web service can
//! serve all of them (a home dashboard, a repo-switcher dropdown, and a
//! settings screen) instead of one dashboard daemon per repo.
//!
//! [`Registry`] is a plain in-memory read-modify-write value; callers choose
//! how to load/save it. [`with_lock_at`] is the locked entry point used by
//! both the CLI (`story web register|deregister`) and the web server's own
//! `POST /api/repos` / `DELETE /api/repos/<id>` routes, so concurrent
//! writers — a CLI invocation racing a settings-page click — can't corrupt
//! the file. Reads (e.g. resolving `<id>` on every request, or listing repos
//! for the home screen) go through [`Registry::load_from`] directly, with no
//! lock, so a slow reader never blocks a writer or another reader.

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use fs4::FileExt;
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::storage;

/// One repo the web dashboard knows about.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisteredRepo {
    /// Stable, URL-safe identifier used in `/api/repos/<id>/...` routes.
    /// Minted at registration time from the repo directory's basename and
    /// never changes afterwards, even if the repo is later renamed on disk
    /// (deregister and re-register to pick up a new id).
    pub id: String,
    /// Display name shown in the dashboard — defaults to the directory
    /// basename but can be overridden at registration time.
    pub name: String,
    /// Canonicalized (symlink-resolved, absolute) filesystem path to the
    /// repo root, i.e. the directory containing its `.storyhook/`.
    pub path: PathBuf,
}

/// On-disk shape of `registry.toml`: `schema` for forward-compatible format
/// changes, and one `[[repo]]` table per registered repo.
#[derive(Serialize, Deserialize)]
struct RegistryFile {
    schema: u32,
    #[serde(default, rename = "repo")]
    repos: Vec<RegisteredRepo>,
}

/// The current schema version written by this build.
const CURRENT_SCHEMA: u32 = 1;

/// The registry of repos the web dashboard has been told about, together
/// with the path it was loaded from (and will be written back to by
/// [`Registry::save`]).
pub struct Registry {
    path: PathBuf,
    pub schema: u32,
    pub repos: Vec<RegisteredRepo>,
}

impl Registry {
    /// Loads the registry at the default location (`~/.storyhook/registry.toml`).
    pub fn load() -> Result<Registry, AppError> {
        Registry::load_from(&default_registry_path()?)
    }

    /// Loads the registry at an explicit path. A missing file is not an
    /// error — it's an empty, freshly-initialized registry — so callers
    /// don't need a separate "does the registry exist yet" check before the
    /// first `story web register`.
    pub fn load_from(path: &Path) -> Result<Registry, AppError> {
        if !path.exists() {
            return Ok(Registry {
                path: path.to_path_buf(),
                schema: CURRENT_SCHEMA,
                repos: Vec::new(),
            });
        }
        let content = fs::read_to_string(path)?;
        let file: RegistryFile = toml::from_str(&content)?;
        Ok(Registry {
            path: path.to_path_buf(),
            schema: file.schema,
            repos: file.repos,
        })
    }

    /// Writes the registry back to the path it was loaded from, creating
    /// parent directories as needed.
    pub fn save(&self) -> Result<(), AppError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = RegistryFile {
            schema: self.schema,
            repos: self.repos.clone(),
        };
        fs::write(&self.path, toml::to_string_pretty(&file)?)?;
        Ok(())
    }

    /// Registers `path` as a repo, returning the new entry.
    ///
    /// `path` must canonicalize (i.e. exist) and pass
    /// [`storage::ensure_project`] — registration only ever points at a
    /// real, already-initialized storyhook project, never an arbitrary
    /// directory. Registering the same repo (by canonical path) twice is
    /// rejected rather than silently creating a duplicate entry.
    pub fn register(
        &mut self,
        path: &Path,
        name: Option<&str>,
    ) -> Result<RegisteredRepo, AppError> {
        let canonical = path
            .canonicalize()
            .map_err(|e| AppError::Usage(format!("cannot access `{}`: {e}", path.display())))?;
        storage::ensure_project(&canonical)?;

        if let Some(existing) = self.repos.iter().find(|r| r.path == canonical) {
            return Err(AppError::Validation(format!(
                "`{}` is already registered as `{}`",
                canonical.display(),
                existing.id
            )));
        }

        let basename = canonical
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "repo".to_string());
        let id = unique_id(&self.repos, &slugify(&basename));
        let display_name = name.map(str::to_string).unwrap_or_else(|| basename.clone());

        let repo = RegisteredRepo {
            id,
            name: display_name,
            path: canonical,
        };
        self.repos.push(repo.clone());
        Ok(repo)
    }

    /// Removes the repo matching `target`, tried in order as: a registered
    /// `id`, then a filesystem path (canonicalized if it still exists, so a
    /// relative path resolves the same way `register` would have stored it;
    /// falling back to a raw string match against the stored path so a repo
    /// whose directory was already moved or deleted can still be
    /// deregistered by the path the registry itself reports).
    pub fn deregister(&mut self, target: &str) -> Result<RegisteredRepo, AppError> {
        if let Some(pos) = self.repos.iter().position(|r| r.id == target) {
            return Ok(self.repos.remove(pos));
        }

        let canonical_target = Path::new(target).canonicalize().ok();
        if let Some(pos) = self.repos.iter().position(|r| {
            canonical_target.as_deref() == Some(r.path.as_path())
                || r.path.to_string_lossy() == target
        }) {
            return Ok(self.repos.remove(pos));
        }

        Err(AppError::NotFound(format!(
            "no registered repo matches `{target}`"
        )))
    }

    /// Looks up a registered repo by id.
    pub fn find(&self, id: &str) -> Option<&RegisteredRepo> {
        self.repos.iter().find(|r| r.id == id)
    }
}

/// Lowercases `input` and collapses every run of non-alphanumeric characters
/// into a single `-`, trimming leading/trailing dashes. Falls back to
/// `"repo"` if nothing alphanumeric survives (e.g. a basename of `"..."`),
/// so a minted id is never empty.
fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_dash = false;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "repo".to_string()
    } else {
        out
    }
}

/// Returns `base` if it's not already taken among `repos`, else
/// `base-2`, `base-3`, ... — the first unused suffix.
fn unique_id(repos: &[RegisteredRepo], base: &str) -> String {
    if !repos.iter().any(|r| r.id == base) {
        return base.to_string();
    }
    let mut n = 2;
    loop {
        let candidate = format!("{base}-{n}");
        if !repos.iter().any(|r| r.id == candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Resolves `~/.storyhook`, the registry's home directory (also where the
/// global web dashboard daemon keeps its pid/lock/log files).
pub fn registry_dir() -> Result<PathBuf, AppError> {
    let home = std::env::var("HOME")
        .map_err(|_| AppError::Storage("could not determine home directory".to_string()))?;
    Ok(PathBuf::from(home).join(".storyhook"))
}

/// Resolves `~/.storyhook/registry.toml`.
pub fn default_registry_path() -> Result<PathBuf, AppError> {
    Ok(registry_dir()?.join("registry.toml"))
}

/// Runs `action` against the registry at `path` under an exclusive
/// filesystem lock (a sibling `registry.lock` file, mirroring
/// [`storage::with_project_lock`]'s pattern for the per-repo write lock), so
/// two concurrent register/deregister calls — a CLI invocation racing the
/// settings-page UI, or two settings-page clicks — can't clobber each
/// other's changes.
///
/// The registry is loaded fresh under the lock (so `action` always sees the
/// latest state, not a stale in-memory copy) and saved back only if `action`
/// succeeds; a failed mutation leaves the on-disk file untouched.
pub fn with_lock_at<T>(
    path: &Path,
    action: impl FnOnce(&mut Registry) -> Result<T, AppError>,
) -> Result<T, AppError> {
    let lock_dir = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    fs::create_dir_all(&lock_dir)?;
    let lock_file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(lock_dir.join("registry.lock"))?;

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match lock_file.try_lock_exclusive() {
            Ok(()) => break,
            Err(error) => {
                if error.kind() == std::io::ErrorKind::WouldBlock {
                    if Instant::now() >= deadline {
                        return Err(AppError::LockTimeout(
                            "timed out waiting for the registry lock".to_string(),
                        ));
                    }
                    thread::sleep(Duration::from_millis(50));
                    continue;
                }
                return Err(AppError::Storage(format!(
                    "failed to acquire registry lock: {error}"
                )));
            }
        }
    }

    let mut registry = Registry::load_from(path)?;
    let result = action(&mut registry);
    if result.is_ok() {
        registry.save()?;
    }
    if let Err(error) = lock_file.unlock() {
        return Err(AppError::Storage(format!(
            "failed to release registry lock: {error}"
        )));
    }
    result
}

/// [`with_lock_at`] against the default registry path.
pub fn with_lock<T>(
    action: impl FnOnce(&mut Registry) -> Result<T, AppError>,
) -> Result<T, AppError> {
    with_lock_at(&default_registry_path()?, action)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_lowercases_and_collapses_non_alphanumeric() {
        assert_eq!(slugify("My Cool App!!"), "my-cool-app");
        assert_eq!(slugify("already-slug"), "already-slug");
        assert_eq!(slugify("___"), "repo");
        assert_eq!(slugify(""), "repo");
    }

    #[test]
    fn unique_id_appends_numeric_suffix_on_collision() {
        let repos = vec![RegisteredRepo {
            id: "app".to_string(),
            name: "app".to_string(),
            path: PathBuf::from("/a"),
        }];
        assert_eq!(unique_id(&repos, "app"), "app-2");
        assert_eq!(unique_id(&repos, "other"), "other");
    }
}
