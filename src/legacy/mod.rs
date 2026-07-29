//! A permanent, read-only reader for the legacy `.storyhook` on-disk format.
//!
//! # Why this is not `src/storage.rs`
//!
//! `storage.rs` reads *and writes* the same files, and the flip deletes its
//! write half along with most of the rest of it. This module has to outlive
//! that: as long as a `.storyhook` directory can exist anywhere — in a repo
//! nobody has migrated, in a branch someone checks out next year, in a tarball
//! attached to a bug report — storyhook must be able to read it. So the two
//! share no types, no helpers and no file handles, and the coupling between
//! them is zero by construction rather than by convention.
//!
//! The duplication is deliberate and bounded. Everything here is *reading*, and
//! the format is frozen: nothing will ever add a field to `project.toml` again.
//!
//! # It never writes. Ever.
//!
//! Not "tries not to" — cannot. There is no `fs::write`, no `File::create`, no
//! `OpenOptions`, no `create_dir`, no `remove_*` anywhere under `src/legacy/`,
//! and `tests/legacy_reader.rs::the_reader_contains_no_write_calls` reads this
//! directory's own source to keep it that way. The archive database is opened
//! through SQLite's `immutable=1`, which takes no locks and creates neither the
//! `-shm` nor the `-wal` file an ordinary read-only connection would.
//! `import_does_not_mutate_the_legacy_tree` hashes a whole tree before and
//! after a migration and requires the two to be identical.
//!
//! The reason is not tidiness. A migration that half-writes the thing it is
//! reading has no rollback: the operator's undo button is *the tree they
//! started with*, and it has to still be there afterwards.
//!
//! # Unknown events survive
//!
//! An event kind this binary has never heard of is kept verbatim rather than
//! dropped or refused — see [`LegacyEvent`]. A *known* kind that will not
//! decode is a hard error, because the two are indistinguishable to serde and
//! only one of them is safe to wave through.

mod events;
mod paths;

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::domain::{Member, StateDef, TypeDef};
use crate::error::AppError;

pub use events::LegacyEvent;
pub use paths::{LEGACY_DIR, LegacyPaths};

/// What went wrong reading a legacy tree.
///
/// Every variant names a path, because every one of them is something an
/// operator has to go and look at.
#[derive(Debug, thiserror::Error)]
pub enum LegacyError {
    /// There is no legacy project at this root.
    #[error("no legacy `.storyhook` project at `{}`", .0.display())]
    NotAProject(PathBuf),
    /// A file exists but could not be read.
    #[error("cannot read `{}`: {detail}", .path.display())]
    Unreadable {
        /// The file.
        path: PathBuf,
        /// The underlying failure.
        detail: String,
    },
    /// A file was read but does not hold what the format says it should.
    #[error("`{}` is corrupt: {detail}", .path.display())]
    Malformed {
        /// The file.
        path: PathBuf,
        /// What was wrong with it.
        detail: String,
    },
}

impl From<LegacyError> for AppError {
    /// A missing project is `NotFound` (exit 3); a broken one is `Storage`
    /// (exit 5). The same split `storage::ensure_project` and its readers made,
    /// so a migration's exit codes say what every other verb's say.
    fn from(error: LegacyError) -> Self {
        match error {
            LegacyError::NotAProject(_) => Self::NotFound(error.to_string()),
            LegacyError::Unreadable { .. } | LegacyError::Malformed { .. } => {
                Self::Storage(error.to_string())
            }
        }
    }
}

/// One story's whole legacy history.
#[derive(Clone, Debug)]
pub struct LegacyStory {
    /// The story id as the tree spells it, prefix included.
    pub id: String,
    /// Every event, oldest first, unknown kinds retained.
    pub events: Vec<LegacyEvent>,
    /// Whether it lives in the archive database rather than as an open log.
    pub archived: bool,
    /// The file it came from, for error messages.
    pub source: PathBuf,
}

/// Everything one legacy `.storyhook` tree holds.
///
/// Deliberately wider than [`crate::storage::ProjectExport`]: an export
/// document carries states, types, members and stories, and a *tree* also
/// carries the project's creation time, its settings tables and its story-number
/// counter. A migration that went through the export document alone would drop
/// those three without saying so.
#[derive(Clone, Debug)]
pub struct LegacyProject {
    /// The checkout the tree was read from.
    pub root: PathBuf,
    /// `project.toml`'s `schema`.
    pub schema: u32,
    /// When `story init` created it.
    pub created_at: String,
    /// The configured story-id prefix, absent when the default was left in
    /// place. Kept as an option because `story export` emits it as one.
    pub prefix: Option<String>,
    /// `sync.auto_transition`.
    pub sync_auto_transition: Option<bool>,
    /// `doctor.stale_threshold`.
    pub doctor_stale_threshold: Option<String>,
    /// The configured states, in order.
    pub states: Vec<StateDef>,
    /// The configured story types, in order.
    pub types: Vec<TypeDef>,
    /// The project's members.
    pub members: Vec<Member>,
    /// The `next-id` counter — the number the next `story new` would have used.
    pub next_id: u64,
    /// Every story: open ones first in filename order, then archived ones in id
    /// order, which is the order `storage::export_project` produced and the
    /// order `golden-export.json` froze.
    pub stories: Vec<LegacyStory>,
}

