//! Invariants of the project service.
//!
//! `story init` is the one command whose failure mode used to be
//! unrecoverable: the legacy path wrote seven independent files and a SQLite
//! database, and a failure between any two of them left a project that existed
//! enough to be found and not enough to be used. These tests pin the property
//! that replaces it — the project, its catalog and its counters are one
//! transaction — and the repository-side artifacts `init` is still allowed to
//! write.

use std::path::Path;

use storyhook::domain::SuperState;
use storyhook::service::project::{
    DEFAULT_PREFIX, ProjectPointer, closed_state, pointer_path, read_pointer,
};
use storyhook::service::{Clock, InitOptions, ProjectService};
use storyhook::store::{ReadOps, SqliteStore, Store, StoryNo, WriteOps};
use storyhook_test_support::{FIXTURE_NOW, scratch_dir};
use tempfile::TempDir;

// --- helpers ---------------------------------------------------------------

/// A store and a checkout directory, neither of them holding a project yet.
struct Fixture {
    store: SqliteStore,
    root: TempDir,
    _dir: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = scratch_dir();
        let store = SqliteStore::open(dir.path().join("store.db")).expect("opening the store");
        store.migrate().expect("migrating the store");
        Self {
            store,
            root: scratch_dir(),
            _dir: dir,
        }
    }

    fn root(&self) -> &Path {
        self.root.path()
    }

    fn service(&self) -> ProjectService<'_, SqliteStore> {
        ProjectService::new(&self.store, self.root.path()).clock(Clock::Fixed(FIXTURE_NOW.into()))
    }

    fn init(&self, options: &InitOptions) -> storyhook::service::InitOutcome {
        self.service().init(options).expect("initializing")
    }
}

/// `init` with nothing scaffolded — the shape most of these tests want.
fn bare() -> InitOptions {
    InitOptions {
        agents_md: false,
        ..InitOptions::default()
    }
}

// --- the atomic core -------------------------------------------------------

#[test]
fn init_creates_a_project_with_its_catalog_and_counters() {
    let fixture = Fixture::new();
    let outcome = fixture.init(&bare());
    assert!(outcome.created);

    let record = fixture
        .store
        .read(|tx| tx.project(outcome.project))
        .expect("reading the project")
        .expect("the project exists");
    assert_eq!(record.prefix, DEFAULT_PREFIX);
    assert_eq!(record.created_at, FIXTURE_NOW);
    assert_eq!(record.next_story_no, 1, "the id counter starts at 1");

    let states = fixture
        .store
        .read(|tx| tx.states(outcome.project))
        .expect("reading states");
    let slugs: Vec<&str> = states.iter().map(|state| state.slug.as_str()).collect();
    // The floor and the default are the same set (SH-125), so a project is
    // conforming the instant it exists.
    assert_eq!(
        slugs,
        [
            "todo",
            "in-progress",
            "verifying",
            "blocked",
            "done",
            "closed"
        ]
    );
    assert_eq!(states[0].super_state, SuperState::Open);
    assert_eq!(states[1].role.as_deref(), Some("active"));
    assert_eq!(states[2].super_state, SuperState::Open);
    assert_eq!(states[3].super_state, SuperState::Open);
    assert_eq!(states[4].super_state, SuperState::Closed);
    assert_eq!(states[5].super_state, SuperState::Closed);

    let types = fixture
        .store
        .read(|tx| tx.types(outcome.project))
        .expect("reading types");
    let slugs: Vec<&str> = types.iter().map(|t| t.slug.as_str()).collect();
    // SH-157: `task` was a fifth default type; it no longer is. `story` was
    // renamed to `normal`, to stop reading as "a story of type story".
    assert_eq!(slugs, ["normal", "epic", "bug", "chore"]);
    assert!(types.iter().all(|t| t.description.is_some()));
    assert!(
        types.iter().all(|t| t.emoji.is_some()),
        "every default type carries the emoji the web dashboard's badge reads"
    );
}

