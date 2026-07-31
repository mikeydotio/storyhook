//! Bringing a project into existence.
//!
//! `story project init` used to be a sequence of independent filesystem writes: make
//! the directories, write `project.toml`, write `states.toml`, write
//! `types.toml`, write `members.jsonl`, write the id counter, open the archive
//! database. Any failure between two of them left a project that existed
//! enough to be found and not enough to be used — a half-initialised tracker
//! whose repair story was "delete the directory and start again".
//!
//! Here the project row, its default states, its default types and its id
//! counter are one transaction: either the project exists completely or it was
//! never created.
//!
//! # What still touches the repository
//!
//! Two artifacts, both of them content a user asked for by running `init`, and
//! neither written by any other command:
//!
//! * `AGENTS.md`, generated for agent discoverability when the repository has
//!   none — the same decision the legacy path made, replicated here.
//! * the pointer file, the repository's copy of *which* project this checkout
//!   belongs to. Writing it is a parameter ([`InitOptions::pointer`]) rather
//!   than a rule, because pre-flip the legacy tree is still the identity of
//!   record and a stray pointer file would be a second, disagreeing answer.
//!   The wave that flips the default turns it on.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::domain::{StateDef, SuperState, TypeDef};
use crate::error::AppError;
use crate::output::DeinitPlan;
use crate::store::{
    DeletedProject, NewProject, PathKind, ProjectId, ProjectRecord, ReadOps, Store, WriteOps,
};

use super::Clock;
use super::templates;

/// The story-id prefix a project gets when `init` is not told one.
pub const DEFAULT_PREFIX: &str = "SH";

/// Set to a non-empty value to permit creating a project under a temporary
/// directory in a store that is not itself temporary. See
/// [`refuse_temp_project_in_real_store`].
pub const ALLOW_TEMP_PROJECT: &str = "STORYHOOK_ALLOW_TEMP_PROJECT";

/// Refuses to create a project at a throwaway path inside a store that is not
/// throwaway (SH-95).
///
/// The invariant is one sentence: **a throwaway project may only be created in
/// a throwaway store.**
///
/// Before the flip, `story project init` wrote a `.storyhook/` directory into the
/// directory it was run in, so a fixture's data lived in the fixture and was
/// deleted with it. Storage isolated itself, and no test suite driving the CLI
/// ever had to think about it — so none of them did. One global store ended
/// that silently: every one of those fixture sites became a permanent write
/// into the developer's real tracker, with no error and nothing to notice. It
/// went unnoticed until a single run of one repository's suite put 394 projects
/// into a real store, 234 of them carrying stories.
///
/// **Why both halves are required.** Refusing every temporary path would be
/// wrong and would break this project's own suite, whose fixtures legitimately
/// live under `/private/tmp` — with a store that lives there too. A temporary
/// project in a temporary store is exactly what a test *should* build. The
/// defect is the mismatch, so the mismatch is what is refused.
///
/// **Why it fails here rather than being cleaned up later.** Hiding these
/// afterwards — in `doctor`, or in the dashboard — treats a symptom while the
/// writes continue. This is the only point where the mistake is still cheap:
/// nothing has been written, and the message can name the one environment
/// variable that fixes the caller for good.
pub fn refuse_temp_project_in_real_store(root: &Path, data_home: &Path) -> Result<(), AppError> {
    let allowed = std::env::var(ALLOW_TEMP_PROJECT).is_ok_and(|v| !v.trim().is_empty());
    refuse_temp_project(root, data_home, allowed)
}

/// The decision behind [`refuse_temp_project_in_real_store`], with the
/// environment lifted out into `allowed`.
///
/// Separated so it can be tested without mutating process environment, which
/// two tests running in parallel cannot do safely.
fn refuse_temp_project(root: &Path, data_home: &Path, allowed: bool) -> Result<(), AppError> {
    if allowed || !is_under_temp(root) || is_under_temp(data_home) {
        return Ok(());
    }
    Err(AppError::Validation(format!(
        "refusing to create a project at `{}`: it is under a temporary directory, and the store \
         at `{}` is not.\n\nNothing has been written. A project created here would outlive the \
         directory it names by a long way — the directory is deleted at the end of the run, and \
         the project stays in the store forever, pointing at nothing.\n\nIf this is a test \
         suite, give it a store of its own by exporting STORYHOOK_DATA_DIR to a directory \
         inside the fixture; that is what makes its writes disappear with it. If you really do \
         want a throwaway project in this store, set {ALLOW_TEMP_PROJECT}=1.",
        root.display(),
        data_home.display(),
    )))
}

