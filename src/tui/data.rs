use std::collections::BTreeMap;

use crate::cli::Invocation;
use crate::domain::{self, Member, StateDef, StorySnapshot, SuperState};
use crate::error::AppError;
use crate::invoke::{InvokeRequest, Invoker};
use crate::output::{ProjectSnapshotView, Response};
use crate::store::GlobalSeq;

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
    /// The project's unpublished drafts, carried beside `stories` rather than
    /// in it.
    ///
    /// The TUI never *displays* one — the snapshot deliberately keeps drafts
    /// out of `stories` (SH-175). They are here because a draft can still be
    /// somebody's blocker, and a `blocked-by` edge pointing at a story the
    /// index cannot answer for reads as "not blocking" ([`domain::is_ready`]).
    /// Without them the board would call a story ready that the CLI calls
    /// blocked — the same disagreement SH-240 is about, one indirection out.
    pub drafts: Vec<StorySnapshot>,
    pub prefix: String,
    pub members: Vec<Member>,
    /// Each story's `head_global_seq` (SH-336), keyed by id, covering
    /// `stories` and `drafts` alike — the exact recency tiebreak
    /// `recent_stories` needs. Not on `StorySnapshot` itself: that type is
    /// the fold of a story's events and nothing else. Empty when the daemon
    /// that answered predates the field; every reader treats a missing id
    /// the same way, falling back to the incoming order.
    pub head_global_seqs: BTreeMap<String, GlobalSeq>,
}

/// The cross-story context the readiness predicates need, resolved once for a
/// project a caller already holds.
///
/// Exists so that "is this story ready?" cannot be asked with a story list
/// from one project and a state catalog from another, and so that the answer
/// stays [`domain::is_ready`]'s — this type routes to it, it does not restate
/// it. A `DataStore` builds one with [`DataStore::readiness`]; the index
/// borrows, so building it is pointers rather than a clone of the project.
pub struct Readiness<'a> {
    stories: BTreeMap<&'a str, &'a StorySnapshot>,
    active: Option<StateDef>,
}

impl<'a> Readiness<'a> {
    /// Indexes `stories` by id and resolves the project's active state — the
    /// one a claim moves a story into ([`domain::active_state`]).
    pub fn new(stories: impl IntoIterator<Item = &'a StorySnapshot>, states: &[StateDef]) -> Self {
        Self {
            stories: stories
                .into_iter()
                .map(|story| (story.id.as_str(), story))
                .collect(),
            active: domain::active_state(states),
        }
    }

    /// Whether `story` is unblocked — nothing is stopping work on it.
    /// Says nothing about whether someone is already doing that work; that is
    /// [`Self::is_claimable`], and `story list --blocked` draws the same
    /// distinction.
    pub fn is_ready(&self, story: &StorySnapshot) -> bool {
        domain::is_ready(story, &self.stories)
    }