#[test]
fn the_first_configured_state_is_open_so_a_new_story_has_somewhere_to_go() {
    // The state set is written as an ordered list and read back as one; a
    // catalog whose first OPEN state were `in-progress` would open every new
    // story in the wrong state.
    let fixture = Fixture::new();
    let outcome = fixture.init(&bare());
    let states = fixture
        .store
        .read(|tx| tx.states(outcome.project))
        .expect("reading states");
    let first_open = states
        .iter()
        .find(|state| state.super_state == SuperState::Open)
        .expect("an OPEN state");
    assert_eq!(first_open.slug, "todo");
}

#[test]
fn init_links_the_checkout_to_the_project() {
    // `init_registers_the_checkout_against_the_project` used to be this test,
    // and it asserted a row in the resolution index plus a lookup back through
    // it. Both are gone (SH-119). What an attaching run records about the
    // directory now is one thing — where this project's repo-side work runs —
    // and nothing looks a project up by it.
    let fixture = Fixture::new();
    let outcome = fixture.init(&bare());
    assert_eq!(
        fixture
            .store
            .read(|tx| tx.checkout_path(outcome.project))
            .expect("reading the linked checkout")
            .as_deref(),
        Some(
            fixture
                .root()
                .canonicalize()
                .expect("canonicalizing")
                .as_path()
        )
    );
}

#[cfg(feature = "fault-injection")]
#[test]
fn a_failed_init_leaves_no_half_built_project() {
    use storyhook::store::fault::{FaultAction, FaultPoint, arm};

    let fixture = Fixture::new();
    let _fault = arm(
        FaultPoint::BeforeCommit,
        FaultAction::Fail("init interrupted".to_string()),
    );
    let error = fixture
        .service()
        .init(&bare())
        .expect_err("the injected fault must fail the init");
    assert!(error.to_string().contains("init interrupted"), "{error}");
    drop(_fault);

    let projects = fixture
        .store
        .read(|tx| tx.projects())
        .expect("reading projects");
    assert!(
        projects.is_empty(),
        "an interrupted init left {} project(s) behind",
        projects.len()
    );
}

#[cfg(feature = "fault-injection")]
#[test]
fn an_init_that_fails_can_simply_be_retried() {
    use storyhook::store::fault::{FaultAction, FaultPoint, arm};

    let fixture = Fixture::new();
    {
        let _fault = arm(
            FaultPoint::BeforeCommit,
            FaultAction::Fail("interrupted".to_string()),
        );
        fixture.service().init(&bare()).expect_err("first attempt");
    }
    let outcome = fixture.init(&bare());
    assert!(outcome.created);
    let states = fixture
        .store
        .read(|tx| tx.states(outcome.project))
        .expect("reading states");
    assert_eq!(
        states.len(),
        storyhook::domain::REQUIRED_STATES.len(),
        "the retry produced a complete catalog"
    );
}

// --- idempotence -----------------------------------------------------------

#[test]
fn initializing_twice_does_not_create_a_second_project() {
    let fixture = Fixture::new();
    let first = fixture.init(&bare());
    let second = fixture.init(&bare());
    assert_eq!(first.project, second.project);
    assert!(first.created);
    assert!(!second.created, "the second run found the project");
    assert_eq!(
        fixture
            .store
            .read(|tx| tx.projects())
            .expect("reading projects")
            .len(),
        1
    );
}

#[test]
fn re_initializing_leaves_an_edited_catalog_alone() {
    // `init` is idempotent in the legacy path because it skipped every file it
    // found. The same must hold here: a project whose states were customized
    // must not be reset to the defaults by a second `story init`.
    let fixture = Fixture::new();
    let outcome = fixture.init(&bare());
    fixture
        .store
        .write(|tx| tx.put_types(outcome.project, &[]))
        .expect("emptying the type set");

    fixture.init(&bare());
    let types = fixture
        .store
        .read(|tx| tx.types(outcome.project))
        .expect("reading types");
    assert!(types.is_empty(), "the second init overwrote the catalog");
}

