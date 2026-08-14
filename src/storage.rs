//! The legacy `.storyhook/` tree, written and read — the **rollback path**,
//! and nothing else.
//!
//! # No production caller, by design
//!
//! Nothing under `src/` calls into this module. Every `story` command runs
//! against the store; the entry point that used to reach this code
//! (`src/app.rs`) was deleted along with `lock.rs` and `registry.rs` once the
//! dashboard moved onto the services.
//! `tests/invoker_seam.rs::the_legacy_write_path_is_gone` fails if a `src/`
//! file so much as names `crate::storage`.
//!
//! # Then why is it still here?
//!
//! Because the rearchitecture is a **two-way door**, and this is the far side
//! of it. `docs/rearch/flip-checklist.md`'s rollback procedure is `store ->
//! story export -> ProjectExport -> a legacy tree`, and
//! [`import_project`] is what materializes that last step.
//! `tests/migrate_round_trip.rs` runs the whole loop for two fixtures and
//! compares the read models story by story; the W4 revert policy is
//! *conditional* on it staying green. Deleting this module would not simplify
//! the program, it would close the door.
//!
//! It is also what builds the legacy trees `story migrate` is tested against
//! (`tests/legacy_support/`), including the archived half — which lives in a
//! SQLite database and cannot be written by hand.
//!
//! # What was deleted
//!
//! Twenty-six functions: everything the CLI used to call and the round trip
//! does not need — `move_story_to_state`, `archive`/`unarchive`, the state and
//! type editors, `repair_archived_snapshots`, `state_usage`, the `doctor`
//! helpers. What survives is `init_project`, the catalog writers, the story
//! writers, `import_project`/`export_project`, and the readers the round trip
//! verifies with.
//!
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::domain::{
    Member, StateDef, StoryEvent, StorySnapshot, SuperState, TypeDef, fold_story,
    validate_state_defs, validate_state_defs_for_write,
};
use crate::error::AppError;
use crate::service::transfer::{ExportedEvent, ExportedSettings, ExportedStory, ProjectExport};

#[derive(Clone, Debug)]
pub struct ProjectPaths {
    root: PathBuf,
}

#[derive(Serialize, Deserialize, Default)]
struct SyncConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    auto_transition: Option<bool>,
}

#[derive(Serialize, Deserialize, Default)]
struct DoctorConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stale_threshold: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct ProjectFile {
    schema: u32,
    created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sync: Option<SyncConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    doctor: Option<DoctorConfig>,
}

#[derive(Serialize, Deserialize)]
struct StatesFile {
    states: Vec<StateDef>,
}

#[derive(Serialize, Deserialize)]
struct TypesFile {
    types: Vec<TypeDef>,
}

impl ProjectPaths {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    pub fn storyhook_dir(&self) -> PathBuf {
        self.root.join(".storyhook")
    }

    pub fn project_file(&self) -> PathBuf {
        self.storyhook_dir().join("project.toml")
    }

    pub fn states_file(&self) -> PathBuf {
        self.storyhook_dir().join("states.toml")
    }

    pub fn types_file(&self) -> PathBuf {
        self.storyhook_dir().join("types.toml")
    }

    pub fn members_file(&self) -> PathBuf {
        self.storyhook_dir().join("members.jsonl")
    }

    pub fn next_id_file(&self) -> PathBuf {
        self.storyhook_dir().join("next-id")
    }

    pub fn open_stories_dir(&self) -> PathBuf {
        self.storyhook_dir().join("open/stories")
    }

    pub fn open_indexes_dir(&self) -> PathBuf {
        self.storyhook_dir().join("open/indexes")
    }

    pub fn archive_dir(&self) -> PathBuf {
        self.storyhook_dir().join("archive")
    }

    pub fn archive_db(&self) -> PathBuf {
        self.archive_dir().join("archive.db")
    }

    pub fn open_story_file(&self, id: &str) -> PathBuf {
        self.open_stories_dir().join(format!("{id}.jsonl"))
    }

    /// Where the pre-rearchitecture binary kept its github-sync configuration
    /// (SH-189) — never written by this crate's production path, only by this
    /// module's rollback leg and by `tests/` fixtures that need to look like
    /// one.
    pub fn github_sync_file(&self) -> PathBuf {
        self.storyhook_dir().join("github-sync.toml")
    }

    /// Where the pre-rearchitecture binary kept one JSON file per story's
    /// github-sync merge base (SH-189), alongside [`Self::github_sync_file`].
    pub fn github_bases_dir(&self) -> PathBuf {
        self.storyhook_dir().join("github-sync/bases")
    }
}

