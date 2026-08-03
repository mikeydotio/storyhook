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
use crate::store::{ProjectId, ProjectRecord, ReadOps, Store};

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

/// A linked checkout naming a directory that is not there any more.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrphanedRegistration {
    /// The project the stale link belongs to.
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

    /// Every linked checkout whose directory is no longer on disk.
    ///
    /// A linked checkout is a claim that a project can be opened at a path. When
    /// the path is gone the claim is stale, and a stale claim is not harmless:
    /// it is a row in `story project list` and a card on the dashboard's home
    /// screen, indistinguishable from a real repository until it is clicked.
    ///
    /// This is what 394 fixture directories looked like after their test run
    /// ended — every one of them recorded, every one pointing at nothing.
    ///
    /// Reported rather than repaired on sight. A path can be missing because an
    /// external disk is not mounted, and silently forgetting a real project's
    /// only checkout because a volume was unplugged would be a worse defect
    /// than the one being fixed. `story doctor --fix` is where the user says
    /// yes, and `story project link checkout` is the answer when the checkout
    /// moved rather than went away.
    ///
    /// # What it audits now
    ///
    /// `checkout_path`, and nothing else. SH-119 named this method for deletion
    /// along with the `project_paths` index it read, on the ground that it
    /// "exists only to police stored paths" — and `checkout_path`, which did not
    /// exist when that was written, is the stored path that survives. Deleting
    /// the audit outright would leave `story project list` and the dashboard
    /// printing a directory that is gone, with no command to clean it up. So the
    /// subject narrows rather than the method going: one path per project, the
    /// one `story project link checkout` records.
    pub fn orphaned(&self) -> Result<Vec<OrphanedRegistration>, AppError> {
        Ok(self.store.read(|tx| {
            let mut orphans = Vec::new();
            for project in tx.projects()? {
                let Some(path) = tx.checkout_path(project.id)? else {
                    continue;
                };
                if path.exists() {
                    continue;
                }
                orphans.push(OrphanedRegistration {
                    project: project.id,
                    slug: project.slug.clone(),
                    path,
                    stories: tx
                        .stories(project.id, &crate::store::StoryQuery::all())?
                        .len(),
                });
            }
            Ok(orphans)
        })?)
    }

    /// Forgets every link [`orphaned`](Self::orphaned) found, and returns them.
    ///
    /// Only the link goes. The project, its stories and its identity all
    /// survive: it stays in `story project list` and on the dashboard, and
    /// `story project link checkout` or a fresh `story project new` puts a path
    /// back. That reversibility is what makes it safe to offer from
    /// `doctor --fix` rather than demanding a hand-written transaction.
    ///
    /// Conditional on the paths still matching, which is
    /// [`forget_checkout`](super::project::forget_checkout)'s own rule: a
    /// project pointed somewhere else between the report and the repair must not
    /// lose a link somebody made on purpose.
    pub fn deregister_orphaned(&self) -> Result<Vec<OrphanedRegistration>, AppError> {
        let orphans = self.orphaned()?;
        self.store.write(|tx| {
            for orphan in &orphans {
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
    ///
    /// The directory is `checkout_path` — the one a project names, rather than
    /// the best of the several the resolution index used to hold. Choosing
    /// between them was `preferred_checkout`'s job, and it has none: a project
    /// has at most one checkout by construction, and a linked worktree is
    /// nobody's answer to "where does this project's work run".
    pub fn all(&self) -> Result<Vec<CatalogEntry>, AppError> {
        Ok(self.store.read(|tx| {
            let mut entries = Vec::new();
            for project in tx.projects()? {
                let path = tx.checkout_path(project.id)?;
                entries.push(entry(project, path));
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