/// Whether `path` is inside a directory the operating system may reclaim.
///
/// `$TMPDIR` is consulted first because that is where `mktemp` and
/// `tempfile` land by default, and the literals cover the cases it does not:
/// `/tmp` and `/private/tmp` (the same directory on macOS, reached by either
/// name), and `/var/folders`, which is `$TMPDIR`'s real home for a login
/// session other than this process's.
///
/// Every root is compared in both its literal and its canonical spelling, and
/// so is the path — four comparisons rather than one, for a reason the tests
/// pin. `canonical` falls back to its input for a path that does not exist,
/// and the path being judged here usually *does not exist yet*: it is about to
/// be created. So `/tmp/fixture` stays `/tmp/fixture` while the root `/tmp`
/// canonicalizes to `/private/tmp`, and comparing only canonical forms lets
/// exactly the case this guard exists for walk straight through.
pub(crate) fn is_under_temp(path: &Path) -> bool {
    let literal = path.to_path_buf();
    let resolved = canonical(path);
    let mut roots = vec![std::env::temp_dir()];
    roots.extend(
        [
            "/tmp",
            "/private/tmp",
            "/var/folders",
            "/private/var/folders",
        ]
        .into_iter()
        .map(PathBuf::from),
    );
    roots.iter().any(|root| {
        let root_resolved = canonical(root);
        literal.starts_with(root)
            || literal.starts_with(&root_resolved)
            || resolved.starts_with(root)
            || resolved.starts_with(&root_resolved)
    })
}

/// The repository-side file naming the project a checkout belongs to, and
/// carrying the repository's own storyhook configuration.
///
/// This is the *whole* repo footprint of the new design: one small committed
/// file. It holds two different kinds of thing, and the distinction is what
/// keeps the design honest:
///
/// * **Identity** — [`schema`](Self::schema), [`uuid`](Self::uuid) and
///   [`prefix`](Self::prefix), written once by `story project init` so that a fresh
///   clone on another machine knows which project it is looking at before it
///   has any local database row to consult.
/// * **Configuration** — the optional [`plugin`](Self::plugin) and
///   [`hooks`](Self::hooks) tables, which are *user-authored* and which
///   storyhook reads and never writes. They used to be
///   `.storyhook/plugin-config.toml` and `.storyhook/hooks.toml`; they belong
///   in the repository because they are decisions about *this* repository, not
///   data about its stories, and folding them into the pointer means the
///   directory can die without taking a shipped feature with it.
///
/// It deliberately does not carry states, types, members or stories. Those
/// live in the store; a repository that carried its own copy would be a second
/// source of truth, which is the thing this whole rearchitecture exists to
/// delete.
///
/// # Why the two config tables are untyped here
///
/// They are [`toml::Value`], not typed structs, for two reasons that both
/// matter. First, **resolution must not depend on configuration**: a typo in a
/// hook definition would otherwise fail the whole parse and make the repository
/// unresolvable — storyhook would stop knowing which project it was standing
/// in because a timeout was misspelled. Second, storyhook round-trips this file
/// only in the sense of never rewriting it; keeping the tables opaque means a
/// field a newer storyhook understands cannot be silently dropped by an older
/// one that reads the pointer for its identity alone.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectPointer {
    /// Format version, so a later shape change is detectable rather than
    /// silently misread.
    pub schema: u32,
    /// The project's portable identity — [`crate::store::ProjectRecord::uuid`].
    pub uuid: String,
    /// The project's story-id prefix, duplicated here so a clone can render
    /// ids before it can reach the store.
    pub prefix: String,
    /// The `[plugin]` table, if the repository has one. User-authored;
    /// storyhook never writes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<toml::Value>,
    /// The `[hooks]` table, if the repository has one. User-authored;
    /// storyhook never writes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks: Option<toml::Value>,
}