pub fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub fn init_project(root: &Path, prefix: Option<&str>) -> Result<(), AppError> {
    let paths = ProjectPaths::new(root);
    fs::create_dir_all(paths.open_stories_dir())?;
    fs::create_dir_all(paths.open_indexes_dir())?;
    fs::create_dir_all(paths.archive_dir())?;

    if !paths.project_file().exists() {
        let project = ProjectFile {
            schema: 1,
            created_at: now(),
            prefix: prefix.map(|p| p.to_string()),
            sync: None,
            doctor: None,
        };
        fs::write(paths.project_file(), toml::to_string_pretty(&project)?)?;
    }

    if !paths.states_file().exists() {
        let states = vec![
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
        ];
        save_states(root, &states)?;
    }

    ensure_types_file(root)?;

    if !paths.members_file().exists() {
        fs::write(paths.members_file(), "")?;
    }

    if !paths.next_id_file().exists() {
        fs::write(paths.next_id_file(), "1\n")?;
    }

    let connection = open_archive_connection(root)?;
    drop(connection);

    // Create .gitignore inside .storyhook/ to exclude runtime files
    // (lock file, SQLite WAL/SHM) from version control.
    let inner_gitignore = paths.storyhook_dir().join(".gitignore");
    if !inner_gitignore.exists() {
        fs::write(
            &inner_gitignore,
            "# Runtime files — not project data\nlock\narchive/*.db-wal\narchive/*.db-shm\n",
        )?;
    }

    // Create CLAUDE.md inside .storyhook/ with full usage instructions.
    // Claude Code discovers this when accessing files in the directory.
    let claude_md_path = paths.storyhook_dir().join("CLAUDE.md");
    if !claude_md_path.exists() {
        let effective_prefix = prefix.unwrap_or("SH");
        fs::write(
            &claude_md_path,
            generate_claude_md(effective_prefix, "done"),
        )?;
    }

    // If a .gitignore exists, append a comment clarifying that .storyhook/
    // should NOT be ignored — unless the comment is already present.
    let gitignore_path = root.join(".gitignore");
    if gitignore_path.exists() {
        let content = fs::read_to_string(&gitignore_path)?;
        if !content.contains(".storyhook") {
            let mut file = OpenOptions::new().append(true).open(&gitignore_path)?;
            writeln!(file)?;
            writeln!(
                file,
                "# .storyhook/ is version-controlled project data — do not ignore"
            )?;
        }
    }

    Ok(())
}

fn generate_claude_md(prefix: &str, done_state: &str) -> String {
    format!(
        r#"# Task Management with Storyhook

This project uses **storyhook** (`story` CLI) for work tracking.

**Important:** The `.storyhook/` directory is version-controlled project data. Do NOT add it to `.gitignore`.

## Session lifecycle

1. Run `story load-context` at the start of every session to understand project state.
2. Run `story next` to find the highest-priority ready task.
3. Update story status as you work: `story move {prefix}-<n> in-progress`
4. Add progress notes: `story comment {prefix}-<n> "what changed and why"`
5. Mark complete: `story move {prefix}-<n> {done_state} "summary of what was delivered"`
6. Run `story handoff --since 2h` at end of session.

## Planning mode

When creating implementation plans, create a story for each discrete work item, phase, or issue:

```
story new "Phase 1: Set up database schema"
story new "Phase 2: Implement API endpoints"
story new "Phase 3: Add authentication middleware"
```

### Decompose workflow

For larger specs, use `story decompose` to parse a markdown or YAML file into stories
with relationships, priorities, and labels automatically:

```
story decompose spec.md --dry-run    # Preview without creating
story decompose spec.md              # Create stories from spec
cat spec.md | story decompose --stdin
```

### Relationship types

Define relationships between stories to express dependencies and structure:

| Relation | Inverse | Purpose |
|---|---|---|
| `blocks` | `blocked-by` | Task dependencies — `story next` respects these |
| `parent-of` | `child-of` | Hierarchy — group subtasks under a parent |
| `relates-to` | `relates-to` | General link between related stories |
| `duplicate-of` | `duplicate-of` | Mark a story as a duplicate |
| `obviates` | `obviated-by` | One story makes another unnecessary |

```
story relate {prefix}-1 parent-of {prefix}-2
story relate {prefix}-2 blocks {prefix}-3
story relate {prefix}-5 relates-to {prefix}-2
story relate {prefix}-6 obviates {prefix}-7
```

### Dependency graph

Visualize relationships and spot bottlenecks:

```
story graph                           # Full dependency overview
story graph --blocked-by {prefix}-1   # Trace why a story is blocked
```

Set priority on each story so `story next` surfaces the right work:

```
story prioritize {prefix}-1 critical
story prioritize {prefix}-4 high
story prioritize {prefix}-6 medium
```

## During execution

- Before starting a story: `story move {prefix}-<n> in-progress`
- When blocked: `story block {prefix}-<n> "reason"`
- When unblocked: `story unblock {prefix}-<n>`
- When done: `story move {prefix}-<n> {done_state} "what was delivered"`
- To check what's ready: `story next --count 5`
- To see blocked work: `story list --blocked`
- To see the dependency graph: `story graph`

## Commands

| Action | Command |
|---|---|
| Project overview | `story load-context` |
| Next ready task | `story next` |
| List open stories | `story list` |
| Show a story | `story show {prefix}-<n>` |
| Create a story | `story new "<title>"` |
| Add a comment | `story comment {prefix}-<n> "comment text"` |
| Move to state | `story move {prefix}-<n> <state>` |
| Set priority | `story prioritize {prefix}-<n> high` |
| Assign a story | `story assign {prefix}-<n> <member>` |
| Add a label | `story label {prefix}-<n> <label>` |
| Set multiple fields | `story set {prefix}-<n> --priority high --state in-progress` |
| Add relationship | `story relate {prefix}-1 blocks {prefix}-2` |
| Decompose a spec | `story decompose spec.md` |
| Search | `story search "<query>"` |
| Summary stats | `story summary` |
| Dependency graph | `story graph` |
| Interactive TUI | `story tui` |
| Session handoff | `story handoff --since 2h` |

Run `story help <command>` for detailed usage on any command, or `story help --compact` for the full reference.
"#,
        prefix = prefix,
        done_state = done_state,
    )
}

pub fn ensure_project(root: &Path) -> Result<(), AppError> {
    let paths = ProjectPaths::new(root);
    if !paths.project_file().exists() {
        return Err(AppError::NotFound(
            "story project not initialized in this directory; run `story init`".to_string(),
        ));
    }
    Ok(())
}

pub fn load_states(root: &Path) -> Result<Vec<StateDef>, AppError> {
    ensure_project(root)?;
    let paths = ProjectPaths::new(root);
    let raw = fs::read_to_string(paths.states_file())?;
    let states_file = toml::from_str::<StatesFile>(&raw)?;
    validate_state_defs(&states_file.states)?;
    Ok(states_file.states)
}

