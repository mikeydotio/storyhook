//! A project's optional git layer: the origins it answers to, and the one
//! checkout its repo-side work runs in (SH-117).
//!
//! # Two associations, and the asymmetry is the whole design
//!
//! * **An origin** is the *only* thing project selection ever consults. A URL
//!   belongs to at most one project, enforced by the unique index on
//!   `project_remotes.normalized`, which is what makes an ambiguous resolution
//!   unrepresentable rather than handled.
//! * **A checkout** is *never* consulted for resolution — not as a fallback,
//!   not as a hint, not for disambiguation. It answers a different question:
//!   where do this project's repo-side operations execute? At most one per
//!   project, and its only consumer will be `dispatch` (SH-120). A second
//!   consumer appearing is the signal that this design has drifted.
//!
//! A file of its own rather than more methods on
//! [`ProjectService`](super::project::ProjectService), which exists to bring a
//! project into being, or on [`CatalogService`](super::catalog::CatalogService),
//! which spans every project. This is a third thing, and it is the thing SH-119
//! and SH-120 both edit next.
//!
//! # Why these verbs need a resolved project and `init` does not
//!
//! Everything here names an existing project, so the ordinary selection rules
//! apply — `--project`, `$STORYHOOK_PROJECT`, or the working directory — and the
//! refusal when none answers is
//! [`no_project_refusal`](super::project::no_project_refusal), the one
//! constructor for it. That is also what lets `link checkout` do what `story
//! relink` did without needing a pointer file in the directory it is pointed at.

use std::path::{Path, PathBuf};

use crate::domain::remote::RemoteUrl;
use crate::error::AppError;
use crate::service::Ctx;
use crate::store::{ReadOps, Store, WriteOps};

/// One origin registration, for the message that reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OriginLink {
    /// The project's slug, so a report names it the way `story project list`
    /// does rather than by a database id.
    pub project: String,
    /// The URL exactly as it was given.
    pub raw: String,
    /// The identity key resolution matches on — every spelling of one origin
    /// collapses to this.
    pub normalized: String,
}

/// What a checkout link changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckoutLink {
    /// The project's slug.
    pub project: String,
    /// The checkout now recorded; `None` after an unlink.
    pub path: Option<PathBuf>,
    /// What was recorded before, so a report can say what it *replaced* rather
    /// than only what it wrote. A silent replacement is how somebody discovers
    /// six weeks later that dispatch has been running in the wrong tree.
    pub replaced: Option<PathBuf>,
}

/// A project's git associations.
pub struct GitLinkService<'ctx, S: Store> {
    ctx: &'ctx Ctx<'ctx, S>,
}

impl<'ctx, S: Store> GitLinkService<'ctx, S> {
    /// A git-link service bound to `ctx`'s project.
    #[must_use]
    pub fn new(ctx: &'ctx Ctx<'ctx, S>) -> Self {
        Self { ctx }
    }

    /// Registers `remote` as an origin this project answers to.
    ///
    /// Idempotent for the holder: re-registering refreshes the raw spelling and
    /// the timestamp rather than failing, which is what makes this safe to
    /// re-run from a script.
    ///
    /// **Loud for anybody else.** This is the site SH-116 deferred that loudness
    /// to: `story project init` *skips* a collision, because the user typed no
    /// URL and a monorepo's second project must still be creatable, while here
    /// the user typed the URL and is owed an answer about it.
    ///
    /// # Why the holder is read outside the transaction
    ///
    /// The store already refuses a cross-project registration — `link_remote`
    /// raises `StoreError::Invariant`, and the unique index on
    /// `project_remotes.normalized` is under that. But an `Invariant` surfaces
    /// as [`AppError::Integrity`] (exit 5, "the store is damaged"), which is the
    /// wrong diagnosis for an ordinary conflict, and SQLite's constraint error
    /// names a column where a user needs a project *slug* they can act on.
    ///
    /// So the holder is looked up first, purely to produce the right refusal.
    /// That read and the write are not one transaction, and the consequence is
    /// stated rather than hidden: two `link origin` calls racing for one URL
    /// will have the loser refused by the index instead, with exit 5 and an
    /// uglier message. Nothing is mis-registered either way — the index is what
    /// guarantees that, and it is still the backstop.
    pub fn link_origin(&self, remote: &RemoteUrl) -> Result<OriginLink, AppError> {
        let now = self.ctx.now();
        let project = self.ctx.project();
        let slug = self.slug()?;
        if let Some(holder) = self.ctx.store().read(|tx| tx.project_by_remote(remote))?
            && holder.id != project
        {
            return Err(AppError::Validation(format!(
                "`{raw}` is already registered to project `{held}`, and a git origin belongs to \
                 at most one project.\n\nIf `{held}` should no longer own it:\n\n  story \
                 --project {held} project unlink origin {raw}\n  story --project {slug} project \
                 link origin {raw}\n\n`story project list` shows which project holds which \
                 origin.",
                raw = remote.raw(),
                held = holder.slug,
            )));
        }
        self.ctx
            .store()
            .write(|tx| tx.link_remote(project, remote, &now))?;
        Ok(OriginLink {
            project: slug,
            raw: remote.raw().to_string(),
            normalized: remote.key().to_string(),
        })
    }

