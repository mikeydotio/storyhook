use std::collections::BTreeMap;

use crate::cli::Invocation;
use crate::domain::{Member, StateDef, StorySnapshot, SuperState};
use crate::error::AppError;
use crate::invoke::{InvokeRequest, Invoker};
use crate::output::{ProjectSnapshotView, Response};

use super::action::FilterSpec;

/// The project as the TUI holds it: one [`Invocation::ProjectSnapshot`],
/// unpacked.
///
/// Rebuilt on startup and after every change. It is a *snapshot* in the
/// literal sense — every field comes from the same read, so the board can
/// never draw a catalog from one instant beside stories from another, which
/// the five separate filesystem reads this replaced could do.
pub struct DataStore {
    pub states: Vec<StateDef>,
    pub state_map: BTreeMap<String, StateDef>,
    pub stories: Vec<StorySnapshot>,
    pub prefix: String,
    pub members: Vec<Member>,
}

impl DataStore {
    /// Loads the whole project through the seam, in one invocation.
    pub fn load(invoker: &dyn Invoker) -> Result<Self, AppError> {
        match invoker.invoke(InvokeRequest::new(Invocation::ProjectSnapshot))? {
            Response::ProjectSnapshot(view) => Ok(Self::from_snapshot(*view)),
            other => Err(AppError::Storage(format!(
                "internal: a project snapshot answered with {other:?}"
            ))),
        }
    }

    /// Unpacks a snapshot, deriving the state map from the state *list* so the
    /// two cannot disagree.
    fn from_snapshot(view: ProjectSnapshotView) -> Self {
        let state_map = view
            .states
            .iter()
            .map(|state| (state.slug.clone(), state.clone()))
            .collect();
        Self {
            states: view.states,
            state_map,
            stories: view.stories,
            prefix: view.prefix,
            members: view.members,
        }
    }

    /// Construct a DataStore from pre-built data, for testing without filesystem.
    #[cfg(test)]
    pub fn from_test_data(
        states: Vec<StateDef>,
        stories: Vec<StorySnapshot>,
        prefix: String,
        members: Vec<Member>,
    ) -> Self {
        let state_map = states.iter().map(|s| (s.slug.clone(), s.clone())).collect();
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

    /// Resolve a typed assignee (member id or GitHub handle) against the
    /// loaded members, mirroring the matching rule the assignment invocations
    /// use.
    ///
    /// Operates entirely in memory so TUI components can validate user input
    /// before dispatching a mutation, without touching the filesystem.
    pub fn find_member(&self, lookup: &str) -> Option<&Member> {
        self.members
            .iter()
            .find(|member| member.id == lookup || member.github.as_deref() == Some(lookup))
    }

    /// Total number of open stories.
    pub fn story_count(&self) -> usize {
        self.stories.len()
    }

    /// Stories grouped by state, in state definition order, filtered by active filters.
    /// Only includes states with OPEN superstates.
    pub fn filtered_stories_by_state(
        &self,
        filters: &[FilterSpec],
    ) -> Vec<(&StateDef, Vec<&StorySnapshot>)> {
        let filtered = apply_filters(filters, &self.stories);
        self.states
            .iter()
            .filter(|state| state.super_state == SuperState::Open)
            .map(|state| {
                let matching: Vec<&StorySnapshot> = filtered
                    .iter()
                    .filter(|story| story.state == state.slug)
                    .copied()
                    .collect();
                (state, matching)
            })
            .collect()
    }
}

/// Apply a set of filters to a list of stories.
///
/// Multiple filters AND together: a story must match ALL filters to be included.
/// Returns references to matching stories.
pub fn apply_filters<'a>(
    filters: &[FilterSpec],
    stories: &'a [StorySnapshot],
) -> Vec<&'a StorySnapshot> {
    if filters.is_empty() {
        return stories.iter().collect();
    }
    stories
        .iter()
        .filter(|story| filters.iter().all(|f| matches_filter(f, story)))
        .collect()
}