pub fn load_state_map(root: &Path) -> Result<BTreeMap<String, StateDef>, AppError> {
    Ok(load_states(root)?
        .into_iter()
        .map(|state| (state.slug.clone(), state))
        .collect())
}

/// Writes the whole state set, subject to [`validate_state_defs_for_write`].
///
/// Every mutation below funnels through here, so the strict rules are
/// enforced exactly once — including on `import_project`, which must not be
/// able to install a state set the tool itself would refuse to create.
pub fn save_states(root: &Path, states: &[StateDef]) -> Result<(), AppError> {
    validate_state_defs_for_write(states)?;
    let paths = ProjectPaths::new(root);
    fs::write(
        paths.states_file(),
        toml::to_string_pretty(&StatesFile {
            states: states.to_vec(),
        })?,
    )?;
    Ok(())
}

/// The result of an edit that may have migrated stories out of the state it
/// changed, so callers can report both halves of what happened.
#[derive(Clone, Debug)]
pub struct StateEdit {
    pub state: StateDef,
    /// How many open stories were moved out of the edited state.
    pub moved: usize,
}

// This is `storage.rs`'s own default set, frozen: a legacy `.storyhook/`
// tree genuinely could hold a `task` type (SH-157 only retires it from
// *new* stores), and no `emoji` column ever existed in that format's
// `types.toml`, so every entry here carries `emoji: None`.
fn default_types() -> Vec<TypeDef> {
    vec![
        TypeDef {
            slug: "story".to_string(),
            description: Some("A user story or feature".to_string()),
            emoji: None,
        },
        TypeDef {
            slug: "epic".to_string(),
            description: Some("A large initiative containing child stories".to_string()),
            emoji: None,
        },
        TypeDef {
            slug: "bug".to_string(),
            description: Some("A defect or regression".to_string()),
            emoji: None,
        },
        TypeDef {
            slug: "chore".to_string(),
            description: Some("Maintenance or infrastructure work".to_string()),
            emoji: None,
        },
        TypeDef {
            slug: "task".to_string(),
            description: Some("A discrete unit of work".to_string()),
            emoji: None,
        },
    ]
}

pub fn ensure_types_file(root: &Path) -> Result<(), AppError> {
    let paths = ProjectPaths::new(root);
    if !paths.types_file().exists() {
        save_types(root, &default_types())?;
    }
    Ok(())
}

pub fn load_types(root: &Path) -> Result<Vec<TypeDef>, AppError> {
    ensure_project(root)?;
    ensure_types_file(root)?;
    let paths = ProjectPaths::new(root);
    let raw = fs::read_to_string(paths.types_file())?;
    let types_file = toml::from_str::<TypesFile>(&raw)?;
    Ok(types_file.types)
}

pub fn save_types(root: &Path, types: &[TypeDef]) -> Result<(), AppError> {
    let paths = ProjectPaths::new(root);
    fs::write(
        paths.types_file(),
        toml::to_string_pretty(&TypesFile {
            types: types.to_vec(),
        })?,
    )?;
    Ok(())
}

pub fn default_open_state(root: &Path) -> Result<StateDef, AppError> {
    load_states(root)?
        .into_iter()
        .find(|state| state.super_state == SuperState::Open)
        .ok_or_else(|| AppError::Validation("project has no OPEN-mapped default state".to_string()))
}

pub fn load_members(root: &Path) -> Result<Vec<Member>, AppError> {
    ensure_project(root)?;
    let paths = ProjectPaths::new(root);
    let file = OpenOptions::new().read(true).open(paths.members_file())?;
    let reader = BufReader::new(file);
    let mut members = Vec::new();

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        members.push(serde_json::from_str(&line)?);
    }

    Ok(members)
}

pub fn store_member(root: &Path, member: &Member) -> Result<(), AppError> {
    ensure_project(root)?;
    let paths = ProjectPaths::new(root);
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.members_file())?;
    writeln!(file, "{}", serde_json::to_string(member)?)?;
    Ok(())
}

pub fn next_story_id(root: &Path) -> Result<String, AppError> {
    ensure_project(root)?;
    let paths = ProjectPaths::new(root);
    let current = fs::read_to_string(paths.next_id_file())?;
    let value = current
        .trim()
        .parse::<u64>()
        .map_err(|error| AppError::Storage(format!("invalid next-id counter: {error}")))?;
    fs::write(paths.next_id_file(), format!("{}\n", value + 1))?;
    let prefix = load_project_prefix(root)?;
    Ok(format!("{prefix}-{value}"))
}

pub fn load_project_prefix(root: &Path) -> Result<String, AppError> {
    let paths = ProjectPaths::new(root);
    let raw = fs::read_to_string(paths.project_file())?;
    let project: ProjectFile = toml::from_str(&raw)?;
    Ok(project.prefix.unwrap_or_else(|| "SH".to_string()))
}

pub fn create_story(
    root: &Path,
    title: &str,
    initial_state: Option<&str>,
) -> Result<StorySnapshot, AppError> {
    create_story_with_events(root, title, initial_state, &[])
}