impl ProjectPointer {
    /// A pointer carrying identity and no configuration — what `story project init`
    /// writes.
    #[must_use]
    pub fn new(uuid: String, prefix: String) -> Self {
        Self {
            schema: POINTER_SCHEMA,
            uuid,
            prefix,
            plugin: None,
            hooks: None,
        }
    }
}

/// The nearest ancestor of `root` (itself included) holding a legacy
/// `.storyhook/` project, if there is one.
///
/// A *project*, not merely a directory: `<dir>/.storyhook/project.toml` is what
/// `storage::ensure_project` looked for, and a repository that has only a
/// `.storyhook/hooks.toml` — configuration in its old home, which is still read
/// — has not been left behind by anything and must not be reported as
/// unmigrated.
#[must_use]
pub fn legacy_project_at(root: &Path) -> Option<PathBuf> {
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    canonical
        .ancestors()
        .find(|dir| dir.join(".storyhook/project.toml").is_file())
        .map(Path::to_path_buf)
}

/// What to tell someone standing in a repository storyhook has not imported.
///
/// Loud and specific, because the alternative is worse in both directions: a
/// bare "not initialized" invites `story project init`, which would mint an *empty*
/// second project beside data the user still has, and a silent fallback to
/// reading the directory is the thing this whole rearchitecture exists to
/// stop.
#[must_use]
pub fn unmigrated_error(tree: &Path) -> AppError {
    AppError::NotFound(format!(
        "`{}` still keeps its stories in a `.storyhook/` directory, and storyhook now \
         reads a single store outside your repositories. Run `story migrate` there to \
         bring them across — it never writes to the directory it reads, so your data \
         stays where it is until you are satisfied. `story migrate --dry-run` shows what \
         it would import.",
        tree.display()
    ))
}

/// The repository's `[hooks]` table, if it has a readable one.
///
/// Fails **open** at every step, and deliberately: a repository whose pointer
/// file cannot be parsed still has to be usable, and a hook nobody can read is
/// a hook that does not fire rather than a command that refuses to run.
#[must_use]
pub fn pointer_hooks(root: &Path) -> Option<toml::Value> {
    read_pointer(root).ok().flatten().and_then(|p| p.hooks)
}

/// The repository's `[plugin]` table, if it has a readable one. See
/// [`pointer_hooks`].
#[must_use]
pub fn pointer_plugin(root: &Path) -> Option<toml::Value> {
    read_pointer(root).ok().flatten().and_then(|p| p.plugin)
}

/// The pointer format this build writes.
pub(crate) const POINTER_SCHEMA: u32 = 1;

/// Where the pointer file lives: `<root>/.storyhook.toml`.
///
/// A file rather than a directory, and beside the legacy `.storyhook/` rather
/// than inside it, so that the wave which deletes the directory does not have
/// to move the pointer at the same time.
#[must_use]
pub fn pointer_path(root: &Path) -> PathBuf {
    root.join(".storyhook.toml")
}

/// Reads a checkout's pointer file, if it has one.
///
/// Every failure names the file. That is not politeness: `.storyhook.toml` is
/// **committed to the repository and hand-authored** — it carries the user's
/// `[plugin]` and `[hooks]` tables beside the project's identity — so a syntax
/// error in it is an ordinary mistake made in an ordinary editor. Left to
/// `toml`'s own words, `story list` reports `TOML parse error at line 1, column
/// 6` and the user has no file to open. Resolution runs this on almost every
/// command, so the unattributed version could surface anywhere.
pub fn read_pointer(root: &Path) -> Result<Option<ProjectPointer>, AppError> {
    let path = pointer_path(root);
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| {
        AppError::Storage(format!(
            "{} could not be read: {e}. This file names the project this checkout \
             belongs to.",
            path.display()
        ))
    })?;
    let pointer = toml::from_str(&raw).map_err(|e: toml::de::Error| {
        AppError::Storage(format!(
            "{} is not valid: {e}This file names the project this checkout belongs to; \
             it is committed, so `git diff` on it is the fastest way to see what changed.",
            path.display()
        ))
    })?;
    Ok(Some(pointer))
}

/// Writes a checkout's pointer file, replacing any existing one.
pub fn write_pointer(root: &Path, pointer: &ProjectPointer) -> Result<(), AppError> {
    std::fs::write(pointer_path(root), toml::to_string_pretty(pointer)?)?;
    Ok(())
}

