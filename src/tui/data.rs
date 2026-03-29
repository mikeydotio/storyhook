use std::collections::BTreeMap;
use std::path::Path;

use crate::domain::{Member, StateDef, StorySnapshot, SuperState};
use crate::error::AppError;
use crate::storage;

/// Bridge between the storage layer and TUI state.
///
/// Loads project data from disk WITHOUT holding the project lock.
/// Called on startup and after every refresh.
pub struct DataStore {
    pub states: Vec<StateDef>,
    pub state_map: BTreeMap<String, StateDef>,
    pub stories: Vec<StorySnapshot>,
    pub prefix: String,
    pub members: Vec<Member>,
}

impl DataStore {
    /// Load everything from disk without holding the project lock.
    pub fn load(root: &Path) -> Result<Self, AppError> {
        storage::ensure_project(root)?;
        let states = storage::load_states(root)?;
        let state_map = storage::load_state_map(root)?;
        let stories = load_open_snapshots_tolerant(root)?;
        let prefix = storage::load_project_prefix(root)?;
        let members = storage::load_members(root)?;

        Ok(Self {
            states,
            state_map,
            stories,
            prefix,
            members,
        })
    }

    /// Construct a DataStore from pre-built data, for testing without filesystem.
    #[cfg(test)]
    pub fn from_test_data(
        states: Vec<StateDef>,
        stories: Vec<StorySnapshot>,
        prefix: String,
        members: Vec<Member>,
    ) -> Self {
        let state_map = states
            .iter()
            .map(|s| (s.slug.clone(), s.clone()))
            .collect();
        Self {
            states,
            state_map,
            stories,
            prefix,
            members,
        }
    }

    /// Stories grouped by state, in state definition order.
    /// Only includes states with OPEN superstates.
    pub fn stories_by_state(&self) -> Vec<(&StateDef, Vec<&StorySnapshot>)> {
        self.states
            .iter()
            .filter(|state| state.super_state == SuperState::Open)
            .map(|state| {
                let matching: Vec<&StorySnapshot> = self
                    .stories
                    .iter()
                    .filter(|story| story.state == state.slug)
                    .collect();
                (state, matching)
            })
            .collect()
    }

    /// Find a story by ID.
    pub fn find_story(&self, id: &str) -> Option<&StorySnapshot> {
        self.stories.iter().find(|story| story.id == id)
    }

    /// Total number of open stories.
    pub fn story_count(&self) -> usize {
        self.stories.len()
    }
}