#[test]
fn re_initializing_does_not_change_the_prefix() {
    let fixture = Fixture::new();
    fixture.init(&InitOptions {
        prefix: Some("AB".into()),
        ..bare()
    });
    let outcome = fixture.init(&InitOptions {
        prefix: Some("ZZ".into()),
        ..bare()
    });
    let record = fixture
        .store
        .read(|tx| tx.project(outcome.project))
        .expect("reading the project")
        .expect("the project exists");
    assert_eq!(record.prefix, "AB");
}

// --- identity --------------------------------------------------------------

#[test]
fn a_requested_prefix_becomes_the_projects_story_ids() {
    let fixture = Fixture::new();
    let outcome = fixture.init(&InitOptions {
        prefix: Some("PROJ".into()),
        ..bare()
    });
    let story = fixture
        .store
        .write(|tx| tx.allocate_story_no(outcome.project))
        .expect("allocating");
    assert_eq!(story.to_id("PROJ"), "PROJ-1");
    assert_eq!(StoryNo::parse_id("PROJ", "PROJ-1").unwrap(), story);
}

#[test]
fn two_checkouts_with_the_same_basename_get_distinct_slugs() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).expect("opening");
    store.migrate().expect("migrating");

    let mut ids = Vec::new();
    let mut roots = Vec::new();
    for _ in 0..3 {
        let parent = scratch_dir();
        let root = parent.path().join("app");
        std::fs::create_dir(&root).expect("creating the checkout");
        let outcome = ProjectService::new(&store, &root)
            .clock(Clock::Fixed(FIXTURE_NOW.into()))
            .init(&bare())
            .expect("initializing");
        ids.push(outcome.project);
        roots.push(parent);
    }

    let slugs: Vec<String> = store
        .read(|tx| tx.projects())
        .expect("reading projects")
        .into_iter()
        .map(|record| record.slug)
        .collect();
    assert_eq!(slugs, ["app", "app-2", "app-3"]);
    assert_eq!(ids.len(), 3);
}

#[test]
fn every_project_gets_its_own_portable_identity() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).expect("opening");
    store.migrate().expect("migrating");

    let mut roots = Vec::new();
    for _ in 0..8 {
        let root = scratch_dir();
        ProjectService::new(&store, root.path())
            .clock(Clock::Fixed(FIXTURE_NOW.into()))
            .init(&bare())
            .expect("initializing");
        roots.push(root);
    }

    let uuids: std::collections::BTreeSet<String> = store
        .read(|tx| tx.projects())
        .expect("reading projects")
        .into_iter()
        .map(|record| record.uuid)
        .collect();
    assert_eq!(uuids.len(), 8, "two projects share a uuid");
    assert!(
        uuids.iter().all(|uuid| uuid.len() == 36),
        "a uuid is not in the canonical hyphenated form: {uuids:?}"
    );
}

// --- what reaches the repository -------------------------------------------

#[test]
fn an_attaching_run_always_leaves_the_checkout_carrying_its_identity() {
    // The premise this replaces was `InitOptions::pointer`, a switch whose
    // `false` no caller in `src/` ever set — `StoreInvoker::new` hard-coded
    // `true` — and whose only effect was to leave a project no directory could
    // identify. Once the recorded-path index went (SH-119) that state stopped
    // being merely undesirable and became unrepresentable: an attaching run
    // writes the file, or adopts the one already there.
    //
    // `--no-attach` is the verb for "identified by nothing on this machine",
    // and `tests/project_new.rs` holds it to writing no pointer at all.
    let fixture = Fixture::new();
    let outcome = fixture.init(&bare());
    assert!(outcome.pointer, "an attaching run writes the pointer file");
    assert!(pointer_path(&fixture.root().canonicalize().unwrap()).exists());
    assert!(
        read_pointer(fixture.root())
            .expect("reading the pointer")
            .is_some()
    );
}

