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
use crate::store::{ProjectRecord, ReadOps, Store, WriteOps};

use super::project::{path_kind, read_pointer};

/// One row of `story web list`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogEntry {
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
            Ok(CatalogEntry {
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
        id: project.slug,
        name: project.name,
        path,
    }
}