/// Create a story and, in the same append to its event log, write additional
/// enrichment events (priority, labels, description, assignee, type, ...).
///
/// Writing `StoryCreated` and `extra` as a single batch (rather than two
/// separate [`write_story_events`] calls) means a new story is never left
/// half-enriched on disk between the two writes.
pub fn create_story_with_events(
    root: &Path,
    title: &str,
    initial_state: Option<&str>,
    extra: &[StoryEvent],
) -> Result<StorySnapshot, AppError> {
    ensure_project(root)?;
    let id = next_story_id(root)?;
    let state_slug = if let Some(slug) = initial_state {
        let states = load_states(root)?;
        let valid = states
            .iter()
            .any(|s| s.slug == slug && s.super_state == SuperState::Open);
        if !valid {
            return Err(AppError::Validation(format!(
                "'{slug}' is not a valid OPEN state. Available OPEN states: {}",
                states
                    .iter()
                    .filter(|s| s.super_state == SuperState::Open)
                    .map(|s| s.slug.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        slug.to_string()
    } else {
        default_open_state(root)?.slug
    };
    let created = StoryEvent::StoryCreated {
        at: now(),
        title: title.to_string(),
        state: state_slug,
    };
    let mut events = Vec::with_capacity(1 + extra.len());
    events.push(created);
    events.extend_from_slice(extra);
    write_story_events(root, &id, &events)?;
    load_open_story_snapshot(root, &id)
}

pub fn write_story_events(root: &Path, id: &str, events: &[StoryEvent]) -> Result<(), AppError> {
    let paths = ProjectPaths::new(root);
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.open_story_file(id))?;

    for event in events {
        writeln!(file, "{}", serde_json::to_string(event)?)?;
    }

    Ok(())
}

pub fn rewrite_story_events(root: &Path, id: &str, events: &[StoryEvent]) -> Result<(), AppError> {
    let paths = ProjectPaths::new(root);
    let mut body = String::new();
    for event in events {
        body.push_str(&serde_json::to_string(event)?);
        body.push('\n');
    }
    fs::write(paths.open_story_file(id), body)?;
    Ok(())
}

pub fn load_open_story_events(root: &Path, id: &str) -> Result<Vec<StoryEvent>, AppError> {
    ensure_project(root)?;
    let paths = ProjectPaths::new(root);
    let path = paths.open_story_file(id);
    if !path.exists() {
        return Err(AppError::NotFound(format!("story `{id}` not found")));
    }
    read_story_events(&path)
}

fn read_story_events(path: &Path) -> Result<Vec<StoryEvent>, AppError> {
    let file = OpenOptions::new().read(true).open(path)?;
    let reader = BufReader::new(file);
    let mut events = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        events.push(serde_json::from_str(&line)?);
    }
    Ok(events)
}

pub fn load_open_story_snapshot(root: &Path, id: &str) -> Result<StorySnapshot, AppError> {
    let states = load_state_map(root)?;
    let events = load_open_story_events(root, id)?;
    fold_story(id, &events, &states)
}

pub fn archive_story(root: &Path, id: &str) -> Result<StorySnapshot, AppError> {
    let states = load_state_map(root)?;
    let paths = ProjectPaths::new(root);
    let story_path = paths.open_story_file(id);
    if !story_path.exists() {
        return Err(AppError::NotFound(format!("story `{id}` not found")));
    }

    let events = load_open_story_events(root, id)?;
    let snapshot = fold_story(id, &events, &states)?;
    let closed_at = snapshot.closed_at.clone().unwrap_or_else(now);

    let mut connection = open_archive_connection(root)?;
    let transaction = connection.transaction()?;
    transaction.execute(
        "INSERT OR REPLACE INTO closed_stories (id, snapshot_json, events_json, closed_at, state) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            snapshot.id,
            serde_json::to_string(&snapshot)?,
            serde_json::to_string(&events)?,
            closed_at,
            snapshot.state,
        ],
    )?;
    transaction.commit()?;
    fs::remove_file(story_path)?;
    Ok(snapshot)
}

pub fn delete_story(root: &Path, id: &str, reason: &str) -> Result<(), AppError> {
    let paths = ProjectPaths::new(root);
    if !paths.open_story_file(id).exists() {
        return Err(AppError::NotFound(format!("story `{id}` not found")));
    }

    // Append deletion events, then archive exactly like an ordinary close —
    // `fold_story` forces `superstate: CLOSED` whenever `StoryDeleted` is
    // present, so `archive_story`'s normal fold-and-insert already produces
    // a correctly CLOSED, `deleted: true` snapshot. No bespoke archival
    // logic (or a separate `deleted_reason` SQLite column — the reason now
    // round-trips through `snapshot_json` via `StorySnapshot::deleted_reason`
    // instead) is needed here.
    let ts = now();
    write_story_events(
        root,
        id,
        &[
            StoryEvent::StoryCommentAdded {
                at: ts.clone(),
                text: format!("[deleted] {reason}"),
            },
            StoryEvent::StoryDeleted {
                at: ts,
                reason: reason.to_string(),
            },
        ],
    )?;

    archive_story(root, id)?;
    Ok(())
}

pub fn load_all_open_snapshots(root: &Path) -> Result<Vec<StorySnapshot>, AppError> {
    ensure_project(root)?;
    let paths = ProjectPaths::new(root);
    let states = load_state_map(root)?;
    let mut stories = Vec::new();

    if !paths.open_stories_dir().exists() {
        return Ok(stories);
    }

    let mut entries = fs::read_dir(paths.open_stories_dir())?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        let id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| AppError::Storage("invalid story filename".to_string()))?;
        let events = read_story_events(&path)?;
        stories.push(fold_story(id, &events, &states)?);
    }

    Ok(stories)
}

pub fn load_all_archived_snapshots(root: &Path) -> Result<Vec<StorySnapshot>, AppError> {
    let connection = open_archive_connection(root)?;
    let mut statement =
        connection.prepare("SELECT snapshot_json FROM closed_stories ORDER BY id ASC")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut stories = Vec::new();
    for row in rows {
        stories.push(serde_json::from_str::<StorySnapshot>(&row?)?);
    }
    Ok(stories)
}

pub fn load_all_snapshots(root: &Path) -> Result<Vec<StorySnapshot>, AppError> {
    let mut stories = load_all_open_snapshots(root)?;
    stories.extend(load_all_archived_snapshots(root)?);
    Ok(stories)
}