/// Check whether a single story matches a single filter spec.
fn matches_filter(filter: &FilterSpec, story: &StorySnapshot) -> bool {
    // Text filter: case-insensitive substring match on title or id
    if let Some(ref text) = filter.text {
        let lower = text.to_ascii_lowercase();
        let title_match = story.title.to_ascii_lowercase().contains(&lower);
        let id_match = story.id.to_ascii_lowercase().contains(&lower);
        if !title_match && !id_match {
            return false;
        }
    }

    // State filter: exact match on state slug
    if let Some(ref state) = filter.state
        && story.state != *state
    {
        return false;
    }

    // Assignee filter: exact match
    if let Some(ref assignee) = filter.assignee {
        match &story.assignee {
            Some(a) if a == assignee => {}
            _ => return false,
        }
    }

    // Priority filter: exact match
    if let Some(ref priority) = filter.priority
        && story.priority != *priority
    {
        return false;
    }

    // Label filter: any label matches (case-insensitive)
    if let Some(ref label) = filter.label {
        let lower_label = label.to_ascii_lowercase();
        let has_label = story
            .labels
            .iter()
            .any(|l| l.to_ascii_lowercase() == lower_label);
        if !has_label {
            return false;
        }
    }

    // Blocked filter: story has awaiting set
    if filter.blocked && story.awaiting.is_none() {
        return false;
    }

    // Ready filter: story has NO awaiting set
    if filter.ready && story.awaiting.is_some() {
        return false;
    }

    true
}

