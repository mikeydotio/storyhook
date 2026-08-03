//! The dashboard's list of projects.
//!
//! `story web register|deregister|list` used to maintain `~/.storyhook/registry.toml`,
//! a second file that named repositories the dashboard should show and that
//! could disagree with the repositories themselves — a registered path whose
//! `.storyhook/` had since been deleted was a case the dashboard had to survive.
//!
//! In the store there is no second file, and there is no registration step
//! either. A project *is* a catalog entry, so `story project new` puts a project
//! here by creating it and `story project delete` removes it by deleting it.
//! What is left in this module is everything that is *about* the catalog rather
//! than about a project's existence:
//!
//! * [`CatalogService::all`] and [`CatalogService::list`] report it — with and
//!   without the projects this machine has no checkout of,
//! * [`CatalogService::orphaned`] and [`CatalogService::deregister_orphaned`]
//!   are `story doctor`'s half: a linked checkout that is not there any more.
//!
//! Two things used to live here and are gone. `relink` was the answer when a
//! checkout moved rather than went away; `story project link checkout` does the
//! same job without needing a pointer file in the directory it is pointed at,
//! which is precisely what a moved, renamed or freshly cloned checkout may not
//! have. And `adopt_legacy_registry` re-read `~/.storyhook/registry.toml` on
//! every store open, recording each path it named against the project it
//! belonged to — into the resolution index SH-119 deleted. With nowhere to
//! write, it had nothing left to do; the file it read is still on disk,
//! untouched, exactly as it always promised to leave it.

use std::path::PathBuf;

use crate::error::AppError;
use crate::store::{
    PathKind, ProjectId, ProjectPathRecord, ProjectRecord, ReadOps, Store, WriteOps,
};

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