#[test]
fn the_pointer_file_names_the_project_it_points_at() {
    let fixture = Fixture::new();
    let outcome = fixture.init(&InitOptions {
        prefix: Some("PT".into()),
        ..bare()
    });
    assert!(outcome.pointer);

    let root = fixture.root().canonicalize().expect("canonicalizing");
    let pointer = read_pointer(&root)
        .expect("reading the pointer")
        .expect("the pointer exists");
    let record = fixture
        .store
        .read(|tx| tx.project(outcome.project))
        .expect("reading the project")
        .expect("the project exists");
    assert_eq!(
        pointer,
        ProjectPointer::new(record.uuid.clone(), "PT".to_string())
    );
    assert_eq!(pointer.schema, 1);
    assert!(
        pointer.plugin.is_none() && pointer.hooks.is_none(),
        "init writes identity and nothing else; the config tables are the user's"
    );

    let by_uuid = fixture
        .store
        .read(|tx| tx.project_by_uuid(&pointer.uuid))
        .expect("resolving the pointer");
    assert_eq!(by_uuid.map(|record| record.id), Some(outcome.project));
}

#[test]
fn agents_md_is_generated_by_default_and_names_this_projects_prefix() {
    let fixture = Fixture::new();
    let outcome = fixture.init(&InitOptions {
        prefix: Some("QQ".into()),
        ..InitOptions::default()
    });
    assert!(outcome.agents_md);

    let root = fixture.root().canonicalize().expect("canonicalizing");
    let content = std::fs::read_to_string(root.join("AGENTS.md")).expect("reading AGENTS.md");
    assert!(content.contains("story show QQ-<n>"), "{content}");
    assert!(content.contains("story move QQ-<n> <state>"), "{content}");
}

#[test]
fn agents_md_names_the_projects_own_closed_state() {
    let fixture = Fixture::new();
    let outcome = fixture.init(&bare());
    fixture
        .store
        .write(|tx| {
            let mut states = tx.states(outcome.project)?;
            let closed = states
                .iter()
                .position(|state| state.super_state == SuperState::Closed)
                .expect("a CLOSED state");
            states[closed].slug = "shipped".to_string();
            tx.put_states(outcome.project, &states)
        })
        .expect("renaming the closed state");
    std::fs::remove_file(fixture.root().canonicalize().unwrap().join("AGENTS.md")).ok();

    fixture.init(&InitOptions::default());
    let content = std::fs::read_to_string(fixture.root().canonicalize().unwrap().join("AGENTS.md"))
        .expect("reading AGENTS.md");
    assert!(
        content.contains("moves the story to `shipped`"),
        "{content}"
    );
}

#[test]
fn agents_md_is_suppressed_on_request() {
    let fixture = Fixture::new();
    let outcome = fixture.init(&bare());
    assert!(!outcome.agents_md);
    assert!(
        !fixture
            .root()
            .canonicalize()
            .unwrap()
            .join("AGENTS.md")
            .exists()
    );
}

#[test]
fn an_existing_agents_md_is_never_overwritten() {
    let fixture = Fixture::new();
    let path = fixture.root().join("AGENTS.md");
    std::fs::write(&path, "hand written").expect("seeding AGENTS.md");

    let outcome = fixture.init(&InitOptions::default());
    assert!(
        !outcome.agents_md,
        "init reported writing a file it skipped"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("reading AGENTS.md"),
        "hand written"
    );
}

#[test]
fn init_writes_nothing_else_into_the_repository() {
    // The design's headline constraint: `init` may scaffold the two artifacts
    // it was asked for and nothing more. A directory tree appearing here is a
    // regression toward the storage model this replaces.
    let fixture = Fixture::new();
    fixture.init(&InitOptions {
        ..InitOptions::default()
    });

    let root = fixture.root().canonicalize().expect("canonicalizing");
    let mut entries: Vec<String> = std::fs::read_dir(&root)
        .expect("listing the checkout")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into()
        })
        .collect();
    entries.sort();
    assert_eq!(entries, [".storyhook.toml", "AGENTS.md"]);
}

// --- the closed-state lookup templates depend on ---------------------------