/// Reads the pre-rearchitecture github-sync configuration file, if this tree
/// carries one — `.storyhook/github-sync.toml` (SH-189).
///
/// Held as an opaque `serde_json::Value`, matching
/// [`ProjectExport::github_sync`] and the store's own
/// `project_settings.github_sync` column: this module has no knowledge of the
/// `github-sync`-feature-gated shape and needs none to carry the bytes through
/// unchanged.
fn load_legacy_github_sync(root: &Path) -> Result<Option<serde_json::Value>, AppError> {
    let path = ProjectPaths::new(root).github_sync_file();
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path)?;
    let value: toml::Value = toml::from_str(&text)?;
    Ok(Some(serde_json::to_value(value)?))
}

/// Writes `.storyhook/github-sync.toml` in the exact shape the
/// pre-rearchitecture binary read (SH-189).
///
/// `pub` for the same reason [`save_states`] and [`store_member`] are: this
/// module is the one place that knows how to *write* the legacy format, so a
/// test fixture that needs a tree with github-sync configured builds it here
/// rather than assembling bytes by hand and testing its own idea of the format.
pub fn save_legacy_github_sync(
    root: &Path,
    github_sync: &serde_json::Value,
) -> Result<(), AppError> {
    let path = ProjectPaths::new(root).github_sync_file();
    let value: toml::Value = serde_json::from_value(github_sync.clone())?;
    fs::write(path, toml::to_string_pretty(&value)?)?;
    Ok(())
}

/// Reads every per-story github-sync merge base the pre-rearchitecture binary
/// left under `.storyhook/github-sync/bases/` (SH-189), keyed by story id.
fn load_legacy_github_bases(root: &Path) -> Result<BTreeMap<String, StorySnapshot>, AppError> {
    let dir = ProjectPaths::new(root).github_bases_dir();
    let mut bases = BTreeMap::new();
    if !dir.exists() {
        return Ok(bases);
    }
    let mut entries = fs::read_dir(&dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| AppError::Storage("invalid github-sync base filename".to_string()))?
            .to_string();
        let text = fs::read_to_string(&path)?;
        let snapshot: StorySnapshot = serde_json::from_str(&text)?;
        bases.insert(id, snapshot);
    }
    Ok(bases)
}

/// Writes every carried github-sync merge base to
/// `.storyhook/github-sync/bases/<id>.json` (SH-189), one file per story,
/// exactly as the pre-rearchitecture binary wrote them.
///
/// `pub` for the reason [`save_legacy_github_sync`] is.
pub fn save_legacy_github_bases(
    root: &Path,
    bases: &BTreeMap<String, StorySnapshot>,
) -> Result<(), AppError> {
    if bases.is_empty() {
        return Ok(());
    }
    let dir = ProjectPaths::new(root).github_bases_dir();
    fs::create_dir_all(&dir)?;
    for (id, snapshot) in bases {
        let text = serde_json::to_string_pretty(snapshot)?;
        fs::write(dir.join(format!("{id}.json")), text)?;
    }
    Ok(())
}

pub fn export_project(root: &Path) -> Result<ProjectExport, AppError> {
    ensure_project(root)?;
    let paths = ProjectPaths::new(root);

    let raw = fs::read_to_string(paths.project_file())?;
    let project: ProjectFile = toml::from_str(&raw)?;

    let states = load_states(root)?;
    let types = load_types(root)?;
    let members = load_members(root)?;
    let mut stories = Vec::new();

    // Export open stories
    if paths.open_stories_dir().exists() {
        let mut entries = fs::read_dir(paths.open_stories_dir())?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            let id = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .ok_or_else(|| AppError::Storage("invalid story filename".to_string()))?
                .to_string();
            // A legacy log holds only kinds this build understands, because
            // `read_story_events` parses each line as a `StoryEvent` — so every
            // event out of a tree is `Known` by construction.
            let events = read_story_events(&path)?
                .into_iter()
                .map(ExportedEvent::Known)
                .collect();
            stories.push(ExportedStory {
                id,
                events,
                archived: false,
            });
        }
    }

    // Export archived stories
    let connection = open_archive_connection(root)?;
    let mut statement =
        connection.prepare("SELECT id, events_json FROM closed_stories ORDER BY id ASC")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (id, events_json) = row?;
        let events: Vec<StoryEvent> = serde_json::from_str(&events_json)?;
        stories.push(ExportedStory {
            id,
            events: events.into_iter().map(ExportedEvent::Known).collect(),
            archived: true,
        });
    }

    Ok(ProjectExport {
        schema: project.schema,
        prefix: project.prefix,
        // Through the document's own constructor rather than built here, so
        // that "nothing is set" gets the same encoding from both exporters. An
        // emitted empty table and an absent one are the same fact, and
        // `a_round_trip_survives_a_second_lap` byte-compares the two documents.
        settings: ExportedSettings::new(
            project.sync.as_ref().and_then(|sync| sync.auto_transition),
            project
                .doctor
                .as_ref()
                .and_then(|doctor| doctor.stale_threshold.clone()),
        ),
        // A legacy tree has never had anywhere to record a registered origin —
        // `project.toml` carries no such table, before or after the
        // rearchitecture — so this leg of the export always answers empty. The
        // store-side restore is the one that carries them; see `ExportedRemote`.
        remotes: Vec::new(),
        // The pre-rearchitecture binary kept these beside `project.toml`
        // rather than inside it — see `ProjectPaths::github_sync_file`/
        // `github_bases_dir` (SH-189).
        github_sync: load_legacy_github_sync(root)?,
        github_bases: load_legacy_github_bases(root)?,
        states,
        types,
        members,
        stories,
    })
}