// TODO(rearch): migrate to storyhook_test_support::scratch_dir — see clippy.toml.
#[allow(clippy::disallowed_methods)]
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
            story_type: None,
            description: None,
            closed_at: None,
            deleted: false,
            deleted_reason: None,
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
            vec![test_snapshot("SH-1", "done"), test_snapshot("SH-2", "todo")],
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

    fn test_member(id: &str, github: Option<&str>) -> Member {
        Member {
            id: id.to_string(),
            display_name: id.to_string(),
            email: None,
            github: github.map(|g| g.to_string()),
            created_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn find_member_by_id() {
        let store = DataStore::from_test_data(
            test_states(),
            vec![],
            "SH".to_string(),
            vec![test_member("mikey", Some("mikeyward"))],
        );

        let found = store.find_member("mikey").expect("should find by id");
        assert_eq!(found.id, "mikey");
    }

    #[test]
    fn find_member_by_github_handle() {
        let store = DataStore::from_test_data(
            test_states(),
            vec![],
            "SH".to_string(),
            vec![test_member("mikey", Some("mikeyward"))],
        );

        let found = store
            .find_member("mikeyward")
            .expect("should find by github handle");
        assert_eq!(found.id, "mikey");
    }

    #[test]
    fn find_member_unknown_lookup_returns_none() {
        let store = DataStore::from_test_data(
            test_states(),
            vec![],
            "SH".to_string(),
            vec![test_member("mikey", Some("mikeyward"))],
        );

        assert!(store.find_member("nobody").is_none());
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

    /// A store, a checkout, and a project in it.
    ///
    /// The store's directory is a fixture of its own rather than the machine's:
    /// these are in-process tests, and an in-process test cannot redirect
    /// `STORYHOOK_DATA_DIR` for itself.
    struct Fixture {
        store: crate::store::SqliteStore,
        root: std::path::PathBuf,
        env: crate::env::Environment,
        _data: tempfile::TempDir,
        _repo: tempfile::TempDir,
    }

    impl Fixture {
        fn invoker(&self) -> crate::invoke::StoreInvoker<'_, crate::store::SqliteStore> {
            crate::invoke::StoreInvoker::new(&self.store, &self.root, self.env.clone())
        }
    }

    /// A project with `prefix`, built through the seam.
    fn seeded_project(prefix: &str, titles: &[&str]) -> Fixture {
        use crate::store::Store as _;
        let data = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let env = crate::env::Environment::at(data.path());
        let store = crate::store::SqliteStore::open(env.store_path()).unwrap();
        store.migrate().unwrap();
        let fixture = Fixture {
            store,
            root: repo.path().to_path_buf(),
            env,
            _data: data,
            _repo: repo,
        };
        let invoker = fixture.invoker();
        invoker
            .invoke(InvokeRequest::new(Invocation::Project {
                action: crate::cli::ProjectAction::Init {
                    path: None,
                    prefix: Some(prefix.to_string()),
                    name: None,
                    no_agents_md: true,
                },
            }))
            .unwrap();
        for title in titles {
            invoker
                .invoke(InvokeRequest::new(Invocation::New {
                    title: (*title).to_string(),
                    state: None,
                    story_type: None,
                    description: None,
                    priority: None,
                    labels: None,
                    assignee: None,
                }))
                .unwrap();
        }
        drop(invoker);
        fixture
    }

    #[test]
    fn load_from_disk_with_tempdir() {
        let fixture = seeded_project("TEST", &["First story", "Second story"]);

        let store = DataStore::load(&fixture.invoker()).unwrap();
        assert_eq!(store.story_count(), 2);
        assert_eq!(store.prefix, "TEST");
        assert!(store.find_story("TEST-1").is_some());
        assert!(store.find_story("TEST-2").is_some());
    }

    #[test]
    fn a_torn_trailing_write_is_no_longer_representable() {
        // This case used to append an incomplete JSON line to a story's log,
        // simulating a concurrent write caught mid-flush, and asserted that the
        // reader skipped it. **The failure mode is gone**: a story's events are
        // rows inside a transaction now, and SQLite cannot expose a half-written
        // one — so there is nothing to tolerate and nothing to fabricate.
        //
        // What survives is the claim the tolerance existed to protect: a load
        // that races a writer still returns a complete, coherent project.
        let fixture = seeded_project("SH", &["Test story"]);

        let store = DataStore::load(&fixture.invoker()).unwrap();
        assert_eq!(store.story_count(), 1);
        assert_eq!(store.find_story("SH-1").unwrap().title, "Test story");
    }

    // --- Task 4.2: Filter application tests ---

    fn make_rich_snapshot(
        id: &str,
        state: &str,
        title: &str,
        priority: Priority,
        labels: Vec<&str>,
        assignee: Option<&str>,
        awaiting: Option<&str>,
    ) -> StorySnapshot {
        StorySnapshot {
            id: id.to_string(),
            title: title.to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            state: state.to_string(),
            superstate: SuperState::Open,
            assignee: assignee.map(|s| s.to_string()),
            awaiting: awaiting.map(|s| s.to_string()),
            comments: vec![],
            relationships: vec![],
            priority,
            labels: labels.into_iter().map(|s| s.to_string()).collect(),
            story_type: None,
            description: None,
            closed_at: None,
            deleted: false,
            deleted_reason: None,
        }
    }

    #[test]
    fn filter_by_text_matches_title() {
        let stories = vec![
            make_rich_snapshot(
                "SH-1",
                "todo",
                "Fix login flow",
                Priority::None,
                vec![],
                None,
                None,
            ),
            make_rich_snapshot(
                "SH-2",
                "todo",
                "Add search",
                Priority::None,
                vec![],
                None,
                None,
            ),
        ];
        let filters = vec![FilterSpec {
            text: Some("login".to_string()),
            ..Default::default()
        }];
        let result = apply_filters(&filters, &stories);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "SH-1");
    }

    #[test]
    fn filter_by_text_matches_id() {
        let stories = vec![
            make_rich_snapshot("SH-1", "todo", "First", Priority::None, vec![], None, None),
            make_rich_snapshot("SH-2", "todo", "Second", Priority::None, vec![], None, None),
        ];
        let filters = vec![FilterSpec {
            text: Some("SH-2".to_string()),
            ..Default::default()
        }];
        let result = apply_filters(&filters, &stories);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "SH-2");
    }

    #[test]
    fn filter_by_text_is_case_insensitive() {
        let stories = vec![make_rich_snapshot(
            "SH-1",
            "todo",
            "Fix Login Flow",
            Priority::None,
            vec![],
            None,
            None,
        )];
        let filters = vec![FilterSpec {
            text: Some("fix login".to_string()),
            ..Default::default()
        }];
        let result = apply_filters(&filters, &stories);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn filter_by_state() {
        let stories = vec![
            make_rich_snapshot("SH-1", "todo", "First", Priority::None, vec![], None, None),
            make_rich_snapshot(
                "SH-2",
                "in-progress",
                "Second",
                Priority::None,
                vec![],
                None,
                None,
            ),
        ];
        let filters = vec![FilterSpec {
            state: Some("todo".to_string()),
            ..Default::default()
        }];
        let result = apply_filters(&filters, &stories);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "SH-1");
    }

    #[test]
    fn filter_by_assignee() {
        let stories = vec![
            make_rich_snapshot(
                "SH-1",
                "todo",
                "First",
                Priority::None,
                vec![],
                Some("mikey"),
                None,
            ),
            make_rich_snapshot(
                "SH-2",
                "todo",
                "Second",
                Priority::None,
                vec![],
                Some("bob"),
                None,
            ),
            make_rich_snapshot("SH-3", "todo", "Third", Priority::None, vec![], None, None),
        ];
        let filters = vec![FilterSpec {
            assignee: Some("mikey".to_string()),
            ..Default::default()
        }];
        let result = apply_filters(&filters, &stories);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "SH-1");
    }

    #[test]
    fn filter_by_priority() {
        let stories = vec![
            make_rich_snapshot("SH-1", "todo", "First", Priority::High, vec![], None, None),
            make_rich_snapshot("SH-2", "todo", "Second", Priority::Low, vec![], None, None),
            make_rich_snapshot("SH-3", "todo", "Third", Priority::High, vec![], None, None),
        ];
        let filters = vec![FilterSpec {
            priority: Some(Priority::High),
            ..Default::default()
        }];
        let result = apply_filters(&filters, &stories);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].id, "SH-1");
        assert_eq!(result[1].id, "SH-3");
    }

    #[test]
    fn filter_by_label() {
        let stories = vec![
            make_rich_snapshot(
                "SH-1",
                "todo",
                "First",
                Priority::None,
                vec!["bug", "tui"],
                None,
                None,
            ),
            make_rich_snapshot(
                "SH-2",
                "todo",
                "Second",
                Priority::None,
                vec!["feature"],
                None,
                None,
            ),
            make_rich_snapshot("SH-3", "todo", "Third", Priority::None, vec![], None, None),
        ];
        let filters = vec![FilterSpec {
            label: Some("bug".to_string()),
            ..Default::default()
        }];
        let result = apply_filters(&filters, &stories);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "SH-1");
    }

    #[test]
    fn filter_by_label_is_case_insensitive() {
        let stories = vec![make_rich_snapshot(
            "SH-1",
            "todo",
            "First",
            Priority::None,
            vec!["BUG"],
            None,
            None,
        )];
        let filters = vec![FilterSpec {
            label: Some("bug".to_string()),
            ..Default::default()
        }];
        let result = apply_filters(&filters, &stories);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn filter_by_blocked() {
        let stories = vec![
            make_rich_snapshot(
                "SH-1",
                "todo",
                "First",
                Priority::None,
                vec![],
                None,
                Some("waiting for deploy"),
            ),
            make_rich_snapshot("SH-2", "todo", "Second", Priority::None, vec![], None, None),
        ];
        let filters = vec![FilterSpec {
            blocked: true,
            ..Default::default()
        }];
        let result = apply_filters(&filters, &stories);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "SH-1");
    }

    #[test]
    fn filter_by_ready() {
        let stories = vec![
            make_rich_snapshot(
                "SH-1",
                "todo",
                "First",
                Priority::None,
                vec![],
                None,
                Some("waiting"),
            ),
            make_rich_snapshot("SH-2", "todo", "Second", Priority::None, vec![], None, None),
        ];
        let filters = vec![FilterSpec {
            ready: true,
            ..Default::default()
        }];
        let result = apply_filters(&filters, &stories);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "SH-2");
    }

    #[test]
    fn multiple_filters_and_together() {
        let stories = vec![
            make_rich_snapshot(
                "SH-1",
                "todo",
                "Fix login",
                Priority::High,
                vec!["bug"],
                Some("mikey"),
                None,
            ),
            make_rich_snapshot(
                "SH-2",
                "todo",
                "Fix signup",
                Priority::High,
                vec!["bug"],
                Some("bob"),
                None,
            ),
            make_rich_snapshot(
                "SH-3",
                "todo",
                "Add search",
                Priority::Low,
                vec!["feature"],
                Some("mikey"),
                None,
            ),
        ];
        // state:todo AND p:high AND @mikey
        let filters = vec![
            FilterSpec {
                state: Some("todo".to_string()),
                ..Default::default()
            },
            FilterSpec {
                priority: Some(Priority::High),
                ..Default::default()
            },
            FilterSpec {
                assignee: Some("mikey".to_string()),
                ..Default::default()
            },
        ];
        let result = apply_filters(&filters, &stories);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "SH-1");
    }

    #[test]
    fn empty_filters_return_all() {
        let stories = vec![
            make_rich_snapshot("SH-1", "todo", "First", Priority::None, vec![], None, None),
            make_rich_snapshot("SH-2", "todo", "Second", Priority::None, vec![], None, None),
        ];
        let result = apply_filters(&[], &stories);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn no_match_returns_empty() {
        let stories = vec![make_rich_snapshot(
            "SH-1",
            "todo",
            "First",
            Priority::None,
            vec![],
            None,
            None,
        )];
        let filters = vec![FilterSpec {
            assignee: Some("nobody".to_string()),
            ..Default::default()
        }];
        let result = apply_filters(&filters, &stories);
        assert!(result.is_empty());
    }

    // =======================================================================
    // QA: filtered_stories_by_state edge cases
    // =======================================================================

    #[test]
    fn filtered_stories_by_state_empty_project() {
        let store = DataStore::from_test_data(test_states(), vec![], "SH".to_string(), vec![]);
        let result = store.filtered_stories_by_state(&[]);
        // Should still have 2 open state groups (todo, in-progress) with 0 stories each
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].1.len(), 0);
        assert_eq!(result[1].1.len(), 0);
    }

    #[test]
    fn filtered_stories_by_state_filter_removes_all() {
        let store = DataStore::from_test_data(
            test_states(),
            vec![
                test_snapshot("SH-1", "todo"),
                test_snapshot("SH-2", "in-progress"),
            ],
            "SH".to_string(),
            vec![],
        );
        let filters = vec![FilterSpec {
            text: Some("ZZZZZ_NO_MATCH".to_string()),
            ..Default::default()
        }];
        let result = store.filtered_stories_by_state(&filters);
        // State groups still present, but all empty
        assert_eq!(result.len(), 2);
        let total: usize = result.iter().map(|(_, s)| s.len()).sum();
        assert_eq!(total, 0);
    }

    #[test]
    fn filtered_stories_by_state_preserves_state_order() {
        let store = DataStore::from_test_data(
            test_states(),
            vec![
                test_snapshot("SH-1", "in-progress"),
                test_snapshot("SH-2", "todo"),
            ],
            "SH".to_string(),
            vec![],
        );
        let result = store.filtered_stories_by_state(&[]);
        // Order is defined by states definition, not story insertion
        assert_eq!(result[0].0.slug, "todo");
        assert_eq!(result[1].0.slug, "in-progress");
    }

    #[test]
    fn empty_data_store_default() {
        let store = DataStore::default();
        assert_eq!(store.story_count(), 0);
        assert!(store.find_story("anything").is_none());
        assert!(store.stories_by_state().is_empty());
        assert!(store.filtered_stories_by_state(&[]).is_empty());
    }

    // =======================================================================
    // QA: blocked + ready filter conflict
    // =======================================================================

    #[test]
    fn blocked_and_ready_together_matches_nothing() {
        // A story can't be both blocked (awaiting.is_some()) and ready (awaiting.is_none())
        let stories = vec![
            make_rich_snapshot(
                "SH-1",
                "todo",
                "A",
                Priority::None,
                vec![],
                None,
                Some("waiting"),
            ),
            make_rich_snapshot("SH-2", "todo", "B", Priority::None, vec![], None, None),
        ];
        let filters = vec![
            FilterSpec {
                blocked: true,
                ..Default::default()
            },
            FilterSpec {
                ready: true,
                ..Default::default()
            },
        ];
        let result = apply_filters(&filters, &stories);
        assert!(result.is_empty(), "blocked AND ready should match nothing");
    }

    // =======================================================================
    // QA: Filter on empty story collections
    // =======================================================================

    #[test]
    fn apply_filters_on_empty_stories() {
        let filters = vec![FilterSpec {
            text: Some("anything".to_string()),
            ..Default::default()
        }];
        let result = apply_filters(&filters, &[]);
        assert!(result.is_empty());
    }
}
