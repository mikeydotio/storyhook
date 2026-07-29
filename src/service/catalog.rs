//! The dashboard's list of projects.
//!
//! `story web register|deregister|list` used to maintain `~/.storyhook/registry.toml`,
//! a second file that named repositories the dashboard should show and that
//! could disagree with the repositories themselves — a registered path whose
//! `.storyhook/` had since been deleted was a case the dashboard had to survive.
//!
//! In the store there is no second file. A project *is* a catalog entry, and its
//! checkouts are rows beside it, so:
//!
//! * **register** records a checkout against the project it belongs to,
//! * **deregister** forgets a checkout (never the project, and never its
//!   stories),
//! * **list** reports the projects that have at least one known checkout.
//!
//! The daemon and the HTTP server are not ported here; they keep reading the
//! legacy registry until the wave that promotes the daemon.

use std::path::{Path, PathBuf};

use crate::error::AppError;
use crate::store::{ProjectId, ProjectRecord, ReadOps, Store, WriteOps};

use super::project::{path_kind, read_pointer};

/// One row of `story web list`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogEntry {
    /// The project this entry describes, for a caller that goes on to read it.
    pub project: ProjectId,
    /// The project's slug — what the legacy registry called an id.
    pub id: String,
    /// The project's display name.
    pub name: String,
    /// The checkout the dashboard should open, if any is known.
    pub path: Option<PathBuf>,
}

/// The project catalog, over a whole store.
///
/// Not [`Ctx`](super::Ctx)-shaped: `web list` spans every project, and `web
/// register` names the one it is about by path.
pub struct CatalogService<'a, S: Store> {
    store: &'a S,
}

impl<'a, S: Store> CatalogService<'a, S> {
    /// A catalog service over `store`.
    pub fn new(store: &'a S) -> Self {
        Self { store }
    }

    /// Records `path` as a checkout of the project it belongs to.
    ///
    /// The project has to exist already — `register` never creates one, exactly
    /// as the legacy path required an initialized `.storyhook/` before it would
    /// add a registry entry. It is resolved by the checkout's pointer file when
    /// it has one and by the path itself otherwise, so a checkout that was
    /// deregistered can be registered again.
    ///
    /// Idempotent, which is a deliberate divergence: the legacy registry
    /// rejected a second registration of the same path, and in the store the
    /// path is recorded by `story init` itself, so the same rule would make the
    /// command permanently unusable.
    pub fn register(&self, path: &Path, name: Option<&str>) -> Result<CatalogEntry, AppError> {
        let canonical = path
            .canonicalize()
            .map_err(|e| AppError::Usage(format!("cannot access `{}`: {e}", path.display())))?;
        let pointer = read_pointer(&canonical)?;

        Ok(self.store.write(|tx| {
            let project = match &pointer {
                Some(pointer) => tx.project_by_uuid(&pointer.uuid)?,
                None => tx.project_by_path(&canonical)?,
            }
            .ok_or_else(|| {
                AppError::NotFound(format!(
                    "`{}` is not a storyhook project; run `story init` there first",
                    canonical.display()
                ))
            })?;
            tx.touch_project_path(project.id, &canonical, path_kind(&canonical))?;
            // `--name` is *recorded*, not merely echoed. The legacy registry
            // held a display name per repo; the catalog is the projects table
            // now, so this is the only place it can go — and a flag that is
            // accepted and dropped is worse than one that does not exist.
            if let Some(name) = name {
                tx.rename_project(project.id, name)?;
            }
            Ok(CatalogEntry {
                project: project.id,
                id: project.slug,
                name: name.map_or(project.name, str::to_string),
                path: Some(canonical),
            })
        })?)
    }

    /// Forgets the checkout `target` names, by project slug or by path.
    ///
    /// A slug forgets every checkout of that project; a path forgets that one.
    /// Neither deletes the project or its stories — the catalog is a list of
    /// places storyhook has been used, not the data itself.
    pub fn deregister(&self, target: &str) -> Result<CatalogEntry, AppError> {
        let canonical = Path::new(target).canonicalize().ok();

        Ok(self.store.write(|tx| {
            if let Some(project) = tx.project_by_slug(target)? {
                let paths: Vec<String> = tx
                    .project_paths(project.id)?
                    .into_iter()
                    .map(|record| record.path)
                    .collect();
                let first = paths.first().map(PathBuf::from);
                for path in &paths {
                    tx.forget_project_path(project.id, Path::new(path))?;
                }
                return Ok(entry(project, first));
            }

            // A path, either as it exists now or as the catalog recorded it: a
            // checkout whose directory has since been deleted must still be
            // removable by the path the catalog itself reports.
            for candidate in [canonical.clone(), Some(PathBuf::from(target))]
                .into_iter()
                .flatten()
            {
                if let Some(project) = tx.project_by_path(&candidate)? {
                    tx.forget_project_path(project.id, &candidate)?;
                    return Ok(entry(project, Some(candidate)));
                }
            }

            Err(AppError::NotFound(format!("no registered repo matches `{target}`")).into())
        })?)
    }