/// Load all open snapshots, tolerating a trailing incomplete JSON line.
///
/// When reading JSONL event files, a concurrent append-mode write could leave
/// a partial trailing line. If serde_json parse fails on the last line of a
/// file, we skip it rather than propagating the error.
fn load_open_snapshots_tolerant(root: &Path) -> Result<Vec<StorySnapshot>, AppError> {
    use std::fs;
    use std::io::{BufRead, BufReader};

    storage::ensure_project(root)?;
    let paths = storage::ProjectPaths::new(root);
    let states = storage::load_state_map(root)?;
    let mut stories = Vec::new();

    let stories_dir = paths.open_stories_dir();
    if !stories_dir.exists() {
        return Ok(stories);
    }

    let mut entries = fs::read_dir(&stories_dir)?.collect::<Result<Vec<_>, _>>()?;
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

        let file = fs::OpenOptions::new().read(true).open(&path)?;
        let reader = BufReader::new(file);
        let mut events = Vec::new();
        let lines: Vec<String> = reader.lines().collect::<Result<Vec<_>, _>>()?;
        let line_count = lines.len();

        for (i, line) in lines.into_iter().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str(&line) {
                Ok(event) => events.push(event),
                Err(_) if i == line_count - 1 => {
                    // Skip incomplete trailing line from concurrent write
                }
                Err(e) => return Err(AppError::from(e)),
            }
        }

        if !events.is_empty() {
            stories.push(crate::domain::fold_story(id, &events, &states)?);
        }
    }

    Ok(stories)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Priority;

    fn test_states() -> Vec<StateDef> {
        vec![
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
        ]
    }

    fn test_snapshot(id: &str, state: &str) -> StorySnapshot {
        StorySnapshot {
            id: id.to_string(),
            title: format!("Story {id}"),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            state: state.to_string(),
            superstate: if state == "done" {
                SuperState::Closed
            } else {
                SuperState::Open
            },
            assignee: None,
            awaiting: None,
            comments: vec![],
            relationships: vec![],
            priority: Priority::None,
            labels: vec![],
            closed_at: None,
        }
    }

    #[test]
    fn stories_by_state_groups_correctly() {
        let store = DataStore::from_test_data(
            test_states(),
            vec![
                test_snapshot("SH-1", "todo"),
                test_snapshot("SH-2", "in-progress"),
                test_snapshot("SH-3", "todo"),
            ],
            "SH".to_string(),
            vec![],
        );

        let grouped = store.stories_by_state();
        // Only OPEN states: todo, in-progress (not done)
        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped[0].0.slug, "todo");
        assert_eq!(grouped[0].1.len(), 2);
        assert_eq!(grouped[1].0.slug, "in-progress");
        assert_eq!(grouped[1].1.len(), 1);
    }

    #[test]
    fn stories_by_state_excludes_closed() {
        let store = DataStore::from_test_data(
            test_states(),
            vec![
                test_snapshot("SH-1", "done"),
                test_snapshot("SH-2", "todo"),
            ],
            "SH".to_string(),
            vec![],
        );

        let grouped = store.stories_by_state();
        // Only OPEN states shown
        assert_eq!(grouped.len(), 2);
        // "done" stories are excluded from grouping since only OPEN states are shown
        let total_stories: usize = grouped.iter().map(|(_, stories)| stories.len()).sum();
        assert_eq!(total_stories, 1);
    }

    #[test]
    fn find_story_by_id() {
        let store = DataStore::from_test_data(
            test_states(),
            vec![
                test_snapshot("SH-1", "todo"),
                test_snapshot("SH-2", "in-progress"),
            ],
            "SH".to_string(),
            vec![],
        );

        assert!(store.find_story("SH-1").is_some());
        assert_eq!(store.find_story("SH-1").unwrap().state, "todo");
        assert!(store.find_story("SH-99").is_none());
    }

    #[test]
    fn story_count_is_correct() {
        let store = DataStore::from_test_data(
            test_states(),
            vec![
                test_snapshot("SH-1", "todo"),
                test_snapshot("SH-2", "todo"),
                test_snapshot("SH-3", "in-progress"),
            ],
            "SH".to_string(),
            vec![],
        );

        assert_eq!(store.story_count(), 3);
    }

    #[test]
    fn load_from_disk_with_tempdir() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        storage::init_project(root, Some("TEST")).unwrap();

        crate::lock::with_project_lock(root, || {
            storage::create_story(root, "First story")?;
            storage::create_story(root, "Second story")?;
            Ok(())
        })
        .unwrap();

        let store = DataStore::load(root).unwrap();
        assert_eq!(store.story_count(), 2);
        assert_eq!(store.prefix, "TEST");
        assert!(store.find_story("TEST-1").is_some());
        assert!(store.find_story("TEST-2").is_some());
    }

    #[test]
    fn tolerant_read_skips_incomplete_trailing_line() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        storage::init_project(root, Some("SH")).unwrap();

        // Create a story normally
        crate::lock::with_project_lock(root, || {
            storage::create_story(root, "Test story")?;
            Ok(())
        })
        .unwrap();

        // Append an incomplete JSON line (simulating concurrent write)
        let story_path = root.join(".storyhook/open/stories/SH-1.jsonl");
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&story_path)
            .unwrap();
        write!(file, "{{\"kind\":\"StoryCommentAdd").unwrap();

        // Loading should succeed, skipping the incomplete trailing line
        let store = DataStore::load(root).unwrap();
        assert_eq!(store.story_count(), 1);
        assert_eq!(store.find_story("SH-1").unwrap().title, "Test story");
    }
}