    /// Whether `story` is ready *and* unclaimed — what "ready to pick up"
    /// means to `story next` and `story list --ready`.
    pub fn is_claimable(&self, story: &StorySnapshot) -> bool {
        domain::is_claimable(story, &self.stories, self.active.as_ref())
    }
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
            drafts: view.drafts,
            prefix: view.prefix,
            members: view.members,
            head_global_seqs: view.head_global_seqs,
        }
    }

    /// The readiness context for this project: every story it carries,
    /// drafts included, indexed by id, plus the active state.
    pub fn readiness(&self) -> Readiness<'_> {
        Readiness::new(self.stories.iter().chain(self.drafts.iter()), &self.states)
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
            drafts: Vec::new(),
            prefix,
            members,
            head_global_seqs: BTreeMap::new(),
        }
    }

    /// The same, with drafts — only a test that asks what a *draft* blocker
    /// does needs them, so they stay off the main constructor's signature.
    #[cfg(test)]
    pub fn with_drafts(mut self, drafts: Vec<StorySnapshot>) -> Self {
        self.drafts = drafts;
        self
    }

    /// The same, with `head_global_seq` values — only a test that asks about
    /// the SH-336 recency tiebreak needs them, so they stay off the main
    /// constructor's signature the same way `with_drafts` does.
    #[cfg(test)]
    pub fn with_head_global_seqs(mut self, seqs: BTreeMap<String, GlobalSeq>) -> Self {
        self.head_global_seqs = seqs;
        self
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

    /// The project's stories that match every filter in `filters`.
    ///
    /// A method rather than a free function over a story list, because two of
    /// the filters — `ready` and `blocked` — are questions about a story's
    /// place in the project, not about the story alone: they need the
    /// `blocked-by` graph and the state catalog. Taking those as extra
    /// arguments would let a caller pass one project's filters over another
    /// project's graph.
    pub fn filter(&self, filters: &[FilterSpec]) -> Vec<&StorySnapshot> {
        if filters.is_empty() {
            return self.stories.iter().collect();
        }
        let readiness = self.readiness();
        self.stories
            .iter()
            .filter(|story| {
                filters
                    .iter()
                    .all(|filter| matches_filter(filter, story, &readiness))
            })
            .collect()
    }

    /// Stories grouped by state, in state definition order, filtered by active filters.
    /// Only includes states with OPEN superstates.
    pub fn filtered_stories_by_state(
        &self,
        filters: &[FilterSpec],
    ) -> Vec<(&StateDef, Vec<&StorySnapshot>)> {
        let filtered = self.filter(filters);
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

/// Check whether a single story matches a single filter spec, given the
/// project it sits in.
fn matches_filter(filter: &FilterSpec, story: &StorySnapshot, readiness: &Readiness<'_>) -> bool {
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

    // The `blocked` and `ready` chips claim what `story list --blocked` and
    // `story list --ready` claim, so they answer with the same predicates.
    // Both used to test `awaiting` alone, which is one of five ways a story
    // can be stuck and no way at all of telling claimed work from free work
    // (SH-240).
    if filter.blocked && (story.superstate != SuperState::Open || readiness.is_ready(story)) {
        return false;
    }

    if filter.ready && !readiness.is_claimable(story) {
        return false;
    }

    true
}

// TODO(rearch): migrate to storyhook_test_support::scratch_dir — see clippy.toml.
#[allow(clippy::disallowed_methods)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Priority, StoryRelation};

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

    /// A project holding `stories` under the standard catalog — the shape a
    /// filter question has to be asked of, since `ready` and `blocked` are
    /// questions about a story's place in a project.
    fn project(stories: Vec<StorySnapshot>) -> DataStore {
        DataStore::from_test_data(test_states(), stories, "SH".to_string(), vec![])
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
            referenced_by_commits: vec![],
            relationships: vec![],
            priority: Priority::None,
            priority_assessed: false,
            labels: vec![],
            story_type: None,
            description: None,
            closed_at: None,
            deleted: false,
            deleted_reason: None,
            hidden_at: None,
            draft: false,
            attachments: Vec::new(),
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
                action: crate::cli::ProjectAction::New(crate::cli::NewProjectRequest::Stated(
                    crate::cli::NewProjectSpec {
                        attach: crate::cli::Attach::Cwd,
                        prefix: prefix.to_string(),
                        name: None,
                        no_agents_md: true,
                    },
                )),
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
                    draft: false,
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
            referenced_by_commits: vec![],
            relationships: vec![],
            priority_assessed: priority != Priority::None,
            priority,
            labels: labels.into_iter().map(|s| s.to_string()).collect(),
            story_type: None,
            description: None,
            closed_at: None,
            deleted: false,
            deleted_reason: None,
            hidden_at: None,
            draft: false,
            attachments: Vec::new(),
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
        let store = project(stories.clone());
        let result = store.filter(&filters);
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
        let store = project(stories.clone());
        let result = store.filter(&filters);
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
        let store = project(stories.clone());
        let result = store.filter(&filters);
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
        let store = project(stories.clone());
        let result = store.filter(&filters);
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
        let store = project(stories.clone());
        let result = store.filter(&filters);
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
        let store = project(stories.clone());
        let result = store.filter(&filters);
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
        let store = project(stories.clone());
        let result = store.filter(&filters);
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
        let store = project(stories.clone());
        let result = store.filter(&filters);
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
        let store = project(stories.clone());
        let result = store.filter(&filters);
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
        let store = project(stories.clone());
        let result = store.filter(&filters);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "SH-2");
    }

    /// The board's `ready` chip claims the same thing `story list --ready`
    /// does, so it answers with the same predicate: a claimed story is
    /// already someone's work (SH-236).
    #[test]
    fn ready_filter_excludes_a_claimed_story() {
        let stories = vec![
            make_rich_snapshot(
                "SH-1",
                "in-progress",
                "Claimed",
                Priority::None,
                vec![],
                None,
                None,
            ),
            make_rich_snapshot("SH-2", "todo", "Free", Priority::None, vec![], None, None),
        ];
        let filters = vec![FilterSpec {
            ready: true,
            ..Default::default()
        }];
        let store = project(stories.clone());
        let result = store.filter(&filters);
        assert_eq!(
            result.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["SH-2"]
        );
    }

    #[test]
    fn ready_filter_excludes_a_story_in_the_blocked_state() {
        let stories = vec![
            make_rich_snapshot(
                "SH-1",
                "blocked",
                "Parked",
                Priority::None,
                vec![],
                None,
                None,
            ),
            make_rich_snapshot("SH-2", "todo", "Free", Priority::None, vec![], None, None),
        ];
        let filters = vec![FilterSpec {
            ready: true,
            ..Default::default()
        }];
        let store = project(stories.clone());
        let result = store.filter(&filters);
        assert_eq!(
            result.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["SH-2"]
        );
    }

    #[test]
    fn ready_filter_excludes_a_story_blocked_by_an_open_dependency() {
        let mut dependent = make_rich_snapshot(
            "SH-2",
            "todo",
            "Dependent",
            Priority::None,
            vec![],
            None,
            None,
        );
        dependent.relationships.push(StoryRelation {
            relation: "blocked-by".to_string(),
            other_id: "SH-1".to_string(),
        });
        let stories = vec![
            make_rich_snapshot(
                "SH-1",
                "todo",
                "Blocker",
                Priority::None,
                vec![],
                None,
                None,
            ),
            dependent,
        ];
        let filters = vec![FilterSpec {
            ready: true,
            ..Default::default()
        }];
        let store = project(stories.clone());
        let result = store.filter(&filters);
        assert_eq!(
            result.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["SH-1"]
        );
    }

    /// `story list --blocked` means "open, and not ready" — every way a story
    /// can be stuck, not just an `awaiting` reason. The chip meant only the
    /// last of those, so the two halves of the same board disagreed about
    /// which stories were blocked.
    #[test]
    fn blocked_filter_covers_every_way_a_story_is_stuck() {
        let mut dependent = make_rich_snapshot(
            "SH-3",
            "todo",
            "Dependent",
            Priority::None,
            vec![],
            None,
            None,
        );
        dependent.relationships.push(StoryRelation {
            relation: "blocked-by".to_string(),
            other_id: "SH-4".to_string(),
        });
        let stories = vec![
            make_rich_snapshot(
                "SH-1",
                "todo",
                "Awaiting",
                Priority::None,
                vec![],
                None,
                Some("waiting for deploy"),
            ),
            make_rich_snapshot(
                "SH-2",
                "blocked",
                "Parked",
                Priority::None,
                vec![],
                None,
                None,
            ),
            dependent,
            make_rich_snapshot(
                "SH-4",
                "todo",
                "Blocker",
                Priority::None,
                vec![],
                None,
                None,
            ),
        ];
        let filters = vec![FilterSpec {
            blocked: true,
            ..Default::default()
        }];
        let store = project(stories.clone());
        let result = store.filter(&filters);
        assert_eq!(
            result.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["SH-1", "SH-2", "SH-3"],
            "the blocker itself is not blocked"
        );
    }

    /// A closed story is not blocked, whatever it was awaiting when it
    /// closed — `story list --blocked` filters to OPEN first.
    #[test]
    fn blocked_filter_excludes_a_closed_story() {
        let mut closed = make_rich_snapshot(
            "SH-1",
            "done",
            "Finished",
            Priority::None,
            vec![],
            None,
            Some("waiting for deploy"),
        );
        closed.superstate = SuperState::Closed;
        let filters = vec![FilterSpec {
            blocked: true,
            ..Default::default()
        }];
        let stories = vec![closed];
        let store = project(stories.clone());
        let result = store.filter(&filters);
        assert!(result.is_empty());
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
        let store = project(stories.clone());
        let result = store.filter(&filters);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "SH-1");
    }

    #[test]
    fn empty_filters_return_all() {
        let stories = vec![
            make_rich_snapshot("SH-1", "todo", "First", Priority::None, vec![], None, None),
            make_rich_snapshot("SH-2", "todo", "Second", Priority::None, vec![], None, None),
        ];
        let store = project(stories.clone());
        let result = store.filter(&[]);
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
        let store = project(stories.clone());
        let result = store.filter(&filters);
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
        // Not a coincidence of the fixture: `blocked` is "open and not
        // ready", `ready` is "ready and unclaimed", so the two chips are
        // disjoint by construction for any project.
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
        let store = project(stories.clone());
        let result = store.filter(&filters);
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
        let store = project(Vec::new());
        let result = store.filter(&filters);
        assert!(result.is_empty());
    }
}