    /// Forgets one origin of this project.
    ///
    /// A URL this project does not hold is [`AppError::NotFound`] rather than a
    /// quiet success: "unlink did nothing" and "unlink removed the wrong thing"
    /// are indistinguishable to a caller that is told neither, and the holder is
    /// named because the commonest cause is running it against the wrong
    /// project.
    pub fn unlink_origin(&self, remote: &RemoteUrl) -> Result<OriginLink, AppError> {
        let project = self.ctx.project();
        let slug = self.slug()?;
        let removed = self
            .ctx
            .store()
            .write(|tx| tx.unlink_remote(project, remote))?;
        if !removed {
            let holder = self
                .ctx
                .store()
                .read(|tx| tx.project_by_remote(remote))?
                .map_or_else(
                    || "no project does".to_string(),
                    |holder| format!("project `{}` does", holder.slug),
                );
            return Err(AppError::NotFound(format!(
                "project `{slug}` does not hold `{}` — {holder}.\n\n`story project list` shows \
                 which project holds which origin.",
                remote.raw(),
            )));
        }
        Ok(OriginLink {
            project: slug,
            raw: remote.raw().to_string(),
            normalized: remote.key().to_string(),
        })
    }

    /// Records `path` as where this project's repo-side work runs, replacing
    /// whatever was recorded before.
    ///
    /// The directory must exist, and that is the only thing checked. It is not
    /// required to be a git repository — a checkout that has not been cloned yet
    /// is a legitimate thing to name ahead of time, and the one consumer fails
    /// loudly on its own — and it is not required to be unique across projects,
    /// because nothing resolves a project by it. See migration 0007's header.
    pub fn link_checkout(&self, path: &Path) -> Result<CheckoutLink, AppError> {
        if !path.is_dir() {
            return Err(AppError::NotFound(format!(
                "cannot link `{}` as a checkout: no such directory",
                path.display()
            )));
        }
        let path = crate::env::canonical_ish(path)?;
        let project = self.ctx.project();
        let slug = self.slug()?;
        let replaced = self.ctx.store().read(|tx| tx.checkout_path(project))?;
        self.ctx
            .store()
            .write(|tx| tx.set_checkout_path(project, Some(&path)))?;
        Ok(CheckoutLink {
            project: slug,
            path: Some(path),
            replaced,
        })
    }

    /// Forgets this project's checkout, reporting what went.
    ///
    /// A project that had none is a success rather than a refusal, unlike
    /// [`unlink_origin`](Self::unlink_origin): there is exactly one checkout
    /// slot, so "there was nothing there" is unambiguous and the caller cannot
    /// have removed the wrong one.
    pub fn unlink_checkout(&self) -> Result<CheckoutLink, AppError> {
        let project = self.ctx.project();
        let slug = self.slug()?;
        let replaced = self.ctx.store().read(|tx| tx.checkout_path(project))?;
        if replaced.is_some() {
            self.ctx
                .store()
                .write(|tx| tx.set_checkout_path(project, None))?;
        }
        Ok(CheckoutLink {
            project: slug,
            path: None,
            replaced,
        })
    }

    /// This project's slug — how every message here names it.
    fn slug(&self) -> Result<String, AppError> {
        let project = self.ctx.project();
        let record = self.ctx.store().read(|tx| tx.project(project))?;
        record
            .map(|record| record.slug)
            .ok_or_else(|| AppError::NotFound("this project is no longer in the store".to_string()))
    }
}