#[test]
fn a_project_with_no_closed_state_still_renders_a_template() {
    let fixture = Fixture::new();
    let outcome = fixture.init(&bare());
    fixture
        .store
        .write(|tx| {
            let open: Vec<_> = tx
                .states(outcome.project)?
                .into_iter()
                .filter(|state| state.super_state == SuperState::Open)
                .collect();
            tx.put_states(outcome.project, &open)
        })
        .expect("removing every closed state");

    let done = fixture
        .store
        .read(|tx| Ok(closed_state(tx, outcome.project)?))
        .expect("reading the closed state");
    assert_eq!(done, "done", "the template must still render");
}

// --- the pointer file as the repository's config home -----------------------

#[test]
fn a_user_authored_pointer_file_survives_a_second_init() {
    let fixture = Fixture::new();
    fixture.init(&InitOptions { ..bare() });
    let root = fixture.root().canonicalize().expect("canonicalizing");

    // The user adds their hooks by hand, as they always have — storyhook writes
    // this file exactly once and reads it forever after.
    let authored = format!(
        "{}\n[hooks.on_create]\ncommand = \"notify-send $STORY\"\n",
        std::fs::read_to_string(pointer_path(&root)).expect("reading the pointer")
    );
    std::fs::write(pointer_path(&root), &authored).expect("authoring hooks");

    let second = fixture.init(&InitOptions { ..bare() });
    assert!(
        !second.pointer,
        "a second init must report that it wrote no pointer file"
    );
    assert_eq!(
        std::fs::read_to_string(pointer_path(&root)).expect("re-reading the pointer"),
        authored,
        "`story init` is idempotent, and this file is user-authored the moment it \
         carries configuration — overwriting it would delete a user's hooks"
    );
}

#[test]
fn a_broken_config_table_does_not_make_the_repository_unresolvable() {
    // The identity fields and the config tables are parsed by one serde call,
    // so the config tables are `toml::Value`: anything well-formed parses, and
    // only a syntactically broken *file* can fail. A typed `[hooks]` table
    // would mean a misspelled timeout stopped storyhook knowing which project
    // it was standing in.
    let fixture = Fixture::new();
    let outcome = fixture.init(&InitOptions { ..bare() });
    let root = fixture.root().canonicalize().expect("canonicalizing");
    let mut text = std::fs::read_to_string(pointer_path(&root)).expect("reading the pointer");
    text.push_str("\n[hooks]\ntimeout_seconds = \"not a number\"\nnonsense = [1, 2]\n");
    std::fs::write(pointer_path(&root), text).expect("breaking the hooks table");

    let pointer = read_pointer(&root)
        .expect("a pointer with an odd hooks table must still parse")
        .expect("the pointer exists");
    let record = fixture
        .store
        .read(|tx| tx.project(outcome.project))
        .expect("reading the project")
        .expect("the project exists");
    assert_eq!(pointer.uuid, record.uuid);
    assert!(pointer.hooks.is_some());
}

#[test]
fn a_clone_carrying_a_pointer_is_adopted_rather_than_given_a_second_project() {
    let fixture = Fixture::new();
    let first = fixture.init(&InitOptions { ..bare() });
    let root = fixture.root().canonicalize().expect("canonicalizing");
    let pointer = std::fs::read_to_string(pointer_path(&root)).expect("reading the pointer");

    // A second checkout of the same repository: a path the store has never
    // seen, carrying the committed pointer file.
    let clone = scratch_dir();
    std::fs::write(pointer_path(clone.path()), pointer).expect("committing the pointer");
    let second = ProjectService::new(&fixture.store, clone.path())
        .clock(Clock::Fixed(FIXTURE_NOW.to_string()))
        .init(&InitOptions { ..bare() })
        .expect("initializing the clone");

    assert_eq!(
        second.project, first.project,
        "resolving by path alone would mint a second project for a repository \
         that already has one, leaving the clone pointing at the old identity \
         and storing into the new"
    );
    assert!(!second.created);
    assert_eq!(
        fixture
            .store
            .read(|tx| tx.checkout_path(first.project))
            .expect("reading the linked checkout")
            .as_deref(),
        Some(root.as_path()),
        "adopting a clone must not re-point the project at it: `adopt_checkout` \
         fills a gap and never replaces"
    );
}