/// An event a rollback could not carry into the legacy tree, and where it was.
///
/// A legacy tree cannot hold an event this build does not understand: every
/// line of a `.jsonl` log is parsed as a `StoryEvent` by [`read_story_events`]
/// and an archived story is folded on the way in, so a tree carrying one would
/// be unreadable by `storage.rs` itself — and by the reverted binary a rollback
/// exists to hand the data back to, which is older still. Dropping it is
/// therefore not a choice this module makes but a property of the format it
/// writes; what it *does* choose is to say so.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UncarriedEvent {
    /// The story it belonged to.
    pub story: String,
    /// Its position in that story's history, counting from one.
    pub position: usize,
    /// Its `kind` discriminant.
    pub kind: String,
}

/// Materializes an export document as a legacy tree — the rollback path.
///
/// Returns every event it could not carry; see [`UncarriedEvent`]. The list is
/// empty for a document produced by a build that understood everything in it,
/// which is every document taken before a storyhook that writes a kind this one
/// has never heard of.
///
/// The whole document is classified **before** anything is written, because
/// this function writes story by story with `fs::write` and holds no
/// transaction: a refusal discovered halfway would leave a half-built tree on
/// disk, which is worse than either outcome it is choosing between.
pub fn import_project(
    root: &Path,
    export: &ProjectExport,
) -> Result<Vec<UncarriedEvent>, AppError> {
    let mut carried: Vec<Vec<StoryEvent>> = Vec::with_capacity(export.stories.len());
    let mut uncarried = Vec::new();
    for story in &export.stories {
        let mut events = Vec::with_capacity(story.events.len());
        for (index, event) in story.events.iter().enumerate() {
            match event {
                ExportedEvent::Known(decoded) => events.push(decoded.clone()),
                ExportedEvent::Unknown(raw) => uncarried.push(UncarriedEvent {
                    story: story.id.clone(),
                    position: index + 1,
                    kind: raw.kind.clone(),
                }),
            }
        }
        carried.push(events);
    }

    let paths = ProjectPaths::new(root);
    fs::create_dir_all(paths.open_stories_dir())?;
    fs::create_dir_all(paths.open_indexes_dir())?;
    fs::create_dir_all(paths.archive_dir())?;

    // The settings the document carries reach the tree's `project.toml`, which
    // is the only place a reverted binary would look for them. `None` stood
    // here until SH-133, so a rollback silently restored `sync.auto_transition`
    // to its default — and that default is `true`, so the setting whose purpose
    // is stopping `commit-sync` came back switched on.
    let settings = export.settings.as_ref();
    let project = ProjectFile {
        schema: export.schema,
        created_at: now(),
        prefix: export.prefix.clone(),
        sync: settings
            .and_then(ExportedSettings::auto_transition)
            .map(|auto_transition| SyncConfig {
                auto_transition: Some(auto_transition),
            }),
        doctor: settings
            .and_then(ExportedSettings::stale_threshold)
            .map(|stale_threshold| DoctorConfig {
                stale_threshold: Some(stale_threshold.to_string()),
            }),
    };
    fs::write(paths.project_file(), toml::to_string_pretty(&project)?)?;

    // Beside `project.toml`, not inside it — matching where the
    // pre-rearchitecture binary itself read them from (SH-189). Written before
    // the bases so a reader always finds a mapping before it finds what it
    // merged against, though nothing here depends on that order.
    if let Some(github_sync) = &export.github_sync {
        save_legacy_github_sync(root, github_sync)?;
    }
    save_legacy_github_bases(root, &export.github_bases)?;

    save_states(root, &export.states)?;

    if !export.types.is_empty() {
        save_types(root, &export.types)?;
    }

    // Write members
    fs::write(paths.members_file(), "")?;
    for member in &export.members {
        store_member(root, member)?;
    }

    // Write stories
    let mut max_id: u64 = 0;
    let state_map = load_state_map(root)?;

    for (story, events) in export.stories.iter().zip(&carried) {
        // Track max ID for next-id counter
        if let Some(num) = story
            .id
            .split('-')
            .nth(1)
            .and_then(|n| n.parse::<u64>().ok())
            && num > max_id
        {
            max_id = num;
        }

        if story.archived {
            let snapshot = fold_story(&story.id, events, &state_map)?;
            let closed_at = snapshot.closed_at.clone().unwrap_or_else(now);
            let mut connection = open_archive_connection(root)?;
            let transaction = connection.transaction()?;
            transaction.execute(
                "INSERT OR REPLACE INTO closed_stories (id, snapshot_json, events_json, closed_at, state) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    snapshot.id,
                    serde_json::to_string(&snapshot)?,
                    serde_json::to_string(events)?,
                    closed_at,
                    snapshot.state,
                ],
            )?;
            transaction.commit()?;
        } else {
            rewrite_story_events(root, &story.id, events)?;
        }
    }

    fs::write(paths.next_id_file(), format!("{}\n", max_id + 1))?;
    Ok(uncarried)
}

fn open_archive_connection(root: &Path) -> Result<Connection, AppError> {
    let paths = ProjectPaths::new(root);
    fs::create_dir_all(paths.archive_dir())?;
    let connection = Connection::open(paths.archive_db())?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.execute(
        "CREATE TABLE IF NOT EXISTS closed_stories (
            id TEXT PRIMARY KEY,
            snapshot_json TEXT NOT NULL,
            events_json TEXT NOT NULL,
            closed_at TEXT NOT NULL,
            state TEXT NOT NULL
        )",
        [],
    )?;

    // Migration: add deleted_reason column if missing
    let has_col: bool = connection
        .prepare("PRAGMA table_info(closed_stories)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|r| r.ok())
        .any(|name| name == "deleted_reason");
    if !has_col {
        connection.execute(
            "ALTER TABLE closed_stories ADD COLUMN deleted_reason TEXT",
            [],
        )?;
    }

    Ok(connection)
}