impl LegacyProject {
    /// The effective prefix — [`prefix`](Self::prefix) or the default.
    #[must_use]
    pub fn effective_prefix(&self) -> &str {
        self.prefix
            .as_deref()
            .unwrap_or(crate::service::project::DEFAULT_PREFIX)
    }

    /// Every event the reader could not decode, with the story it belongs to.
    pub fn unknown_events(&self) -> impl Iterator<Item = (&LegacyStory, &LegacyEvent)> {
        self.stories
            .iter()
            .flat_map(|story| story.events.iter().map(move |event| (story, event)))
            .filter(|(_, event)| event.is_unknown())
    }
}

/// The nearest ancestor of `from` (inclusive) holding a legacy project.
///
/// An ancestor walk, where `storage::ensure_project` looked only at the working
/// directory. That asymmetry is on purpose and it is confined to this one
/// function: `story migrate` is a thing an operator runs once, from wherever
/// they happen to be standing in the repository, and refusing because they were
/// two directories deep would be a worse answer than finding it. Every *other*
/// verb still resolves a project the way it always has.
#[must_use]
pub fn find_root(from: &Path) -> Option<PathBuf> {
    let start = from.canonicalize().unwrap_or_else(|_| from.to_path_buf());
    start
        .ancestors()
        .find(|dir| LegacyPaths::new(dir).exists())
        .map(Path::to_path_buf)
}

/// Reads the whole legacy tree at `root`.
///
/// Refuses rather than guesses. A story log with a corrupt line, an archive
/// database with an uncheckpointed write-ahead log, a `next-id` that is not a
/// number — each is an error naming the file, because the caller's next move is
/// to write all of this into a store and keep it forever.
pub fn read_project(root: &Path) -> Result<LegacyProject, LegacyError> {
    let paths = LegacyPaths::new(root);
    if !paths.exists() {
        return Err(LegacyError::NotAProject(root.to_path_buf()));
    }

    let project = read_project_file(&paths)?;
    let states = read_states(&paths)?;
    let types = read_types(&paths)?;
    let members = read_members(&paths)?;
    let next_id = read_next_id(&paths)?;

    let mut stories = read_open_stories(&paths)?;
    stories.extend(read_archived_stories(&paths)?);

    let (sync, doctor) = (
        project.sync.unwrap_or_default(),
        project.doctor.unwrap_or_default(),
    );
    Ok(LegacyProject {
        root: root.to_path_buf(),
        schema: project.schema,
        created_at: project.created_at,
        prefix: project.prefix,
        sync_auto_transition: sync.auto_transition,
        doctor_stale_threshold: doctor.stale_threshold,
        states,
        types,
        members,
        next_id,
        stories,
    })
}

/// `project.toml`, as its own type rather than `storage`'s.
#[derive(Deserialize)]
struct ProjectFile {
    schema: u32,
    created_at: String,
    #[serde(default)]
    prefix: Option<String>,
    #[serde(default)]
    sync: Option<SyncTable>,
    #[serde(default)]
    doctor: Option<DoctorTable>,
}

#[derive(Default, Deserialize)]
struct SyncTable {
    #[serde(default)]
    auto_transition: Option<bool>,
}

#[derive(Default, Deserialize)]
struct DoctorTable {
    #[serde(default)]
    stale_threshold: Option<String>,
}

#[derive(Deserialize)]
struct StatesFile {
    states: Vec<StateDef>,
}

#[derive(Deserialize)]
struct TypesFile {
    types: Vec<TypeDef>,
}

fn read_project_file(paths: &LegacyPaths) -> Result<ProjectFile, LegacyError> {
    let path = paths.project_file();
    let text = read_to_string(&path)?;
    toml::from_str(&text).map_err(|error| LegacyError::Malformed {
        path,
        detail: error.to_string(),
    })
}

/// The configured states, exactly as written.
///
/// Deliberately *not* validated here. `storage::load_states` ran
/// `validate_state_defs` on every read, so a tree that fails it is one the
/// legacy tool could not open either — but this reader's job is to report what
/// is on disk, and the migration is a better place to say "your states.toml is
/// invalid, here is why" than a reader that can only say it about one file.
fn read_states(paths: &LegacyPaths) -> Result<Vec<StateDef>, LegacyError> {
    let path = paths.states_file();
    let text = read_to_string(&path)?;
    Ok(toml::from_str::<StatesFile>(&text)
        .map_err(|error| LegacyError::Malformed {
            path,
            detail: error.to_string(),
        })?
        .states)
}

/// The configured story types.
///
/// A missing `types.toml` is not an error: `ensure_types_file` wrote one lazily,
/// so a project created before types existed simply has none.
fn read_types(paths: &LegacyPaths) -> Result<Vec<TypeDef>, LegacyError> {
    let path = paths.types_file();
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let text = read_to_string(&path)?;
    Ok(toml::from_str::<TypesFile>(&text)
        .map_err(|error| LegacyError::Malformed {
            path,
            detail: error.to_string(),
        })?
        .types)
}

