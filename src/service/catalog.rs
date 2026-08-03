//! The dashboard's list of projects.
//!
//! `story web register|deregister|list` used to maintain `~/.storyhook/registry.toml`,
//! a second file that named repositories the dashboard should show and that
//! could disagree with the repositories themselves — a registered path whose
//! `.storyhook/` had since been deleted was a case the dashboard had to survive.
//!
//! In the store there is no second file, and there is no registration step
//! either. A project *is* a catalog entry and its checkouts are rows beside it,
//! so `story project new` puts a project here by creating it and
//! `story project delete` removes it by deleting it. What is left in this
//! module is everything that is *about* the catalog rather than about a
//! project's existence:
//!
//! * [`CatalogService::all`] and [`CatalogService::list`] report it — with and
//!   without the projects this machine has no checkout of,
//! * [`CatalogService::orphaned`] and [`CatalogService::deregister_orphaned`]
//!   are `story doctor`'s half: registrations pointing at nothing.
//!
//! `relink` used to live here too, as the answer when a checkout moved rather
//! than went away. It is gone: `story project link checkout` does the same job
//! without needing a pointer file in the directory it is pointed at, which is
//! precisely what a moved, renamed or freshly cloned checkout may not have.
//! Keeping both would be two answers to one question in the module SH-119
//! rewrites next.

use std::path::{Path, PathBuf};

use crate::error::AppError;
use crate::store::{
    PathKind, ProjectId, ProjectPathRecord, ProjectRecord, ReadOps, Store, WriteOps,
};

use super::project::{path_kind, read_pointer};

/// One row of `story project list`.
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

/// A registration naming a directory that is not there any more.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrphanedRegistration {
    /// The project the stale registration belongs to.
    pub project: ProjectId,
    /// Its slug, which is what a user deregisters or re-links it by.
    pub slug: String,
    /// The path that no longer exists.
    pub path: PathBuf,
    /// How many stories the project holds — the difference between a fixture
    /// worth forgetting and real work whose checkout has merely moved.
    pub stories: usize,
}

/// The project catalog, over a whole store.
///
/// Not [`Ctx`](super::Ctx)-shaped: the catalog spans every project, so there is
/// no single one for a context to name.
pub struct CatalogService<'a, S: Store> {
    store: &'a S,
}

impl<'a, S: Store> CatalogService<'a, S> {
    /// A catalog service over `store`.
    pub fn new(store: &'a S) -> Self {
        Self { store }
    }

    /// Every registered checkout whose directory is no longer on disk.
    ///
    /// A registration is a claim that a project can be opened at a path. When
    /// the path is gone the claim is stale, and a stale claim is not harmless:
    /// it is a row in `story project list` and a card on the dashboard's home
    /// screen, indistinguishable from a real repository until it is clicked.
    ///
    /// This is what 394 fixture directories looked like after their test run
    /// ended — every one of them registered, every one pointing at nothing.
    ///
    /// Reported rather than repaired on sight. A path can be missing because an
    /// external disk is not mounted, and silently forgetting a real project's
    /// only registration because a volume was unplugged would be a worse defect
    /// than the one being fixed. `story doctor --fix` is where the user says
    /// yes, and `story project link checkout` is the answer when the checkout
    /// moved rather than went away.
    pub fn orphaned(&self) -> Result<Vec<OrphanedRegistration>, AppError> {
        Ok(self.store.read(|tx| {
            let mut orphans = Vec::new();
            for project in tx.projects()? {
                let stories = tx
                    .stories(project.id, &crate::store::StoryQuery::all())?
                    .len();
                for record in tx.project_paths(project.id)? {
                    let path = PathBuf::from(&record.path);
                    if !path.exists() {
                        orphans.push(OrphanedRegistration {
                            project: project.id,
                            slug: project.slug.clone(),
                            path,
                            stories,
                        });
                    }
                }
            }
            Ok(orphans)
        })?)
    }

    /// Forgets every registration [`orphaned`](Self::orphaned) found, and
    /// returns them.
    ///
    /// Only the path rows go — and the recorded checkout with them, when it is
    /// the same vanished directory. The project, its stories and its identity
    /// all survive: it stays in `story project list` and on the dashboard, and
    /// `story project link checkout` or a fresh `story project new` puts a
    /// path back. That reversibility is what makes it safe to offer from
    /// `doctor --fix` rather than demanding a hand-written transaction.
    ///
    /// The checkout half is not decoration. One directory is recorded in two
    /// places — a `project_paths` row that resolution reads, and
    /// `checkout_path` that says where the project's repo-side work runs — and
    /// forgetting one without the other leaves `story project list` printing a
    /// directory that is gone, on the line below the one that just stopped
    /// printing it. See [`forget_checkout`](super::project::forget_checkout).
    pub fn deregister_orphaned(&self) -> Result<Vec<OrphanedRegistration>, AppError> {
        let orphans = self.orphaned()?;
        self.store.write(|tx| {
            for orphan in &orphans {
                tx.forget_project_path(orphan.project, &orphan.path)?;
                super::project::forget_checkout(tx, orphan.project, &orphan.path)?;
            }
            Ok(())
        })?;
        Ok(orphans)
    }

    /// Every project with a known checkout, ordered by slug.
    pub fn list(&self) -> Result<Vec<CatalogEntry>, AppError> {
        Ok(self
            .all()?
            .into_iter()
            .filter(|entry| entry.path.is_some())
            .collect())
    }

    /// Every project the store knows, checkout or no checkout, ordered by slug.
    ///
    /// The difference from [`list`](Self::list) is a project this machine has
    /// no copy of: one whose checkout was deleted and then forgotten by
    /// `story doctor --fix`, or one that arrived by import. Its stories are in
    /// the store and perfectly readable, so a surface that shows only
    /// `list`'s answer is a surface from which that work cannot be reached at
    /// all.
    ///
    /// `list` still exists, and still filters, for the callers that genuinely
    /// need a directory to act in.
    pub fn all(&self) -> Result<Vec<CatalogEntry>, AppError> {
        Ok(self.store.read(|tx| {
            let mut entries = Vec::new();
            for project in tx.projects()? {
                let path = preferred_checkout(tx.project_paths(project.id)?);
                entries.push(entry(project, path));
            }
            Ok(entries)
        })?)
    }
}

/// The checkout a caller should act in, given every checkout a project has.
///
/// The main working tree wins; a linked worktree is the fallback; a project
/// with neither has none. `project_paths` is ordered by *path*, so without this
/// the answer was whichever directory happened to sort first — and a linked
/// worktree is a branch somebody is working on, whose hooks and whose
/// `AGENTS.md` belong to that branch rather than to the project. `PathKind` has
/// been recorded since schema 1 and, until now, consulted nowhere.
#[must_use]
pub fn preferred_checkout(mut paths: Vec<ProjectPathRecord>) -> Option<PathBuf> {
    paths.sort_by_key(|record| match record.kind {
        PathKind::Main => 0,
        PathKind::Worktree => 1,
    });
    paths.into_iter().next().map(|r| PathBuf::from(r.path))
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