// TODO(rearch): migrate to storyhook_test_support::scratch_dir — see clippy.toml.
#[allow(clippy::disallowed_methods)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Priority;
    use tempfile::tempdir;

    fn setup_project() -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        init_project(dir.path(), None).unwrap();
        dir
    }

    /// A minimal, otherwise-arbitrary snapshot — usable as a github-sync
    /// merge base's contents, since these tests are about whether it survives
    /// a round trip intact, not about what it holds.
    fn sample_snapshot(id: &str) -> StorySnapshot {
        StorySnapshot {
            id: id.to_string(),
            title: "A story".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            state: "todo".to_string(),
            superstate: SuperState::Open,
            assignee: None,
            awaiting: None,
            comments: Vec::new(),
            referenced_by_commits: Vec::new(),
            relationships: Vec::new(),
            priority: Priority::None,
            labels: Vec::new(),
            story_type: None,
            description: None,
            closed_at: None,
            deleted: false,
            deleted_reason: None,
            hidden_at: None,
            draft: false,
        }
    }

    /// The three default states, minimally enough for `import_project` to
    /// write `states.toml` without needing a real project to derive them from.
    fn sample_states() -> Vec<StateDef> {
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

    // --- ProjectPaths::types_file ---

    #[test]
    fn types_file_returns_correct_path() {
        let paths = ProjectPaths::new(Path::new("/tmp/test"));
        assert_eq!(
            paths.types_file(),
            PathBuf::from("/tmp/test/.storyhook/types.toml")
        );
    }

    // --- ensure_types_file ---

    #[test]
    fn ensure_types_file_creates_defaults_when_missing() {
        let dir = setup_project();
        let paths = ProjectPaths::new(dir.path());
        // init_project already calls ensure_types_file, so the file exists.
        // Remove it and call ensure again.
        fs::remove_file(paths.types_file()).unwrap();
        assert!(!paths.types_file().exists());
        ensure_types_file(dir.path()).unwrap();
        assert!(paths.types_file().exists());
        let types = load_types(dir.path()).unwrap();
        assert_eq!(types.len(), 5);
        assert_eq!(types[0].slug, "story");
    }

    #[test]
    fn ensure_types_file_does_not_overwrite_existing() {
        let dir = setup_project();
        // Save a custom types file with only 2 entries
        let custom = vec![
            TypeDef {
                slug: "alpha".to_string(),
                description: None,
                emoji: None,
            },
            TypeDef {
                slug: "beta".to_string(),
                description: None,
                emoji: None,
            },
        ];
        save_types(dir.path(), &custom).unwrap();
        ensure_types_file(dir.path()).unwrap();
        let types = load_types(dir.path()).unwrap();
        assert_eq!(types.len(), 2);
        assert_eq!(types[0].slug, "alpha");
    }

    // --- init_project creates types.toml ---

    #[test]
    fn init_project_creates_types_file() {
        let dir = tempdir().unwrap();
        init_project(dir.path(), None).unwrap();
        let paths = ProjectPaths::new(dir.path());
        assert!(paths.types_file().exists());
    }

    // --- load_types ---

    #[test]
    fn load_types_returns_default_types() {
        let dir = setup_project();
        let types = load_types(dir.path()).unwrap();
        assert_eq!(types.len(), 5);
        let slugs: Vec<&str> = types.iter().map(|t| t.slug.as_str()).collect();
        assert_eq!(slugs, vec!["story", "epic", "bug", "chore", "task"]);
    }

    #[test]
    fn load_types_auto_creates_if_missing() {
        let dir = setup_project();
        let paths = ProjectPaths::new(dir.path());
        fs::remove_file(paths.types_file()).unwrap();
        // load_types calls ensure_types_file lazily
        let types = load_types(dir.path()).unwrap();
        assert_eq!(types.len(), 5);
    }

    // --- load_type_map ---

    // --- save_types ---

    #[test]
    fn save_types_writes_file() {
        let dir = setup_project();
        let custom = vec![TypeDef {
            slug: "feature".to_string(),
            description: Some("A feature".to_string()),
            emoji: None,
        }];
        save_types(dir.path(), &custom).unwrap();
        let types = load_types(dir.path()).unwrap();
        assert_eq!(types.len(), 1);
        assert_eq!(types[0].slug, "feature");
        assert_eq!(types[0].description.as_deref(), Some("A feature"));
    }

    // --- add_type ---

    // --- create_story_with_events ---

    #[test]
    fn create_story_with_events_writes_single_batch() {
        let dir = setup_project();
        let story = create_story_with_events(
            dir.path(),
            "Enriched story",
            None,
            &[
                StoryEvent::StoryPrioritySet {
                    at: now(),
                    priority: Priority::High,
                },
                StoryEvent::StoryDescriptionSet {
                    at: now(),
                    description: "Batched description".to_string(),
                },
            ],
        )
        .unwrap();

        assert_eq!(story.priority, Priority::High);
        assert_eq!(story.description.as_deref(), Some("Batched description"));

        let paths = ProjectPaths::new(dir.path());
        let events = read_story_events(&paths.open_story_file(&story.id)).unwrap();
        assert_eq!(events.len(), 3, "created + 2 enrichment events in one log");
    }

    // --- distinct_labels ---

    // --- state descriptions (SH-49 regression) ---

    #[test]
    fn save_states_round_trips_descriptions() {
        let dir = setup_project();
        let states = vec![
            StateDef {
                slug: "todo".to_string(),
                super_state: SuperState::Open,
                role: None,
                description: Some("Not started yet".to_string()),
            },
            StateDef {
                slug: "done".to_string(),
                super_state: SuperState::Closed,
                role: None,
                description: None,
            },
        ];
        save_states(dir.path(), &states).unwrap();

        let loaded = load_states(dir.path()).unwrap();
        assert_eq!(loaded[0].description.as_deref(), Some("Not started yet"));
        assert_eq!(loaded[1].description, None);
    }

    /// A state without a description must not gain an empty `description = ""`
    /// line — the on-disk format stays byte-identical for projects that never
    /// used descriptions.
    #[test]
    fn save_states_omits_absent_descriptions() {
        let dir = setup_project();
        let raw = fs::read_to_string(ProjectPaths::new(dir.path()).states_file()).unwrap();
        assert!(!raw.contains("description"), "unexpected key:\n{raw}");
    }

    // --- state_usage ---

    // --- add_state ---

    // --- update_state ---

    // --- remove_state ---

    // --- reorder_states ---

    // --- remove_type ---

    // --- default_type ---

    // --- TypesFile wraps Vec<TypeDef> ---

    #[test]
    fn types_file_round_trips_through_toml() {
        let types = vec![
            TypeDef {
                slug: "story".to_string(),
                description: Some("A user story".to_string()),
                emoji: None,
            },
            TypeDef {
                slug: "bug".to_string(),
                description: None,
                emoji: None,
            },
        ];
        let types_file = TypesFile {
            types: types.clone(),
        };
        let serialized = toml::to_string_pretty(&types_file).unwrap();
        let deserialized: TypesFile = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.types.len(), 2);
        assert_eq!(deserialized.types[0].slug, "story");
        assert_eq!(
            deserialized.types[0].description.as_deref(),
            Some("A user story")
        );
        assert_eq!(deserialized.types[1].slug, "bug");
        assert_eq!(deserialized.types[1].description, None);
    }

    // --- github-sync carry, the legacy rollback leg (SH-189) ---

    #[test]
    fn a_tree_with_no_github_sync_file_answers_none() {
        let dir = setup_project();
        assert_eq!(load_legacy_github_sync(dir.path()).unwrap(), None);
        assert!(load_legacy_github_bases(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn the_github_sync_blob_round_trips_through_the_legacy_toml_file() {
        let dir = setup_project();
        let blob = serde_json::json!({
            "github": {"owner": "acme", "repo": "widgets"},
            "sync": {"mode": "manual", "last_sync_at": "2026-01-01T00:00:00Z"},
            "etags": {"issues": "abc123"},
            "mappings": [
                {"story_id": "SH-1", "issue_number": 42, "last_synced_at": "2026-01-01T00:00:00Z"},
            ],
        });
        save_legacy_github_sync(dir.path(), &blob).unwrap();
        assert!(
            ProjectPaths::new(dir.path()).github_sync_file().exists(),
            "must write .storyhook/github-sync.toml, the pre-rearchitecture path"
        );
        assert_eq!(load_legacy_github_sync(dir.path()).unwrap(), Some(blob));
    }

    #[test]
    fn github_bases_round_trip_one_file_per_story() {
        let dir = setup_project();
        let snapshot = sample_snapshot("SH-1");
        let bases = BTreeMap::from([("SH-1".to_string(), snapshot.clone())]);
        save_legacy_github_bases(dir.path(), &bases).unwrap();
        assert!(
            ProjectPaths::new(dir.path())
                .github_bases_dir()
                .join("SH-1.json")
                .exists(),
            "one JSON file per story, the pre-rearchitecture path"
        );
        assert_eq!(load_legacy_github_bases(dir.path()).unwrap(), bases);
    }

    #[test]
    fn saving_no_github_bases_creates_no_directory() {
        let dir = setup_project();
        save_legacy_github_bases(dir.path(), &BTreeMap::new()).unwrap();
        assert!(
            !ProjectPaths::new(dir.path()).github_bases_dir().exists(),
            "a project with no github-sync must not grow an empty bases directory"
        );
    }

    #[test]
    fn export_project_carries_github_sync_and_bases_from_a_legacy_tree() {
        let dir = setup_project();
        let blob = serde_json::json!({
            "github": {"owner": "acme", "repo": "widgets"},
            "sync": {"mode": "manual"},
        });
        save_legacy_github_sync(dir.path(), &blob).unwrap();
        let snapshot = sample_snapshot("SH-1");
        save_legacy_github_bases(
            dir.path(),
            &BTreeMap::from([("SH-1".to_string(), snapshot.clone())]),
        )
        .unwrap();

        let export = export_project(dir.path()).unwrap();
        assert_eq!(export.github_sync, Some(blob));
        assert_eq!(export.github_bases.get("SH-1"), Some(&snapshot));
    }

    #[test]
    fn import_project_writes_github_sync_and_bases_into_a_fresh_legacy_tree() {
        use crate::service::transfer::ProjectExport;

        let blob = serde_json::json!({
            "github": {"owner": "acme", "repo": "widgets"},
            "sync": {"mode": "manual"},
        });
        let snapshot = sample_snapshot("SH-1");
        let document = ProjectExport {
            schema: 1,
            prefix: None,
            states: sample_states(),
            types: Vec::new(),
            members: Vec::new(),
            settings: None,
            remotes: Vec::new(),
            github_sync: Some(blob.clone()),
            github_bases: BTreeMap::from([("SH-1".to_string(), snapshot)]),
            stories: Vec::new(),
        };

        let dir = tempdir().unwrap();
        import_project(dir.path(), &document).unwrap();

        assert_eq!(load_legacy_github_sync(dir.path()).unwrap(), Some(blob));
        assert!(
            load_legacy_github_bases(dir.path())
                .unwrap()
                .contains_key("SH-1")
        );
    }
}