    /// Every project with a known checkout, ordered by slug.
    pub fn list(&self) -> Result<Vec<CatalogEntry>, AppError> {
        Ok(self.store.read(|tx| {
            let mut entries = Vec::new();
            for project in tx.projects()? {
                let path = tx
                    .project_paths(project.id)?
                    .into_iter()
                    .next()
                    .map(|record| PathBuf::from(record.path));
                if path.is_some() {
                    entries.push(entry(project, path));
                }
            }
            Ok(entries)
        })?)
    }
}

/// One catalog row from a project record.
fn entry(project: ProjectRecord, path: Option<PathBuf>) -> CatalogEntry {
    CatalogEntry {
        project: project.id,
        id: project.slug,
        name: project.name,
        path,
    }
}

/// What adopting `~/.storyhook/registry.toml` did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RegistryAdoption {
    /// Checkouts that resolved to a project and are now recorded against it.
    pub adopted: Vec<PathBuf>,
    /// Checkouts the store does not know — repositories still tracked by a
    /// `.storyhook/` directory that nobody has run `story migrate` on.
    pub unmigrated: Vec<PathBuf>,
}

/// Adopts the legacy dashboard registry's checkouts into the store's catalog.
///
/// The registry is the last piece of storyhook's global state that lives
/// outside the store: a list of repositories the dashboard should show. In the
/// store there is no such list — a project *is* a catalog entry — so adopting it
/// means recording each registered path against the project it belongs to.
///
/// Three properties, each of them deliberate:
///
/// * **The file is never written and never deleted.** Nothing reads it any more
///   — the daemon serves the store — but it is the only copy of a list a user
///   built by hand, and a rollback has to find it exactly as it was. A
///   `MIGRATED.txt` marker is dropped beside it saying so, because a directory
///   full of live-looking state that nothing reads is its own kind of trap.
/// * **Idempotent**, so it can run on every invocation without a marker file to
///   forget to write. Recording a checkout is an upsert, and a path already
///   recorded is left alone.
/// * **A repository the store has never heard of is reported, not created.**
///   Minting a project row for an unmigrated `.storyhook/` tree would produce
///   an empty project that looks like a lost one; the honest answer is that it
///   is waiting for `story migrate`.
///
/// `path` is a parameter rather than [`crate::paths::legacy_global_dir`] read
/// here, because an in-process test cannot redirect `HOME` for itself.
pub fn adopt_legacy_registry<S: Store>(
    store: &S,
    path: &Path,
) -> Result<RegistryAdoption, AppError> {
    /// The two fields of a `[[repo]]` table this needs. Read with its own
    /// minimal shape rather than through `crate::registry`, which the daemon
    /// wave deletes — adoption has to outlive the thing it adopts.
    #[derive(serde::Deserialize)]
    struct RegistryFile {
        #[serde(default, rename = "repo")]
        repos: Vec<RegisteredRepo>,
    }
    #[derive(serde::Deserialize)]
    struct RegisteredRepo {
        path: PathBuf,
    }

    let mut adoption = RegistryAdoption::default();
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Ok(adoption);
    };
    // A registry this build cannot parse is not a reason to fail every command:
    // it is a file the dashboard owns, and the store works without it.
    let Ok(file) = toml::from_str::<RegistryFile>(&raw) else {
        return Ok(adoption);
    };

    for repo in file.repos {
        let canonical = repo
            .path
            .canonicalize()
            .unwrap_or_else(|_| repo.path.clone());
        let pointer = read_pointer(&canonical).unwrap_or(None);
        let resolved = store.read(|tx| {
            if let Some(pointer) = &pointer
                && let Some(project) = tx.project_by_uuid(&pointer.uuid)?
            {
                return Ok(Some(project.id));
            }
            Ok(tx.project_by_path(&canonical)?.map(|project| project.id))
        })?;
        match resolved {
            Some(project) => {
                store.write(|tx| {
                    tx.touch_project_path(project, &canonical, path_kind(&canonical))
                })?;
                adoption.adopted.push(canonical);
            }
            None => adoption.unmigrated.push(canonical),
        }
    }
    mark_retired(path);
    Ok(adoption)
}

/// Leaves a note beside a legacy registry saying that nothing reads it.
///
/// Written once, never overwritten, and failure is ignored: the marker is a
/// courtesy to whoever finds the directory, and a read-only home directory is
/// not a reason to fail a command that has already done its work.
fn mark_retired(registry_path: &Path) {
    let Some(dir) = registry_path.parent() else {
        return;
    };
    let marker = dir.join("MIGRATED.txt");
    if marker.exists() {
        return;
    }
    let _ = std::fs::write(
        marker,
        "storyhook no longer reads this directory.\n\n         Story data, the project catalog and the daemon's runtime files now live in\n         storyhook's own store — see `story help storage`. `registry.toml` has been\n         read once and its repositories recorded against the projects they belong to.\n\n         Nothing here is deleted, and nothing here is written to. You may remove this\n         directory yourself once you are satisfied you no longer want what is in it.\n",
    );
}