/// What `story project init` was asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InitOptions {
    /// The story-id prefix, defaulting to [`DEFAULT_PREFIX`].
    pub prefix: Option<String>,
    /// The project's display name, defaulting to the directory's basename.
    ///
    /// Unlike [`prefix`](Self::prefix) this is applied on **every** init, not
    /// only the one that creates the project: renaming is a reversible display
    /// change, whereas a prefix is baked into every story id ever minted, so
    /// re-initializing must leave one alone and may honour the other.
    pub name: Option<String>,
    /// Generate `AGENTS.md` when the repository does not already have one.
    pub agents_md: bool,
    /// Write the committed [pointer file](ProjectPointer).
    ///
    /// Off by default and left off by the dispatcher: until the store becomes
    /// the identity of record, a pointer file in a repository that is still
    /// tracked by `.storyhook/` would be a second answer to "which project is
    /// this", and the two would disagree the moment either moved.
    pub pointer: bool,
}

impl Default for InitOptions {
    fn default() -> Self {
        Self {
            prefix: None,
            name: None,
            agents_md: true,
            pointer: false,
        }
    }
}

/// What `story project init` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InitOutcome {
    /// The project this checkout now belongs to.
    pub project: ProjectId,
    /// Whether the project was created, as opposed to already existing.
    ///
    /// `story project init` is idempotent — running it twice re-registers the checkout
    /// and reports success — so this distinguishes the two without changing
    /// what the user is told.
    pub created: bool,
    /// Whether `AGENTS.md` was generated by this run.
    pub agents_md: bool,
    /// Whether the pointer file was written by this run.
    pub pointer: bool,
}

/// The name of the agent-instruction file `init` generates.
pub const AGENTS_MD: &str = "AGENTS.md";

/// What a deinit was asked to destroy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeinitTarget {
    /// A checkout directory.
    Path(PathBuf),
    /// A project slug, for one this machine has no checkout of.
    Slug(String),
}

/// What a [`ProjectService::deinit`] destroyed.
#[derive(Clone, Debug)]
pub struct DeinitOutcome {
    /// The plan it carried out, with [`files`](DeinitPlan::files) narrowed to
    /// the ones that were actually there to remove.
    pub plan: DeinitPlan,
    /// What the store deleted, counted inside the deleting transaction.
    pub removed: DeletedProject,
}

/// Whether `AGENTS.md` is byte-for-byte what this build would generate.
///
/// The deletion predicate, and deliberately the strictest one available.
/// `init` refuses to overwrite an existing `AGENTS.md` because it may be the
/// user's; deleting one it *did* generate but the user has since edited would
/// destroy exactly what that care was protecting.
///
/// Being strict has a visible cost and it is the right cost: when the template
/// changes, every file generated by an older build stops matching and is kept
/// instead of removed. A leftover file is a tidiness complaint. The failure in
/// the other direction — a fuzzy match that decides an edited file is "close
/// enough" — destroys work. Do not loosen this.
#[must_use]
pub fn agents_md_is_pristine(root: &Path, prefix: &str, done_state: &str) -> bool {
    let Ok(found) = std::fs::read_to_string(root.join(AGENTS_MD)) else {
        return false;
    };
    found == templates::agents_md(prefix, done_state)
}

/// Creating projects and registering the checkouts that belong to them.
///
/// Unlike every other service this one does *not* take a
/// [`Ctx`](super::Ctx): a context names the project it operates on, and
/// `init`'s whole job is that there is not one yet.
pub struct ProjectService<'a, S: Store> {
    store: &'a S,
    root: PathBuf,
    clock: Clock,
}

impl<'a, S: Store> ProjectService<'a, S> {
    /// A project service for the checkout at `root`.
    pub fn new(store: &'a S, root: impl Into<PathBuf>) -> Self {
        Self {
            store,
            root: root.into(),
            clock: Clock::System,
        }
    }

