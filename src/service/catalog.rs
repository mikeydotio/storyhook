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
use crate::output::ProjectView;
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
    /// The story-id prefix minted into every story this project creates.
    pub prefix: String,
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

    /// One project, in full: its identity and the two git associations it may
    /// hold.
    ///
    /// **The scoped singular to [`all`](Self::all)'s plural.** `all` answers
    /// "what projects are there"; this answers "what is *this* project", which
    /// is the question `story project show` exists to ask and the one nothing
    /// in the CLI could answer before SH-120.
    ///
    /// Three reads in one transaction, so the identity, the checkout and the
    /// origins are all observed at the same instant. Reading them separately
    /// would let a concurrent `link checkout` land between two of them and
    /// produce a value describing no state the store was ever in.
    ///
    /// The checkout is reported exactly as recorded, **unchecked**. Whether the
    /// directory is still on disk is [`orphaned`](Self::orphaned)'s question,
    /// and probing it here would put a filesystem call on the path of a lookup —
    /// and would make the answer depend on which machine asked, which is the
    /// dependency this epic exists to remove.
    pub fn describe(&self, project: ProjectId) -> Result<ProjectView, AppError> {
        Ok(self.store.read(|tx| {
            let record = tx.project(project)?.ok_or_else(|| {
                crate::store::StoreError::from(AppError::NotFound(format!(
                    "project `{project:?}` is not in the store"
                )))
            })?;
            Ok(ProjectView {
                slug: record.slug,
                name: record.name,
                prefix: record.prefix,
                checkout: tx.checkout_path(project)?,
                origins: tx
                    .project_remotes(project)?
                    .into_iter()
                    .map(|remote| remote.raw)
                    .collect(),
            })
        })?)
    }

    /// Every project whose checkout knows a git origin the store does not.
    ///
    /// **SH-151's R4, and the reason it is binding on SH-119.** A project used
    /// to be reachable from its own directory through the recorded-path index;
    /// with the index deleted, what answers for a checkout carrying no pointer
    /// file — a fresh clone, most of all — is the origin it reports, and only
    /// if some project has registered it. Registration happens in
    /// `story project new` and `story project link origin`, so a project that
    /// predates those verbs has none.
    ///
    /// The probe is the SH-151 ownership constructor applied to each project's
    /// recorded checkout, and only [`OriginFinding::Registrable`] is ever acted
    /// on. Everything else is reported: an inherited origin belongs to the
    /// repository above, a held one belongs to another project, and an
    /// unanswerable probe is a question for the user rather than a default.
    ///
    /// Costs one `git` invocation per project **with a checkout that is still
    /// on disk**, which is why it lives in `story doctor` and nowhere on the
    /// path of an ordinary command.
    pub fn unregistered_origins(&self) -> Result<Vec<UnregisteredOrigin>, AppError> {
        use crate::domain::remote::RepoOrigin;

        let candidates: Vec<(ProjectId, String, PathBuf)> = self.store.read(|tx| {
            let mut candidates = Vec::new();
            for project in tx.projects()? {
                if !tx.project_remotes(project.id)?.is_empty() {
                    continue;
                }
                let Some(checkout) = tx.checkout_path(project.id)? else {
                    continue;
                };
                if !checkout.is_dir() {
                    continue;
                }
                candidates.push((project.id, project.slug.clone(), checkout));
            }
            Ok(candidates)
        })?;

        let mut findings = Vec::new();
        for (project, slug, checkout) in candidates {
            // Probed outside the read, because it spawns `git`: a subprocess
            // inside a store transaction holds a connection open for however
            // long that process takes.
            let finding = match super::project::origin_at(&checkout) {
                RepoOrigin::Owned(owned) => {
                    match self.store.read(|tx| tx.project_by_remote(owned.url()))? {
                        Some(holder) => OriginFinding::HeldBy {
                            origin: owned.url().clone(),
                            holder: holder.slug,
                        },
                        None => OriginFinding::Registrable(owned),
                    }
                }
                RepoOrigin::Inherited { origin, owner } => {
                    OriginFinding::Inherited { origin, owner }
                }
                RepoOrigin::Unknown(command) => OriginFinding::Unknown(command),
                // A checkout with no origin at all is not a finding. It is an
                // ordinary local repository, or none, and there is nothing to
                // register or to tell anybody about.
                RepoOrigin::Absent => continue,
            };
            findings.push(UnregisteredOrigin {
                project,
                slug,
                checkout,
                finding,
            });
        }
        Ok(findings)
    }

    /// Registers every origin [`unregistered_origins`](Self::unregistered_origins)
    /// found registrable, and returns the whole set it looked at.
    ///
    /// Only `Registrable` is written. The three other findings come back
    /// untouched so the caller can report them, which is R4's "reported, never
    /// guessed at" in the shape `doctor --fix` already uses for the checkout
    /// audit above.
    pub fn register_found_origins(&self) -> Result<Vec<UnregisteredOrigin>, AppError> {
        let found = self.unregistered_origins()?;
        for finding in &found {
            let OriginFinding::Registrable(owned) = &finding.finding else {
                continue;
            };
            let now = crate::service::Clock::System.now();
            self.store.write(|tx| {
                super::project::register_origin(tx, finding.project, owned, &now)?;
                Ok(())
            })?;
        }
        Ok(found)
    }
}

/// A project whose checkout knows an origin the store does not (SH-119, R4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnregisteredOrigin {
    /// The project the finding is about.
    pub project: ProjectId,
    /// Its slug — what a user would name it by.
    pub slug: String,
    /// The checkout that was probed.
    pub checkout: PathBuf,
    /// What the probe found there.
    pub finding: OriginFinding,
}

/// What probing a project's checkout for an origin found.
///
/// Four answers rather than two, because R4 is explicit that a project whose
/// checkout does not own an origin must be **reported, never guessed at**. Only
/// [`Registrable`](Self::Registrable) is acted on; the rest exist so the report
/// can say why nothing was done.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OriginFinding {
    /// The checkout owns an origin, and no project holds it yet.
    Registrable(crate::domain::remote::OwnedOrigin),
    /// The checkout reports an origin belonging to the repository above it.
    /// Registering it here would be one repository wearing two identities.
    Inherited {
        /// The origin, so the report can name it.
        origin: crate::domain::remote::RemoteUrl,
        /// The directory entitled to it.
        owner: PathBuf,
    },
    /// Another project already holds the origin this checkout owns.
    HeldBy {
        /// The origin.
        origin: crate::domain::remote::RemoteUrl,
        /// The slug of the project that holds it.
        holder: String,
    },
    /// The ownership probe could not be run or could not be read, naming the
    /// git invocation that failed.
    Unknown(String),
}

/// One catalog row from a project record.
fn entry(project: ProjectRecord, path: Option<PathBuf>) -> CatalogEntry {
    CatalogEntry {
        project: project.id,
        id: project.slug,
        name: project.name,
        prefix: project.prefix,
        path,
    }
}
