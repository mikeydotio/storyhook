//! Bringing a project into existence.
//!
//! `story init` used to be a sequence of independent filesystem writes: make
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
use crate::store::{NewProject, PathKind, ProjectId, ReadOps, Store, WriteOps};

use super::Clock;
use super::templates;

/// The story-id prefix a project gets when `init` is not told one.
pub const DEFAULT_PREFIX: &str = "SH";

/// The repository-side file naming the project a checkout belongs to.
///
/// This is the *whole* repo footprint of the new design: one small committed
/// file holding the project's portable identity and its story-id prefix, so
/// that a fresh clone on another machine knows which project it is looking at
/// before it has any local database row to consult.
///
/// It deliberately does not carry states, types, members or stories. Those
/// live in the store; a repository that carried its own copy would be a second
/// source of truth, which is the thing this whole rearchitecture exists to
/// delete.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectPointer {
    /// Format version, so a later shape change is detectable rather than
    /// silently misread.
    pub schema: u32,
    /// The project's portable identity — [`crate::store::ProjectRecord::uuid`].
    pub uuid: String,
    /// The project's story-id prefix, duplicated here so a clone can render
    /// ids before it can reach the store.
    pub prefix: String,
}

/// The pointer format this build writes.
const POINTER_SCHEMA: u32 = 1;

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
pub fn read_pointer(root: &Path) -> Result<Option<ProjectPointer>, AppError> {
    let path = pointer_path(root);
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)?;
    Ok(Some(toml::from_str(&raw)?))
}

/// Writes a checkout's pointer file, replacing any existing one.
pub fn write_pointer(root: &Path, pointer: &ProjectPointer) -> Result<(), AppError> {
    std::fs::write(pointer_path(root), toml::to_string_pretty(pointer)?)?;
    Ok(())
}

/// What `story init` was asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InitOptions {
    /// The story-id prefix, defaulting to [`DEFAULT_PREFIX`].
    pub prefix: Option<String>,
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
            agents_md: true,
            pointer: false,
        }
    }
}

/// What `story init` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InitOutcome {
    /// The project this checkout now belongs to.
    pub project: ProjectId,
    /// Whether the project was created, as opposed to already existing.
    ///
    /// `story init` is idempotent — running it twice re-registers the checkout
    /// and reports success — so this distinguishes the two without changing
    /// what the user is told.
    pub created: bool,
    /// Whether `AGENTS.md` was generated by this run.
    pub agents_md: bool,
    /// Whether the pointer file was written by this run.
    pub pointer: bool,
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
    pub fn init(&self, options: &InitOptions) -> Result<InitOutcome, AppError> {
        let root = canonical(&self.root);
        let now = self.clock.now();

        let (project, created, uuid, prefix) = self.store.write(|tx| {
            if let Some(existing) = tx.project_by_path(&root)? {
                tx.touch_project_path(existing.id, &root, path_kind(&root))?;
                return Ok((existing.id, false, existing.uuid, existing.prefix));
            }

            let prefix = options
                .prefix
                .clone()
                .unwrap_or_else(|| DEFAULT_PREFIX.to_string());
            let uuid = uuid::Uuid::new_v4().to_string();
            let name = display_name(&root);
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

        let pointer = options.pointer;
        if pointer {
            write_pointer(
                &root,
                &ProjectPointer {
                    schema: POINTER_SCHEMA,
                    uuid,
                    prefix: prefix.clone(),
                },
            )?;
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
        let path = root.join("AGENTS.md");
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