    /// Sets the clock this service's timestamps come from.
    #[must_use]
    pub fn clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// Initialises a project for this checkout.
    ///
    /// The project row, its default states, its default types and its story-id
    /// counter are written in one transaction. The repository-side artifacts
    /// are written afterwards, deliberately: they are content, not identity,
    /// and a failure to write one must not undo a project that already exists
    /// in the store.
    ///
    /// Idempotent. A checkout that already belongs to a project has its
    /// registration refreshed and its `prefix` left alone — the same shape as
    /// the legacy path, which skipped every file it found already present.
    ///
    /// "Already belongs" is asked of the committed pointer file *before* the
    /// path, matching every other resolution in the codebase. It matters on a
    /// fresh clone: the checkout is at a path the store has never seen, but its
    /// pointer names a project the store may well know — and answering by path
    /// alone would mint a second project for a repository that already has one,
    /// leaving the clone pointing at the old identity and storing into the new.
    pub fn init(&self, options: &InitOptions) -> Result<InitOutcome, AppError> {
        let root = canonical(&self.root);
        let now = self.clock.now();
        let existing_pointer = read_pointer(&root)?;

        // A repository whose stories are still in `.storyhook/` is not an
        // uninitialized one, and treating it as such is how a user ends up with
        // an empty project beside their real data. `story migrate` is the
        // command for it, and this says so before anything is written.
        if existing_pointer.is_none()
            && self.store.read(|tx| tx.project_by_path(&root))?.is_none()
            && root.join(".storyhook/project.toml").is_file()
        {
            return Err(unmigrated_error(&root));
        }

        let (project, created, uuid, prefix) = self.store.write(|tx| {
            if let Some(pointer) = &existing_pointer
                && let Some(existing) = tx.project_by_uuid(&pointer.uuid)?
            {
                tx.touch_project_path(existing.id, &root, path_kind(&root))?;
                if let Some(name) = &options.name {
                    tx.rename_project(existing.id, name)?;
                }
                return Ok((existing.id, false, existing.uuid, existing.prefix));
            }
            if let Some(existing) = tx.project_by_path(&root)? {
                tx.touch_project_path(existing.id, &root, path_kind(&root))?;
                if let Some(name) = &options.name {
                    tx.rename_project(existing.id, name)?;
                }
                return Ok((existing.id, false, existing.uuid, existing.prefix));
            }

            // A checkout that already carries a pointer is a *clone*, not a new
            // repository, and the identity it names is the one to create. The
            // uuid exists precisely so a project survives being copied to
            // another machine; minting a fresh one here would leave the
            // committed file naming a project that exists nowhere, and the
            // repository resolving by path alone from then on — so moving the
            // checkout, or cloning it again, would stop resolving entirely.
            //
            // The prefix comes with it for the same reason. A clone whose
            // history is full of `ZZ-*` ids must not get a project that mints
            // `SH-1`; that is a second tracker wearing the first one's name.
            // `--prefix` is therefore ignored here, which matches the rule that
            // `init` on a project that already exists leaves its prefix alone.
            let (uuid, prefix) = match &existing_pointer {
                Some(pointer) => (pointer.uuid.clone(), pointer.prefix.clone()),
                None => (
                    uuid::Uuid::new_v4().to_string(),
                    options
                        .prefix
                        .clone()
                        .unwrap_or_else(|| DEFAULT_PREFIX.to_string()),
                ),
            };
            let name = options.name.clone().unwrap_or_else(|| display_name(&root));
            let project = tx.create_project(&NewProject {
                uuid: uuid.clone(),
                slug: unique_slug(&*tx, &name)?,
                name,
                prefix: prefix.clone(),
                created_at: now.clone(),
            })?;
            tx.touch_project_path(project, &root, path_kind(&root))?;
            tx.put_states(project, &default_states())?;
            tx.put_types(project, &default_types())?;
            Ok((project, true, uuid, prefix))
        })?;

        // Never overwritten. The file is user-authored the moment it carries a
        // `[plugin]` or `[hooks]` table, and `story project init` is idempotent — so a
        // second `init` in a repository that already has a pointer must leave
        // the user's configuration exactly where it is rather than replacing
        // the file with a freshly generated identity-only copy.
        let pointer = options.pointer && existing_pointer.is_none();
        if pointer {
            write_pointer(&root, &ProjectPointer::new(uuid, prefix.clone()))?;
        }

        let done_state = self.store.read(|tx| Ok(closed_state(tx, project)?))?;
        let agents_md = options.agents_md && self.write_agents_md(&root, &prefix, &done_state)?;

        Ok(InitOutcome {
            project,
            created,
            agents_md,
            pointer,
        })
    }

