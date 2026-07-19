use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::domain::{
    Member, StateDef, StoryEvent, StorySnapshot, SuperState, TypeDef, fold_story,
    validate_state_defs,
};
use crate::error::AppError;

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
            },
            StateDef {
                slug: "in-progress".to_string(),
                super_state: SuperState::Open,
                role: Some("active".to_string()),
            },
            StateDef {
                slug: "done".to_string(),
                super_state: SuperState::Closed,
                role: None,
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

pub fn save_states(root: &Path, states: &[StateDef]) -> Result<(), AppError> {
    validate_state_defs(states)?;
    let paths = ProjectPaths::new(root);
    fs::write(
        paths.states_file(),
        toml::to_string_pretty(&StatesFile {
            states: states.to_vec(),
        })?,
    )?;
    Ok(())
}

pub fn add_state(
    root: &Path,
    slug: &str,
    superstate: SuperState,
    role: Option<String>,
) -> Result<StateDef, AppError> {
    let mut states = load_states(root)?;
    if states.iter().any(|state| state.slug == slug) {
        return Err(AppError::Validation(format!(
            "state `{slug}` already exists"
        )));
    }

    let state = StateDef {
        slug: slug.to_string(),
        super_state: superstate,
        role,
    };
    states.push(state.clone());
    save_states(root, &states)?;
    Ok(state)
}

pub fn remove_state(root: &Path, slug: &str) -> Result<(), AppError> {
    let states = load_states(root)?;
    if !states.iter().any(|state| state.slug == slug) {
        return Err(AppError::NotFound(format!("state `{slug}` not found")));
    }

    if load_all_snapshots(root)?
        .into_iter()
        .any(|story| story.state == slug)
    {
        return Err(AppError::Validation(format!(
            "state `{slug}` is still used by an existing story"
        )));
    }

    let retained = states
        .into_iter()
        .filter(|state| state.slug != slug)
        .collect::<Vec<_>>();
    save_states(root, &retained)?;
    Ok(())
}

fn default_types() -> Vec<TypeDef> {
    vec![
        TypeDef {
            slug: "story".to_string(),
            description: Some("A user story or feature".to_string()),
        },
        TypeDef {
            slug: "epic".to_string(),
            description: Some("A large initiative containing child stories".to_string()),
        },
        TypeDef {
            slug: "bug".to_string(),
            description: Some("A defect or regression".to_string()),
        },
        TypeDef {
            slug: "chore".to_string(),
            description: Some("Maintenance or infrastructure work".to_string()),
        },
        TypeDef {
            slug: "task".to_string(),
            description: Some("A discrete unit of work".to_string()),
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

pub fn load_type_map(root: &Path) -> Result<BTreeMap<String, TypeDef>, AppError> {
    Ok(load_types(root)?
        .into_iter()
        .map(|t| (t.slug.clone(), t))
        .collect())
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

pub fn add_type(root: &Path, slug: &str, description: Option<&str>) -> Result<TypeDef, AppError> {
    if slug.eq_ignore_ascii_case("none") {
        return Err(AppError::Validation(
            "type slug `none` is reserved and cannot be used".to_string(),
        ));
    }
    if slug.eq_ignore_ascii_case("default") {
        return Err(AppError::Validation(
            "type slug `default` is reserved and cannot be used".to_string(),
        ));
    }

    let mut types = load_types(root)?;
    if types.iter().any(|t| t.slug == slug) {
        return Err(AppError::Validation(format!(
            "type `{slug}` already exists"
        )));
    }

    let type_def = TypeDef {
        slug: slug.to_string(),
        description: description.map(|d| d.to_string()),
    };
    types.push(type_def.clone());
    save_types(root, &types)?;
    Ok(type_def)
}

pub fn remove_type(root: &Path, slug: &str) -> Result<(), AppError> {
    let types = load_types(root)?;
    if types.len() <= 1 {
        return Err(AppError::Validation(
            "cannot remove the last type".to_string(),
        ));
    }
    if !types.iter().any(|t| t.slug == slug) {
        return Err(AppError::NotFound(format!("type `{slug}` not found")));
    }

    if load_all_snapshots(root)?
        .into_iter()
        .any(|story| story.story_type.as_deref() == Some(slug))
    {
        return Err(AppError::Validation(format!(
            "type `{slug}` is still used by an existing story"
        )));
    }

    let retained = types
        .into_iter()
        .filter(|t| t.slug != slug)
        .collect::<Vec<_>>();
    save_types(root, &retained)?;
    Ok(())
}

pub fn default_type(root: &Path) -> Result<String, AppError> {
    let types = load_types(root)?;
    types
        .first()
        .map(|t| t.slug.clone())
        .ok_or_else(|| AppError::Validation("types.toml has no types defined".to_string()))
}

pub fn default_open_state(root: &Path) -> Result<StateDef, AppError> {
    load_states(root)?
        .into_iter()
        .find(|state| state.super_state == SuperState::Open)
        .ok_or_else(|| AppError::Validation("project has no OPEN-mapped default state".to_string()))
}

pub fn is_auto_transition_enabled(root: &Path) -> Result<bool, AppError> {
    let paths = ProjectPaths::new(root);
    let raw = fs::read_to_string(paths.project_file())?;
    let project: ProjectFile = toml::from_str(&raw)?;
    Ok(project.sync.and_then(|s| s.auto_transition).unwrap_or(true))
}

pub fn get_stale_threshold(root: &Path) -> Result<String, AppError> {
    let paths = ProjectPaths::new(root);
    let raw = fs::read_to_string(paths.project_file())?;
    let project: ProjectFile = toml::from_str(&raw)?;
    Ok(project
        .doctor
        .and_then(|d| d.stale_threshold)
        .unwrap_or_else(|| "14d".to_string()))
}

pub fn find_active_state(root: &Path) -> Result<Option<StateDef>, AppError> {
    let states = load_states(root)?;
    // Priority 1: explicit role = "active"
    if let Some(state) = states.iter().find(|s| s.role.as_deref() == Some("active")) {
        return Ok(Some(state.clone()));
    }
    // Priority 2: heuristic - if exactly 2 OPEN states, the second is "active"
    let open_states: Vec<&StateDef> = states
        .iter()
        .filter(|s| s.super_state == SuperState::Open)
        .collect();
    if open_states.len() == 2 {
        return Ok(Some(open_states[1].clone()));
    }
    // No clear active state
    Ok(None)
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

pub fn find_member(root: &Path, lookup: &str) -> Result<Member, AppError> {
    let members = load_members(root)?;
    members
        .into_iter()
        .find(|member| member.id == lookup || member.github.as_deref() == Some(lookup))
        .ok_or_else(|| AppError::NotFound(format!("member `{lookup}` not found")))
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
    let event = StoryEvent::StoryCreated {
        at: now(),
        title: title.to_string(),
        state: state_slug,
    };
    write_story_events(root, &id, &[event])?;
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

pub fn load_story_snapshot(root: &Path, id: &str) -> Result<StorySnapshot, AppError> {
    match load_open_story_snapshot(root, id) {
        Ok(story) => Ok(story),
        Err(AppError::NotFound(_)) => load_archived_story(root, id),
        Err(error) => Err(error),
    }
}

pub fn is_archived(root: &Path, id: &str) -> Result<bool, AppError> {
    let connection = open_archive_connection(root)?;
    let count = connection.query_row(
        "SELECT COUNT(*) FROM closed_stories WHERE id = ?1",
        [id],
        |row| row.get::<_, i64>(0),
    )?;
    Ok(count > 0)
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
    let story_file = paths.open_story_file(id);
    if !story_file.exists() {
        return Err(AppError::NotFound(format!("story `{id}` not found")));
    }
    let states = load_state_map(root)?;

    // Append deletion events
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

    // Reload and fold
    let events = load_open_story_events(root, id)?;
    let snapshot = fold_story(id, &events, &states)?;

    // Archive to SQLite with deleted_reason
    let mut connection = open_archive_connection(root)?;
    let transaction = connection.transaction()?;
    transaction.execute(
        "INSERT OR REPLACE INTO closed_stories (id, snapshot_json, events_json, closed_at, state, deleted_reason) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            snapshot.id,
            serde_json::to_string(&snapshot)?,
            serde_json::to_string(&events)?,
            snapshot.closed_at.as_deref().unwrap_or(""),
            snapshot.state,
            Some(reason),
        ],
    )?;
    transaction.commit()?;

    // Remove the JSONL file
    fs::remove_file(&story_file)?;
    Ok(())
}

pub fn load_archived_story(root: &Path, id: &str) -> Result<StorySnapshot, AppError> {
    let connection = open_archive_connection(root)?;
    let mut statement =
        connection.prepare("SELECT snapshot_json FROM closed_stories WHERE id = ?1 LIMIT 1")?;
    let snapshot_json = statement
        .query_row([id], |row| row.get::<_, String>(0))
        .map_err(|_| AppError::NotFound(format!("story `{id}` not found")))?;
    Ok(serde_json::from_str(&snapshot_json)?)
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

pub fn open_story_exists(root: &Path, id: &str) -> bool {
    let paths = ProjectPaths::new(root);
    paths.open_story_file(id).exists()
}

pub fn unarchive_story(root: &Path, id: &str) -> Result<(), AppError> {
    let connection = open_archive_connection(root)?;
    let events_json: String = connection
        .query_row(
            "SELECT events_json FROM closed_stories WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
        .map_err(|_| AppError::NotFound(format!("story `{id}` not found in archive")))?;

    let events: Vec<StoryEvent> = serde_json::from_str(&events_json)?;
    // Filter out the close/delete markers so the story reopens cleanly as a
    // normal open story — `StoryDeleted` is stripped too so that "undeleting"
    // a soft-deleted story (see `Invocation::Reopen`) folds back to
    // `deleted: false` rather than staying CLOSED. The `[deleted] <reason>`
    // comment added by `delete_story` is left in place as audit history.
    let events: Vec<StoryEvent> = events
        .into_iter()
        .filter(|e| {
            !matches!(
                e,
                StoryEvent::StoryClosedAndArchived { .. } | StoryEvent::StoryDeleted { .. }
            )
        })
        .collect();

    rewrite_story_events(root, id, &events)?;
    connection.execute("DELETE FROM closed_stories WHERE id = ?1", [id])?;
    Ok(())
}

/// Outcome of [`repair_archived_snapshots`]: which archived stories' cached
/// snapshots were rewritten, and which could not be re-folded at all.
pub struct ArchiveRepairReport {
    /// IDs of archived stories whose cached `snapshot_json` was stale and has
    /// been rewritten to match a fresh fold of their event log.
    pub repaired: Vec<String>,
    /// Human-readable notes for archived stories that failed to re-fold
    /// (e.g. their event log references a state slug no longer configured),
    /// left untouched rather than overwritten with a broken snapshot.
    pub issues: Vec<String>,
}

/// Re-folds every archived story's cached `snapshot_json` from its
/// authoritative `events_json` and rewrites the cache when the two differ.
///
/// Unlike open stories (re-folded on every load, see
/// [`load_all_open_snapshots`]), archived snapshots are cached in SQLite and
/// read back verbatim (see [`load_all_archived_snapshots`]) — so a change to
/// `fold_story`'s behavior (for example, #18's fix making `StoryDeleted`
/// force `superstate: CLOSED`) does not retroactively apply to stories
/// archived before the change. `story doctor --fix` calls this to self-heal
/// those stale caches from the event log, which remains the source of truth.
pub fn repair_archived_snapshots(root: &Path) -> Result<ArchiveRepairReport, AppError> {
    let states = load_state_map(root)?;
    let connection = open_archive_connection(root)?;
    let mut statement = connection
        .prepare("SELECT id, snapshot_json, events_json FROM closed_stories ORDER BY id ASC")?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;

    let mut report = ArchiveRepairReport {
        repaired: Vec::new(),
        issues: Vec::new(),
    };
    for row in rows {
        let (id, cached_snapshot_json, events_json) = row?;
        let events: Vec<StoryEvent> = serde_json::from_str(&events_json)?;
        let refolded = match fold_story(&id, &events, &states) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                report
                    .issues
                    .push(format!("{id}: {error} (archive repair skipped)"));
                continue;
            }
        };

        let refolded_json = serde_json::to_string(&refolded)?;
        if refolded_json != cached_snapshot_json {
            connection.execute(
                "UPDATE closed_stories SET snapshot_json = ?1, closed_at = ?2, state = ?3 WHERE id = ?4",
                params![
                    refolded_json,
                    refolded.closed_at.as_deref().unwrap_or(""),
                    refolded.state,
                    id,
                ],
            )?;
            report.repaired.push(id);
        }
    }

    Ok(report)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectExport {
    pub schema: u32,
    pub prefix: Option<String>,
    pub states: Vec<StateDef>,
    #[serde(default)]
    pub types: Vec<TypeDef>,
    pub members: Vec<Member>,
    pub stories: Vec<ExportedStory>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExportedStory {
    pub id: String,
    pub events: Vec<StoryEvent>,
    pub archived: bool,
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
            let events = read_story_events(&path)?;
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
            events,
            archived: true,
        });
    }

    Ok(ProjectExport {
        schema: project.schema,
        prefix: project.prefix,
        states,
        types,
        members,
        stories,
    })
}

pub fn import_project(root: &Path, export: &ProjectExport) -> Result<(), AppError> {
    let paths = ProjectPaths::new(root);
    fs::create_dir_all(paths.open_stories_dir())?;
    fs::create_dir_all(paths.open_indexes_dir())?;
    fs::create_dir_all(paths.archive_dir())?;

    let project = ProjectFile {
        schema: export.schema,
        created_at: now(),
        prefix: export.prefix.clone(),
        sync: None,
        doctor: None,
    };
    fs::write(paths.project_file(), toml::to_string_pretty(&project)?)?;

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

    for story in &export.stories {
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
            let snapshot = fold_story(&story.id, &story.events, &state_map)?;
            let closed_at = snapshot.closed_at.clone().unwrap_or_else(now);
            let mut connection = open_archive_connection(root)?;
            let transaction = connection.transaction()?;
            transaction.execute(
                "INSERT OR REPLACE INTO closed_stories (id, snapshot_json, events_json, closed_at, state) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    snapshot.id,
                    serde_json::to_string(&snapshot)?,
                    serde_json::to_string(&story.events)?,
                    closed_at,
                    snapshot.state,
                ],
            )?;
            transaction.commit()?;
        } else {
            rewrite_story_events(root, &story.id, &story.events)?;
        }
    }

    fs::write(paths.next_id_file(), format!("{}\n", max_id + 1))?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn setup_project() -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        init_project(dir.path(), None).unwrap();
        dir
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
            },
            TypeDef {
                slug: "beta".to_string(),
                description: None,
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

    #[test]
    fn load_type_map_returns_btree_keyed_by_slug() {
        let dir = setup_project();
        let map = load_type_map(dir.path()).unwrap();
        assert_eq!(map.len(), 5);
        assert!(map.contains_key("story"));
        assert!(map.contains_key("epic"));
        assert!(map.contains_key("bug"));
        assert!(map.contains_key("chore"));
        assert!(map.contains_key("task"));
        assert_eq!(
            map["story"].description.as_deref(),
            Some("A user story or feature")
        );
    }

    // --- save_types ---

    #[test]
    fn save_types_writes_file() {
        let dir = setup_project();
        let custom = vec![TypeDef {
            slug: "feature".to_string(),
            description: Some("A feature".to_string()),
        }];
        save_types(dir.path(), &custom).unwrap();
        let types = load_types(dir.path()).unwrap();
        assert_eq!(types.len(), 1);
        assert_eq!(types[0].slug, "feature");
        assert_eq!(types[0].description.as_deref(), Some("A feature"));
    }

    // --- add_type ---

    #[test]
    fn add_type_appends_new_type() {
        let dir = setup_project();
        let result = add_type(dir.path(), "spike", Some("A time-boxed investigation")).unwrap();
        assert_eq!(result.slug, "spike");
        assert_eq!(
            result.description.as_deref(),
            Some("A time-boxed investigation")
        );
        let types = load_types(dir.path()).unwrap();
        assert_eq!(types.len(), 6);
        assert_eq!(types[5].slug, "spike");
    }

    #[test]
    fn add_type_rejects_duplicate() {
        let dir = setup_project();
        let result = add_type(dir.path(), "story", Some("duplicate"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn add_type_rejects_none_slug() {
        let dir = setup_project();
        let result = add_type(dir.path(), "none", Some("reserved"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("reserved"));
    }

    #[test]
    fn add_type_without_description() {
        let dir = setup_project();
        let result = add_type(dir.path(), "spike", None).unwrap();
        assert_eq!(result.slug, "spike");
        assert_eq!(result.description, None);
    }

    // --- remove_type ---

    #[test]
    fn remove_type_removes_unused_type() {
        let dir = setup_project();
        let types_before = load_types(dir.path()).unwrap();
        assert!(types_before.iter().any(|t| t.slug == "chore"));
        remove_type(dir.path(), "chore").unwrap();
        let types_after = load_types(dir.path()).unwrap();
        assert_eq!(types_after.len(), types_before.len() - 1);
        assert!(!types_after.iter().any(|t| t.slug == "chore"));
    }

    #[test]
    fn remove_type_rejects_nonexistent() {
        let dir = setup_project();
        let result = remove_type(dir.path(), "nonexistent");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn remove_type_rejects_in_use() {
        let dir = setup_project();
        // Create a story and set its type to "epic"
        let story = create_story(dir.path(), "test story", None).unwrap();
        write_story_events(
            dir.path(),
            &story.id,
            &[StoryEvent::StoryTypeSet {
                at: now(),
                story_type: "epic".to_string(),
            }],
        )
        .unwrap();
        let result = remove_type(dir.path(), "epic");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("still used"));
    }

    #[test]
    fn remove_type_rejects_last_type() {
        let dir = setup_project();
        let single = vec![TypeDef {
            slug: "only".to_string(),
            description: None,
        }];
        save_types(dir.path(), &single).unwrap();
        let result = remove_type(dir.path(), "only");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("last"));
    }

    // --- default_type ---

    #[test]
    fn default_type_returns_first_slug() {
        let dir = setup_project();
        let dt = default_type(dir.path()).unwrap();
        assert_eq!(dt, "story");
    }

    #[test]
    fn default_type_returns_first_after_custom_save() {
        let dir = setup_project();
        let custom = vec![
            TypeDef {
                slug: "zeta".to_string(),
                description: None,
            },
            TypeDef {
                slug: "alpha".to_string(),
                description: None,
            },
        ];
        save_types(dir.path(), &custom).unwrap();
        let dt = default_type(dir.path()).unwrap();
        assert_eq!(dt, "zeta");
    }

    #[test]
    fn default_type_errors_on_empty_types() {
        let dir = setup_project();
        save_types(dir.path(), &[]).unwrap();
        let result = default_type(dir.path());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("no types defined"));
    }

    // --- TypesFile wraps Vec<TypeDef> ---

    #[test]
    fn types_file_round_trips_through_toml() {
        let types = vec![
            TypeDef {
                slug: "story".to_string(),
                description: Some("A user story".to_string()),
            },
            TypeDef {
                slug: "bug".to_string(),
                description: None,
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
}
