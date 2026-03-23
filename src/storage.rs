use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::domain::{
    Member, StateDef, StoryEvent, StorySnapshot, SuperState, fold_story, validate_state_defs,
};
use crate::error::AppError;

#[derive(Clone, Debug)]
pub struct ProjectPaths {
    root: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct ProjectFile {
    schema: u32,
    created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prefix: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct StatesFile {
    states: Vec<StateDef>,
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
        };
        fs::write(paths.project_file(), toml::to_string_pretty(&project)?)?;
    }

    if !paths.states_file().exists() {
        let states = vec![
            StateDef {
                slug: "todo".to_string(),
                super_state: SuperState::Open,
            },
            StateDef {
                slug: "done".to_string(),
                super_state: SuperState::Closed,
            },
        ];
        save_states(root, &states)?;
    }

    if !paths.members_file().exists() {
        fs::write(paths.members_file(), "")?;
    }

    if !paths.next_id_file().exists() {
        fs::write(paths.next_id_file(), "1\n")?;
    }

    let connection = open_archive_connection(root)?;
    drop(connection);
    Ok(())
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

pub fn add_state(root: &Path, slug: &str, superstate: SuperState) -> Result<StateDef, AppError> {
    let mut states = load_states(root)?;
    if states.iter().any(|state| state.slug == slug) {
        return Err(AppError::Validation(format!(
            "state `{slug}` already exists"
        )));
    }

    let state = StateDef {
        slug: slug.to_string(),
        super_state: superstate,
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

fn load_project_prefix(root: &Path) -> Result<String, AppError> {
    let paths = ProjectPaths::new(root);
    let raw = fs::read_to_string(paths.project_file())?;
    let project: ProjectFile = toml::from_str(&raw)?;
    Ok(project.prefix.unwrap_or_else(|| "SH".to_string()))
}

pub fn create_story(root: &Path, title: &str) -> Result<StorySnapshot, AppError> {
    ensure_project(root)?;
    let id = next_story_id(root)?;
    let state = default_open_state(root)?;
    let event = StoryEvent::StoryCreated {
        at: now(),
        title: title.to_string(),
        state: state.slug,
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
    // Filter out the close event so the story reopens cleanly
    let events: Vec<StoryEvent> = events
        .into_iter()
        .filter(|e| !matches!(e, StoryEvent::StoryClosedAndArchived { .. }))
        .collect();

    rewrite_story_events(root, id, &events)?;
    connection.execute("DELETE FROM closed_stories WHERE id = ?1", [id])?;
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectExport {
    pub schema: u32,
    pub prefix: Option<String>,
    pub states: Vec<StateDef>,
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
    };
    fs::write(paths.project_file(), toml::to_string_pretty(&project)?)?;

    save_states(root, &export.states)?;

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
    Ok(connection)
}