    /// Everything a [`deinit`](Self::deinit) would destroy. Writes nothing.
    ///
    /// Separated from the deletion so that the answer can be shown to somebody
    /// before it happens, and so that the CLI's prompt and the dashboard's
    /// modal are looking at the same value rather than each computing their
    /// own.
    pub fn deinit_plan(&self, target: &DeinitTarget) -> Result<DeinitPlan, AppError> {
        let record = self.resolve_deinit_target(target)?;
        let (stories, events, checkouts) = self.store.read(|tx| {
            Ok((
                tx.stories(record.id, &crate::store::StoryQuery::all())?
                    .len(),
                tx.event_count(record.id)?,
                tx.project_paths(record.id)?
                    .into_iter()
                    .map(|row| row.path)
                    .collect::<Vec<_>>(),
            ))
        })?;

        // Repository files are only ever considered in a checkout that is
        // actually there. Deinitializing a project by slug, from somewhere
        // else, must not go looking for files in a directory it was not given.
        let mut files = Vec::new();
        let mut kept = Vec::new();
        if let Some(root) = self.repository_root(target, &checkouts) {
            if pointer_path(&root).is_file() {
                files.push(pointer_path(&root).display().to_string());
            }
            let agents = root.join(AGENTS_MD);
            if agents.is_file() {
                let done = self.store.read(|tx| Ok(closed_state(tx, record.id)?))?;
                if agents_md_is_pristine(&root, &record.prefix, &done) {
                    files.push(agents.display().to_string());
                } else {
                    kept.push((
                        agents.display().to_string(),
                        "edited since it was generated".to_string(),
                    ));
                }
            }
        }

        Ok(DeinitPlan {
            slug: record.slug,
            name: record.name,
            prefix: record.prefix,
            stories,
            events,
            checkouts,
            files,
            kept,
        })
    }

    /// Destroys the project `target` names, and the repository files `init`
    /// generated for it.
    ///
    /// Graceful about partial states, because they are reachable with `rm`: a
    /// pointer file already deleted by hand is a step with nothing to do, not a
    /// failure. What is *not* tolerated is a target that names no project —
    /// the caller has just told a user what it was about to destroy.
    ///
    /// The store transaction commits before any file is removed. That ordering
    /// is the recoverable one: a crash between them leaves a checkout whose
    /// pointer names a project that no longer exists, which resolution already
    /// reports clearly, rather than a repository stripped of its identity while
    /// its stories are still live.
    pub fn deinit(&self, target: &DeinitTarget) -> Result<DeinitOutcome, AppError> {
        let plan = self.deinit_plan(target)?;
        let record = self.resolve_deinit_target(target)?;
        let removed = self.store.write(|tx| tx.delete_project(record.id))?;

        let mut removed_files = Vec::new();
        for file in &plan.files {
            match std::fs::remove_file(file) {
                Ok(()) => removed_files.push(file.clone()),
                // Already gone is the outcome this wanted.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(AppError::Storage(format!(
                        "the project was deleted, but `{file}` could not be removed: {e}"
                    )));
                }
            }
        }

        Ok(DeinitOutcome {
            plan: DeinitPlan {
                files: removed_files,
                ..plan
            },
            removed,
        })
    }

    /// The project a deinit target names, by path or by slug.
    ///
    /// Path first, because that is what an unqualified `story project deinit`
    /// means and what a user standing in a repository expects. A slug is
    /// accepted because a project whose checkout is gone cannot be named by
    /// path at all — and that is exactly the project most worth deleting.
    fn resolve_deinit_target(&self, target: &DeinitTarget) -> Result<ProjectRecord, AppError> {
        match target {
            DeinitTarget::Path(path) => {
                let root = canonical(path);
                let pointer = read_pointer(&root)?;
                let found = self.store.read(|tx| {
                    if let Some(pointer) = &pointer
                        && let Some(project) = tx.project_by_uuid(&pointer.uuid)?
                    {
                        return Ok(Some(project));
                    }
                    tx.project_by_path(&root)
                })?;
                found.ok_or_else(|| {
                    AppError::NotFound(format!(
                        "`{}` is not a storyhook project.\n\nIf you meant a project whose \
                         checkout is gone, name it by slug — `story project list` prints them.",
                        root.display()
                    ))
                })
            }
            DeinitTarget::Slug(slug) => self
                .store
                .read(|tx| tx.project_by_slug(slug))?
                .ok_or_else(|| AppError::NotFound(format!("no project `{slug}`"))),
        }
    }

    /// The checkout whose files a deinit may remove, if there is one.
    ///
    /// Only a directory the caller actually named: deinitializing by slug from
    /// an unrelated directory must not reach into a recorded checkout and
    /// delete files there. Naming a path is the act that authorizes touching
    /// that path.
    fn repository_root(&self, target: &DeinitTarget, checkouts: &[String]) -> Option<PathBuf> {
        match target {
            DeinitTarget::Path(path) => Some(canonical(path)),
            DeinitTarget::Slug(_) => {
                // A slug names no directory. The one exception that is still
                // unambiguous: the service's own root is one of this project's
                // recorded checkouts, so the user is standing in it.
                let root = canonical(&self.root);
                checkouts
                    .iter()
                    .any(|c| Path::new(c) == root)
                    .then_some(root)
            }
        }
    }

    /// Writes `AGENTS.md`, reporting whether it did.
    ///
    /// An existing file is never overwritten: it is repository content the
    /// user may have edited, and `init` is idempotent.
    fn write_agents_md(
        &self,
        root: &Path,
        prefix: &str,
        done_state: &str,
    ) -> Result<bool, AppError> {
        let path = root.join(AGENTS_MD);
        if path.exists() {
            return Ok(false);
        }
        std::fs::write(&path, templates::agents_md(prefix, done_state))?;
        Ok(true)
    }
}

