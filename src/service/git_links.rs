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
    /// What happened to the directory's `.storyhook.toml` — the artifact
    /// project *resolution* actually reads, as opposed to `checkout_path`
    /// above, which resolution never does (SH-167).
    pub pointer: PointerOutcome,
}

/// What a checkout link or unlink did to the directory's `.storyhook.toml`.
///
/// `checkout_path` and the pointer file answer different questions and have
/// different scopes. `checkout_path` is machine-local and many-to-one — two
/// projects may legitimately name the same directory as their checkout,
/// which is a monorepo rather than an ambiguity
/// (`store::conformance::two_projects_may_name_the_same_checkout`). The
/// pointer file is committed, repository-global, and one-to-one: a directory
/// has exactly one identity. So `link checkout` always records the first,
/// and only ever writes the second when the directory is unclaimed or
/// already claims this same project — never by overwriting a different
/// project's identity, which would silently steal it out from under every
/// other clone on their next pull.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PointerOutcome {
    /// No pointer file existed; one was written naming this project.
    Written(PathBuf),
    /// A pointer file already named this project with the same prefix, so
    /// nothing needed to change and nothing was written — re-running `link
    /// checkout` leaves the working tree clean.
    AlreadyCorrect(PathBuf),
    /// A pointer file already named this project but with a stale `prefix`,
    /// now repaired. Any `[plugin]`/`[hooks]` tables it carried came through
    /// untouched.
    PrefixRepaired {
        path: PathBuf,
        was: String,
        now: String,
    },
    /// The directory's pointer file names a *different* project. Left
    /// exactly as it was — a checkout link is not a claim on identity, and
    /// the directory keeps resolving to whichever project its pointer names.
    AnotherProject {
        path: PathBuf,
        uuid: String,
        holder: Option<String>,
    },
    /// The link was recorded in the store; the pointer file itself could not
    /// be written. The directory will not resolve until it exists.
    Unwritable { path: PathBuf, reason: String },
    /// `unlink` only: a pointer file is present and is deliberately left in
    /// place — it is committed, may carry user-authored configuration, and
    /// other clones resolve by it.
    LeftInPlace(PathBuf),
    /// `unlink` only: there was no pointer file to leave.
    NoPointer,
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
    /// to: `story project new` *skips* a collision, because the user typed no
    /// URL and a monorepo's second project must still be creatable, while here
    /// the user typed the URL and is owed an answer about it.
    ///
    /// # Why the holder is found inside the transaction
    ///
    /// The store already refuses a cross-project registration — `link_remote`
    /// raises `StoreError::Invariant`, and the unique index on
    /// `project_remotes.normalized` is under that. But an `Invariant` surfaces
    /// as [`AppError::Integrity`] (exit 5, "the store is damaged"), which is the
    /// wrong diagnosis for an ordinary conflict, and SQLite's constraint error
    /// names a column where a user needs a project *slug* they can act on.
    ///
    /// So the holder is looked up to produce the right refusal — inside the
    /// same transaction as the write, through
    /// [`register_origin`](crate::service::project::register_origin). It used
    /// to be a separate read beforehand, with the race stated as a known cost:
    /// two `link origin` calls racing for one URL had the loser refused by the
    /// index instead, with exit 5 and an uglier message. SH-151 needed the
    /// funnel anyway, and one transaction closes that window for free.
    pub fn link_origin(
        &self,
        origin: &crate::domain::remote::OwnedOrigin,
    ) -> Result<OriginLink, AppError> {
        let now = self.ctx.now();
        let project = self.ctx.project();
        let slug = self.slug()?;
        let remote = origin.url();
        let registration = self
            .ctx
            .store()
            .write(|tx| crate::service::project::register_origin(tx, project, origin, &now))?;
        if let crate::service::project::Registration::HeldBy(held) = registration {
            return Err(AppError::Validation(format!(
                "`{raw}` is already registered to project `{held}`, and a git origin belongs to \
                 at most one project.\n\nIf `{held}` should no longer own it:\n\n  story \
                 --project {held} project unlink origin {raw}\n  story --project {slug} project \
                 link origin {raw}\n\n`story project list` shows which project holds which \
                 origin.",
                raw = remote.raw(),
            )));
        }
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
    /// whatever was recorded before — and, unless the directory already
    /// claims a *different* project, writes or repairs the `.storyhook.toml`
    /// pointer that lets a bare story id resolve there (SH-167).
    ///
    /// The directory must exist, and that is the only thing checked. It is not
    /// required to be a git repository — a checkout that has not been cloned yet
    /// is a legitimate thing to name ahead of time, and the one consumer fails
    /// loudly on its own — and it is not required to be unique across projects,
    /// because nothing resolves a project by it. See migration 0007's header.
    ///
    /// # The pointer file is read before anything is written
    ///
    /// A pointer that exists but will not parse refuses here — naming the
    /// file, via [`read_pointer`](crate::service::project::read_pointer)'s own
    /// message — before the store is touched at all. There is no way to tell
    /// "this is mine, just stale" from "this is someone else's" through a
    /// syntax error, and guessing wrong in either direction is worse than
    /// asking the caller to look.
    ///
    /// # Store write, then pointer write, matching `story project new` and
    /// `set_prefix`
    ///
    /// The store commits first. A failed pointer write after that is reported
    /// rather than propagated — [`PointerOutcome::Unwritable`] — because
    /// failing into today's status quo (a recorded checkout with no pointer)
    /// is better than a pointer-first ordering's failure mode: a store write
    /// that then fails would strand a `.storyhook.toml` naming a project this
    /// store does not have, which is `invoke::unresolvable_pointer_refusal`'s
    /// state.
    pub fn link_checkout(&self, path: &Path) -> Result<CheckoutLink, AppError> {
        if !path.is_dir() {
            return Err(AppError::NotFound(format!(
                "cannot link `{}` as a checkout: no such directory",
                path.display()
            )));
        }
        let path = crate::env::canonical_ish(path)?;
        let project = self.ctx.project();
        let record = self.project_record()?;

        // Read-before-write, and outside any transaction: a syntax error here
        // must refuse before the store is touched, which `?` gives for free.
        let existing_pointer = crate::service::project::read_pointer(&path)?;

        let replaced = self.ctx.store().read(|tx| tx.checkout_path(project))?;
        self.ctx
            .store()
            .write(|tx| tx.set_checkout_path(project, Some(&path)))?;

        let pointer = self.reconcile_pointer(&path, &record, existing_pointer)?;

        Ok(CheckoutLink {
            project: record.slug,
            path: Some(path),
            replaced,
            pointer,
        })
    }

    /// Writes, repairs, or leaves alone the pointer file at `path`, given
    /// what it already contained (if anything) and this project's identity.
    /// Never called before the store write it reports alongside has
    /// committed — see [`link_checkout`](Self::link_checkout)'s ordering note.
    fn reconcile_pointer(
        &self,
        path: &Path,
        record: &crate::store::ProjectRecord,
        existing: Option<crate::service::project::ProjectPointer>,
    ) -> Result<PointerOutcome, AppError> {
        let pointer_path = crate::service::project::pointer_path(path);

        let Some(mut pointer) = existing else {
            let fresh = crate::service::project::ProjectPointer::new(
                record.uuid.clone(),
                record.prefix.clone(),
            );
            return Ok(match crate::service::project::write_pointer(path, &fresh) {
                Ok(()) => PointerOutcome::Written(pointer_path),
                Err(error) => PointerOutcome::Unwritable {
                    path: pointer_path,
                    reason: error.to_string(),
                },
            });
        };

        if pointer.uuid != record.uuid {
            let holder = self
                .ctx
                .store()
                .read(|tx| tx.project_by_uuid(&pointer.uuid))?
                .map(|held| held.slug);
            return Ok(PointerOutcome::AnotherProject {
                path: pointer_path,
                uuid: pointer.uuid,
                holder,
            });
        }

        if pointer.prefix == record.prefix {
            return Ok(PointerOutcome::AlreadyCorrect(pointer_path));
        }

        let was = std::mem::replace(&mut pointer.prefix, record.prefix.clone());
        Ok(
            match crate::service::project::write_pointer(path, &pointer) {
                Ok(()) => PointerOutcome::PrefixRepaired {
                    path: pointer_path,
                    was,
                    now: record.prefix.clone(),
                },
                Err(error) => PointerOutcome::Unwritable {
                    path: pointer_path,
                    reason: error.to_string(),
                },
            },
        )
    }

    /// Forgets this project's checkout, reporting what went.
    ///
    /// A project that had none is a success rather than a refusal, unlike
    /// [`unlink_origin`](Self::unlink_origin): there is exactly one checkout
    /// slot, so "there was nothing there" is unambiguous and the caller cannot
    /// have removed the wrong one.
    ///
    /// **Never deletes the pointer file (SH-167).** `.storyhook.toml` is
    /// committed to the repository and may carry user-authored `[plugin]`/
    /// `[hooks]` tables; deleting it would propagate to every other clone on
    /// their next pull and destroy configuration this service has promised
    /// never to write, let alone remove. `story project delete` sets the same
    /// precedent for the checkout itself: it "never touches the project's own
    /// files on disk."
    pub fn unlink_checkout(&self) -> Result<CheckoutLink, AppError> {
        let project = self.ctx.project();
        let slug = self.slug()?;
        let replaced = self.ctx.store().read(|tx| tx.checkout_path(project))?;
        if replaced.is_some() {
            self.ctx
                .store()
                .write(|tx| tx.set_checkout_path(project, None))?;
        }
        let pointer = match &replaced {
            Some(path) if crate::service::project::pointer_path(path).is_file() => {
                PointerOutcome::LeftInPlace(crate::service::project::pointer_path(path))
            }
            _ => PointerOutcome::NoPointer,
        };
        Ok(CheckoutLink {
            project: slug,
            path: None,
            replaced,
            pointer,
        })
    }

    /// This project's full record — identity, slug and prefix — for the
    /// pointer reconciliation [`link_checkout`](Self::link_checkout) needs
    /// beyond the slug [`slug`](Self::slug) alone answers.
    fn project_record(&self) -> Result<crate::store::ProjectRecord, AppError> {
        let project = self.ctx.project();
        self.ctx
            .store()
            .read(|tx| tx.project(project))?
            .ok_or_else(|| AppError::NotFound("this project is no longer in the store".to_string()))
    }

    /// This project's slug — how every message here names it.
    fn slug(&self) -> Result<String, AppError> {
        self.project_record().map(|record| record.slug)
    }
}