/// The project's members. A missing file means none, matching `init`'s empty
/// one.
fn read_members(paths: &LegacyPaths) -> Result<Vec<Member>, LegacyError> {
    let path = paths.members_file();
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let text = read_to_string(&path)?;
    let mut members = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        members.push(
            serde_json::from_str(line).map_err(|error| LegacyError::Malformed {
                path: path.clone(),
                detail: format!("member {}: {error}", index + 1),
            })?,
        );
    }
    Ok(members)
}

/// The `next-id` counter. A missing file means the project never minted a
/// story, which `init` wrote as `1`.
fn read_next_id(paths: &LegacyPaths) -> Result<u64, LegacyError> {
    let path = paths.next_id_file();
    if !path.is_file() {
        return Ok(1);
    }
    let text = read_to_string(&path)?;
    text.trim()
        .parse::<u64>()
        .map_err(|error| LegacyError::Malformed {
            path,
            detail: format!("not a story-number counter: {error}"),
        })
}

/// Open stories, in filename order — the order `export_project` used.
fn read_open_stories(paths: &LegacyPaths) -> Result<Vec<LegacyStory>, LegacyError> {
    let dir = paths.open_stories_dir();
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|error| LegacyError::Unreadable {
            path: dir.clone(),
            detail: error.to_string(),
        })?
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(|error| LegacyError::Unreadable {
                    path: dir.clone(),
                    detail: error.to_string(),
                })
        })
        .collect::<Result<_, _>>()?;
    entries.sort();

    let mut stories = Vec::new();
    for path in entries {
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        let id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| LegacyError::Malformed {
                path: path.clone(),
                detail: "story filename is not usable as an id".to_string(),
            })?
            .to_string();
        stories.push(LegacyStory {
            events: events::read_jsonl_log(&path)?,
            id,
            archived: false,
            source: path,
        });
    }
    Ok(stories)
}

/// Archived stories, in id order — the archive's own `ORDER BY id ASC`.
///
/// Only `events_json` is read. The `snapshot_json` column beside it is a cache
/// of a fold, and `repair_archived_snapshots` exists precisely because it goes
/// stale; the events are the source of truth and the migration re-folds them.
fn read_archived_stories(paths: &LegacyPaths) -> Result<Vec<LegacyStory>, LegacyError> {
    let db = paths.archive_db();
    if !db.is_file() {
        return Ok(Vec::new());
    }
    refuse_uncheckpointed_wal(paths)?;

    let unreadable = |error: rusqlite::Error| LegacyError::Unreadable {
        path: db.clone(),
        detail: error.to_string(),
    };

    // `immutable=1` promises SQLite the file cannot change underneath it, which
    // is how a read takes no locks and creates no `-shm` sidecar. It is a
    // *promise*, and the WAL check above is what makes it true: an immutable
    // open ignores a `-wal` file entirely, so an unchecked one would be read
    // straight past.
    let uri = format!("file:{}?immutable=1", db.display());
    let connection = rusqlite::Connection::open_with_flags(
        &uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(unreadable)?;

    let mut statement = connection
        .prepare("SELECT id, events_json FROM closed_stories ORDER BY id ASC")
        .map_err(unreadable)?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(unreadable)?;

    let mut stories = Vec::new();
    for row in rows {
        let (id, events_json) = row.map_err(unreadable)?;
        stories.push(LegacyStory {
            events: events::read_event_array(&db, &events_json)?,
            id,
            archived: true,
            source: db.clone(),
        });
    }
    Ok(stories)
}

/// Refuses an archive database whose write-ahead log still holds data.
///
/// SQLite checkpoints the WAL when the last connection closes, so a non-empty
/// `-wal` file means something is running — a `story web` daemon, a TUI, an
/// editor — or that something was killed mid-write. Either way an
/// `immutable=1` read would report the database as it stood *before* those
/// writes and say nothing about it, which is the exact failure mode this whole
/// module is written to make impossible.
fn refuse_uncheckpointed_wal(paths: &LegacyPaths) -> Result<(), LegacyError> {
    let wal = paths.archive_wal();
    let size = match std::fs::metadata(&wal) {
        Ok(metadata) => metadata.len(),
        Err(_) => return Ok(()),
    };
    if size == 0 {
        return Ok(());
    }
    Err(LegacyError::Malformed {
        path: wal,
        detail: format!(
            "{size} bytes of un-checkpointed writes sit beside the archive database. Close every \
             process using this project (`story web stop`, any open `story tui`) so SQLite \
             checkpoints them, then run this again — reading past them would silently import a \
             stale archive"
        ),
    })
}

/// `std::fs::read_to_string`, with the path in the error.
///
/// The only file-opening call in this module, so "the reader never writes" is a
/// property of four lines rather than of every call site.
fn read_to_string(path: &Path) -> Result<String, LegacyError> {
    std::fs::read_to_string(path).map_err(|error| LegacyError::Unreadable {
        path: path.to_path_buf(),
        detail: error.to_string(),
    })
}