/// The state set a new project starts with.
///
/// The definition, not a copy of one: the legacy path builds the same three
/// states inline in `storage::init_project`, and the differential harness
/// compares the two catalogs so they cannot drift apart unnoticed.
#[must_use]
pub fn default_states() -> Vec<StateDef> {
    vec![
        StateDef {
            slug: "todo".to_string(),
            super_state: SuperState::Open,
            role: None,
            description: None,
        },
        StateDef {
            slug: "in-progress".to_string(),
            super_state: SuperState::Open,
            role: Some("active".to_string()),
            description: None,
        },
        StateDef {
            slug: "done".to_string(),
            super_state: SuperState::Closed,
            role: None,
            description: None,
        },
    ]
}

/// The type set a new project starts with. See [`default_states`].
#[must_use]
pub fn default_types() -> Vec<TypeDef> {
    [
        ("story", "A user story or feature"),
        ("epic", "A large initiative containing child stories"),
        ("bug", "A defect or regression"),
        ("chore", "Maintenance or infrastructure work"),
        ("task", "A discrete unit of work"),
    ]
    .into_iter()
    .map(|(slug, description)| TypeDef {
        slug: slug.to_string(),
        description: Some(description.to_string()),
    })
    .collect()
}

/// The project's first CLOSED state, for the templates that name "done".
///
/// Falls back to the literal `done` for a project that has none — a template
/// is documentation, and documentation that fails to render is worse than
/// documentation naming a state the reader can correct.
pub fn closed_state(tx: &impl ReadOps, project: ProjectId) -> Result<String, AppError> {
    Ok(tx
        .states(project)?
        .into_iter()
        .find(|state| state.super_state == SuperState::Closed)
        .map_or_else(|| "done".to_string(), |state| state.slug))
}

/// Whether this checkout is a main working tree or a linked worktree.
///
/// In a linked worktree `.git` is a *file* pointing at the real git directory,
/// not a directory of its own. Recording which kind a checkout is costs
/// nothing here and is what lets a later wave answer "these two directories
/// are one repository" — the question whose wrong answer minted the colliding
/// story ids this rearchitecture exists to fix.
pub(crate) fn path_kind(root: &Path) -> PathKind {
    if root.join(".git").is_file() {
        PathKind::Worktree
    } else {
        PathKind::Main
    }
}

/// A checkout's canonical path, falling back to the path as given.
///
/// The store matches checkouts by the path it was handed, so the caller has to
/// decide what canonical means. A directory that cannot be canonicalized is
/// one that does not exist yet — `init` on a path the user is about to create
/// — and passing it through unchanged is better than failing on it here, where
/// the eventual filesystem error would be far more specific.
fn canonical(root: &Path) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

/// The display name for a checkout: its directory's basename.
fn display_name(root: &Path) -> String {
    root.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".to_string())
}

/// A slug for `name` that no project in the store is using yet.
///
/// Collisions are expected — every `app` directory on a machine slugs to
/// `app` — and are resolved the way the registry has always resolved them, by
/// appending the first free numeric suffix.
pub(crate) fn unique_slug(tx: &impl ReadOps, name: &str) -> Result<String, AppError> {
    let base = slugify(name);
    if tx.project_by_slug(&base)?.is_none() {
        return Ok(base);
    }
    let mut n = 2;
    loop {
        let candidate = format!("{base}-{n}");
        if tx.project_by_slug(&candidate)?.is_none() {
            return Ok(candidate);
        }
        n += 1;
    }
}

/// Lowercases `value` and collapses every run of non-alphanumerics into a
/// single dash, falling back to `project` when nothing survives.
fn slugify(value: &str) -> String {
    let mut slug = String::with_capacity(value.len());
    let mut last_dash = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "project".to_string()
    } else {
        slug
    }
}

#[cfg(test)]
mod temp_project_tests {
    use super::*;
    use std::path::Path;

    /// The case that put 394 projects in a real store: a fixture directory
    /// under `$TMPDIR`, a store in the user's data home.
    #[test]
    fn a_temp_project_in_a_real_store_is_refused() {
        let root = std::env::temp_dir().join("tmp.abc123");
        let data_home = Path::new("/Users/someone/.local/share/storyhook");
        let error = refuse_temp_project(&root, data_home, false)
            .expect_err("a temp project in a real store must be refused");
        assert_eq!(error.exit_code(), 2, "it is a usage error, not a crash");
        let message = error.to_string();
        assert!(
            message.contains("STORYHOOK_DATA_DIR"),
            "the message must name the variable that fixes the caller: {message}"
        );
        assert!(
            message.contains(ALLOW_TEMP_PROJECT),
            "the message must name the override: {message}"
        );
    }

    /// What this project's own suite does, and what any correctly isolated
    /// suite does. Refusing it would be the fix breaking its own users.
    #[test]
    fn a_temp_project_in_a_temp_store_is_allowed() {
        let root = Path::new("/private/tmp/storyhook-tests/fixture-1");
        let data_home = Path::new("/private/tmp/storyhook-gate.XXXX/data");
        assert!(
            refuse_temp_project(root, data_home, false).is_ok(),
            "a throwaway project in a throwaway store is exactly what a test builds"
        );
    }

    /// The ordinary case: a real repository, a real store.
    #[test]
    fn a_real_project_in_a_real_store_is_allowed() {
        let root = Path::new("/Volumes/Code/mikeyward/storyhook");
        let data_home = Path::new("/Users/someone/.local/share/storyhook");
        assert!(refuse_temp_project(root, data_home, false).is_ok());
    }

    /// A deliberate override is honoured, so the refusal can never become a
    /// wall someone has no way past.
    #[test]
    fn the_override_permits_what_would_otherwise_be_refused() {
        let root = std::env::temp_dir().join("tmp.abc123");
        let data_home = Path::new("/Users/someone/.local/share/storyhook");
        assert!(refuse_temp_project(&root, data_home, true).is_ok());
    }

    /// `/tmp` and `/private/tmp` are the same directory on macOS reached by two
    /// names. Without canonicalization one of them is not recognised as
    /// temporary, and half the cases walk straight through the guard.
    #[test]
    fn both_spellings_of_the_macos_temp_directory_are_recognised() {
        for spelling in ["/tmp/fixture", "/private/tmp/fixture"] {
            assert!(
                is_under_temp(Path::new(spelling)),
                "`{spelling}` must be recognised as temporary"
            );
        }
        assert!(
            !is_under_temp(Path::new("/Volumes/Code/mikeyward/storyhook")),
            "a real checkout must not be mistaken for a temporary directory"
        );
    }
}
