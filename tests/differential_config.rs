//! Legacy versus store for the project, configuration and system families.
//!
//! Same harness, same rule, as `differential_lifecycle.rs`: one `Invocation`
//! driven through `app::run` on a `.storyhook` project and through
//! `invoke::dispatch` on a store-backed project seeded from the same catalog,
//! with the two answers compared verbatim apart from timestamps.
//!
//! Two families need more than a response comparison.
//!
//! **`init` has no comparable fixture.** Both legs need a directory with no
//! project in it, which is exactly what the shared harness does not build, so
//! those rows drive each leg from its own empty directory and compare the
//! answer, the resulting catalog, and the bytes of the `AGENTS.md` each one
//! generated.
//!
//! **The system family writes to disk.** The response is compared here; what
//! landed in `.git/hooks` is `service_system.rs`'s business.

use std::path::Path;

use storyhook::app;
use storyhook::cli::{
    CliOptions, HooksAction, Invocation, MemberInput, PluginAction, StateAction, TypeAction,
};
use storyhook::domain::{StateDef, SuperState, TypeDef};
use storyhook::error::AppError;
use storyhook::invoke::dispatch_unscoped;
use storyhook::output::Response;
use storyhook::service::project::pointer_path;
use storyhook::storage;
use storyhook::store::{ReadOps, SqliteStore, Store};
use storyhook_test_support::scratch_dir;

mod differential_support;
use differential_support::{Differential, canonical};

// --- shorthand -------------------------------------------------------------

fn new_story(title: &str) -> Invocation {
    Invocation::New {
        title: title.to_string(),
        state: None,
        story_type: None,
        description: None,
        priority: None,
        labels: None,
        assignee: None,
    }
}

fn typed_story(title: &str, story_type: &str) -> Invocation {
    Invocation::New {
        title: title.to_string(),
        state: None,
        story_type: Some(story_type.to_string()),
        description: None,
        priority: None,
        labels: None,
        assignee: None,
    }
}

fn move_to(id: &str, state: &str) -> Invocation {
    Invocation::SetState {
        id: id.to_string(),
        state: state.to_string(),
        comment: None,
        if_state: None,
    }
}

fn state(action: StateAction) -> Invocation {
    Invocation::State { action }
}

fn add_state(slug: &str, superstate: &str) -> Invocation {
    state(StateAction::Add {
        slug: slug.to_string(),
        superstate: superstate.to_string(),
        role: None,
        description: None,
    })
}

fn add_state_full(
    slug: &str,
    superstate: &str,
    role: Option<&str>,
    description: Option<&str>,
) -> Invocation {
    state(StateAction::Add {
        slug: slug.to_string(),
        superstate: superstate.to_string(),
        role: role.map(str::to_string),
        description: description.map(str::to_string),
    })
}

/// The optional halves of a `story state set`, so a row names only the flags
/// it is about. `StateAction::Set` is an enum variant and cannot be built with
/// struct-update syntax; this is the stand-in.
#[derive(Default)]
struct Set<'a> {
    superstate: Option<&'a str>,
    role: Option<&'a str>,
    description: Option<&'a str>,
    clear_description: bool,
    move_stories_to: Option<&'a str>,
}

fn set_state(slug: &str, edit: Set<'_>) -> Invocation {
    state(StateAction::Set {
        slug: slug.to_string(),
        superstate: edit.superstate.map(str::to_string),
        role: edit.role.map(str::to_string),
        description: edit.description.map(str::to_string),
        clear_description: edit.clear_description,
        move_stories_to: edit.move_stories_to.map(str::to_string),
    })
}

fn remove_state(slug: &str, move_stories_to: Option<&str>) -> Invocation {
    state(StateAction::Remove {
        slug: slug.to_string(),
        move_stories_to: move_stories_to.map(str::to_string),
    })
}

fn reorder(order: &[&str]) -> Invocation {
    state(StateAction::Reorder {
        order: order.iter().map(|s| (*s).to_string()).collect(),
    })
}

fn list_types() -> Invocation {
    Invocation::Type {
        action: TypeAction::List,
    }
}

fn add_type(slug: &str, description: Option<&str>) -> Invocation {
    Invocation::Type {
        action: TypeAction::Add {
            slug: slug.to_string(),
            description: description.map(str::to_string),
        },
    }
}

fn remove_type(slug: &str) -> Invocation {
    Invocation::Type {
        action: TypeAction::Remove {
            slug: slug.to_string(),
        },
    }
}

fn member(identity: &str) -> Invocation {
    Invocation::MemberAdd {
        input: MemberInput::Identity(identity.to_string()),
    }
}

// --- state listing ---------------------------------------------------------

#[test]
fn listing_states_agrees_on_an_untouched_project() {
    let differential = Differential::new();
    differential.step("state list", state(StateAction::List));
}

#[test]
fn listing_states_agrees_on_the_occupancy_counts() {
    let differential = Differential::new();
    let first = differential.step_id("new", new_story("one"));
    differential.step_id("new", new_story("two"));
    let third = differential.step_id("new", new_story("three"));
    differential.step("move to in-progress", move_to(&first, "in-progress"));
    differential.step("close", move_to(&third, "done"));
    differential.step("state list", state(StateAction::List));
}

#[test]
fn listing_states_agrees_when_a_story_has_been_deleted() {
    let differential = Differential::new();
    let id = differential.step_id("new", new_story("doomed"));
    differential.step(
        "delete",
        Invocation::Delete {
            id: id.clone(),
            reason: "not needed".into(),
        },
    );
    differential.step("state list", state(StateAction::List));
}

// --- adding states ---------------------------------------------------------

#[test]
fn adding_states_agrees_including_every_rejection() {
    let differential = Differential::new();
    differential.step("state add in-review", add_state("in-review", "OPEN"));
    differential.step(
        "state add wontfix --description",
        add_state_full("wontfix", "CLOSED", None, Some("Closed without a fix")),
    );
    differential.step("state list after adding", state(StateAction::List));

    differential.step("state add duplicate", add_state("todo", "OPEN"));
    differential.step("state add bad superstate", add_state("nope", "MAYBE"));
    for bad in [
        "In Review",
        "in_review",
        "in--review",
        "-review",
        "review-",
        "",
    ] {
        differential.step("state add bad slug", add_state(bad, "OPEN"));
    }
    differential.step(
        "state add second active",
        add_state_full("doing", "OPEN", Some("active"), None),
    );
    differential.step(
        "state add unknown role",
        add_state_full("triage", "OPEN", Some("triage"), None),
    );
    differential.step("state list after the rejections", state(StateAction::List));
}

// --- editing states --------------------------------------------------------

#[test]
fn editing_a_state_agrees_field_by_field() {
    let differential = Differential::new();
    differential.step(
        "state set --description",
        set_state(
            "todo",
            Set {
                description: Some("Queued"),
                ..Set::default()
            },
        ),
    );
    differential.step("state list after describing", state(StateAction::List));
    differential.step(
        "state set --no-description",
        set_state(
            "todo",
            Set {
                clear_description: true,
                ..Set::default()
            },
        ),
    );
    differential.step(
        "state set --role none",
        set_state(
            "in-progress",
            Set {
                role: Some("none"),
                ..Set::default()
            },
        ),
    );
    differential.step(
        "state set --role active",
        set_state(
            "todo",
            Set {
                role: Some("active"),
                ..Set::default()
            },
        ),
    );
    differential.step("state list after the role moves", state(StateAction::List));
}

#[test]
fn editing_a_state_agrees_on_every_rejection() {
    let differential = Differential::new();
    differential.step("state set nothing", set_state("todo", Set::default()));
    differential.step(
        "state set unknown state",
        set_state(
            "limbo",
            Set {
                description: Some("x"),
                ..Set::default()
            },
        ),
    );
    differential.step(
        "state set unknown state with no changes",
        set_state("limbo", Set::default()),
    );
    differential.step(
        "state set bad superstate",
        set_state(
            "todo",
            Set {
                superstate: Some("SIDEWAYS"),
                ..Set::default()
            },
        ),
    );
    differential.step(
        "state set unknown role",
        set_state(
            "todo",
            Set {
                role: Some("chief"),
                ..Set::default()
            },
        ),
    );
    differential.step(
        "state set a second active role",
        set_state(
            "todo",
            Set {
                role: Some("active"),
                ..Set::default()
            },
        ),
    );
}

#[test]
fn flipping_a_superstate_agrees_about_migrating_its_occupants() {
    let differential = Differential::new();
    let ids = [
        differential.step_id("new", new_story("one")),
        differential.step_id("new", new_story("two")),
    ];

    differential.step(
        "flip an occupied state with no destination",
        set_state(
            "todo",
            Set {
                superstate: Some("CLOSED"),
                ..Set::default()
            },
        ),
    );
    differential.step(
        "flip with an unknown destination",
        set_state(
            "todo",
            Set {
                superstate: Some("CLOSED"),
                move_stories_to: Some("nowhere"),
                ..Set::default()
            },
        ),
    );
    differential.step(
        "flip into itself",
        set_state(
            "todo",
            Set {
                superstate: Some("CLOSED"),
                move_stories_to: Some("todo"),
                ..Set::default()
            },
        ),
    );
    differential.step(
        "flip with a destination",
        set_state(
            "todo",
            Set {
                superstate: Some("CLOSED"),
                move_stories_to: Some("in-progress"),
                ..Set::default()
            },
        ),
    );
    for id in &ids {
        differential.show("a migrated story", id);
    }
    differential.step("state list after the flip", state(StateAction::List));
}

#[test]
fn a_metadata_edit_on_an_occupied_state_agrees_without_a_destination() {
    let differential = Differential::new();
    differential.step_id("new", new_story("sitting in todo"));
    differential.step(
        "state set --description on an occupied state",
        set_state(
            "todo",
            Set {
                description: Some("Queued"),
                ..Set::default()
            },
        ),
    );
}

#[test]
fn the_last_open_state_cannot_be_flipped_away_in_either_leg() {
    let differential = Differential::new();
    differential.step(
        "flip todo closed",
        set_state(
            "todo",
            Set {
                superstate: Some("CLOSED"),
                ..Set::default()
            },
        ),
    );
    differential.step(
        "flip the last open state closed",
        set_state(
            "in-progress",
            Set {
                superstate: Some("CLOSED"),
                ..Set::default()
            },
        ),
    );
    differential.step("state list", state(StateAction::List));
}

// --- removing states -------------------------------------------------------

#[test]
fn removing_states_agrees_including_every_rejection() {
    let differential = Differential::new();
    differential.step("remove an empty state", remove_state("in-progress", None));
    differential.step("remove an unknown state", remove_state("limbo", None));
    differential.step("remove the last closed state", remove_state("done", None));
    differential.step("state list after the removals", state(StateAction::List));
}

#[test]
fn removing_an_occupied_state_agrees_about_the_migration() {
    let differential = Differential::new();
    let ids = [
        differential.step_id("new", new_story("one")),
        differential.step_id("new", new_story("two")),
        differential.step_id("new", new_story("three")),
    ];
    differential.step("remove with no destination", remove_state("todo", None));
    differential.step(
        "remove with an unknown destination",
        remove_state("todo", Some("nowhere")),
    );
    differential.step("remove into itself", remove_state("todo", Some("todo")));
    differential.step(
        "remove with a destination",
        remove_state("todo", Some("in-progress")),
    );
    for id in &ids {
        differential.show("a migrated story", id);
    }
    differential.step("state list after the migration", state(StateAction::List));
}

#[test]
fn migrating_into_a_closed_state_agrees_about_closing_and_archiving() {
    let differential = Differential::new();
    let id = differential.step_id("new", new_story("on its way out"));
    differential.step("remove todo into done", remove_state("todo", Some("done")));
    differential.show("the closed story", &id);
    differential.step("state list", state(StateAction::List));
    differential.step(
        "the closed story cannot be edited",
        Invocation::Comment {
            id: id.clone(),
            text: "still here?".into(),
        },
    );
}

#[test]
fn a_state_with_archived_history_agrees_that_it_cannot_be_removed() {
    let differential = Differential::new();
    differential.step("state add wontfix", add_state("wontfix", "CLOSED"));
    let id = differential.step_id("new", new_story("shelved"));
    differential.step("close into wontfix", move_to(&id, "wontfix"));
    differential.step("remove wontfix", remove_state("wontfix", None));
    differential.step(
        "remove wontfix with a destination",
        remove_state("wontfix", Some("todo")),
    );
    differential.step("state list", state(StateAction::List));
}

#[test]
fn a_deleted_occupant_agrees_that_it_holds_no_state_open() {
    let differential = Differential::new();
    let id = differential.step_id("new", new_story("doomed"));
    differential.step(
        "delete",
        Invocation::Delete {
            id: id.clone(),
            reason: "gone".into(),
        },
    );
    differential.step("remove todo", remove_state("todo", None));
    differential.step("state list", state(StateAction::List));
}

// --- reordering ------------------------------------------------------------

#[test]
fn reordering_agrees_including_every_rejection() {
    let differential = Differential::new();
    differential.step("a partial order", reorder(&["todo", "done"]));
    differential.step("a repeated slug", reorder(&["todo", "todo", "in-progress"]));
    differential.step(
        "an unknown slug",
        reorder(&["todo", "in-progress", "limbo"]),
    );
    differential.step("an empty order", reorder(&[]));
    differential.step("a permutation", reorder(&["done", "todo", "in-progress"]));
    differential.step("state list after reordering", state(StateAction::List));
    differential.step_id(
        "a new story opens in the new first OPEN state",
        new_story("after"),
    );
}

// --- types -----------------------------------------------------------------

#[test]
fn the_type_family_agrees_including_every_rejection() {
    let differential = Differential::new();
    differential.step("type list", list_types());
    differential.step(
        "type add",
        add_type("spike", Some("A timeboxed investigation")),
    );
    differential.step("type add without a description", add_type("docs", None));
    differential.step("type list after adding", list_types());

    for reserved in ["none", "NONE", "default", "Default"] {
        differential.step("type add reserved", add_type(reserved, None));
    }
    differential.step("type add duplicate", add_type("bug", None));
    differential.step("type remove unknown", remove_type("nope"));
    differential.step("type remove", remove_type("spike"));
    differential.step("type list after removing", list_types());
}

#[test]
fn a_type_in_use_agrees_that_it_cannot_be_removed() {
    let differential = Differential::new();
    let id = differential.step_id("new --type bug", typed_story("a bug", "bug"));
    differential.step("type remove while open", remove_type("bug"));
    differential.step("close it", move_to(&id, "done"));
    differential.step("type remove while closed", remove_type("bug"));
}

#[test]
fn removing_every_type_agrees_about_the_last_one() {
    let differential = Differential::new();
    for slug in ["story", "epic", "bug", "chore"] {
        differential.step("type remove", remove_type(slug));
    }
    differential.step("type remove the last one", remove_type("task"));
    differential.step("type list", list_types());
}

// --- members ---------------------------------------------------------------

#[test]
fn adding_members_agrees_on_the_ids_derived() {
    let differential = Differential::new();
    for identity in [
        "Ada Lovelace <ada@example.com>",
        "Grace Hopper",
        "  Alan Turing  ",
        "<only@example.com>",
        "No Closing Angle <x",
        "!!!",
    ] {
        differential.step("member add", member(identity));
    }
    differential.step(
        "member add -g",
        Invocation::MemberAdd {
            input: MemberInput::Github("mikeyward".into()),
        },
    );
}

#[test]
fn a_duplicate_member_agrees_that_it_is_rejected() {
    let differential = Differential::new();
    differential.step("member add", member("Ada Lovelace <ada@example.com>"));
    differential.step("member add the same id again", member("ada lovelace"));
    let id = differential.step_id("new", new_story("needs an owner"));
    differential.step(
        "assign the member that survived",
        Invocation::Assign {
            id: id.clone(),
            member: "ada-lovelace".into(),
        },
    );
    differential.show("the assigned story", &id);
}

// --- the system family -----------------------------------------------------

#[test]
fn scaffolding_agrees_byte_for_byte() {
    // The templates exist twice — once in the frozen `app.rs` and once in
    // `service::templates` — because this port may not edit the former. This
    // row is what stops the two copies drifting.
    let differential = Differential::new();
    for kind in ["agents-md", "claude-md", "cursor-rules", "vim-rules", ""] {
        differential.step(
            "scaffold",
            Invocation::Scaffold {
                kind: kind.to_string(),
            },
        );
    }
}

#[test]
fn scaffolding_agrees_after_the_catalog_changes() {
    let differential = Differential::new();
    differential.step("state add shipped", add_state("shipped", "CLOSED"));
    differential.step("remove done", remove_state("done", None));
    differential.step(
        "scaffold agents-md",
        Invocation::Scaffold {
            kind: "agents-md".into(),
        },
    );
}

#[test]
fn the_hook_commands_agree() {
    let differential = Differential::new();
    differential.step(
        "hooks list",
        Invocation::Hooks {
            action: HooksAction::List,
        },
    );
    differential.step(
        "hooks test with no config",
        Invocation::Hooks {
            action: HooksAction::Test {
                event_type: "create".into(),
            },
        },
    );
    differential.step(
        "hooks test an unknown event",
        Invocation::Hooks {
            action: HooksAction::Test {
                event_type: "explode".into(),
            },
        },
    );
    // Both legs run from the legacy leg's directory, so one write configures
    // both.
    let hooks_toml = differential.legacy_path().join(".storyhook/hooks.toml");
    std::fs::write(&hooks_toml, "[on_create]\ncommand = \"true\"\n").expect("writing hooks.toml");
    differential.step(
        "hooks list with a config",
        Invocation::Hooks {
            action: HooksAction::List,
        },
    );
}

#[test]
fn the_plugin_rejections_agree() {
    let differential = Differential::new();
    for target in ["vscode", ""] {
        differential.step(
            "plugin install",
            Invocation::Plugin {
                action: PluginAction::Install {
                    target: target.to_string(),
                },
            },
        );
        differential.step(
            "plugin uninstall",
            Invocation::Plugin {
                action: PluginAction::Uninstall {
                    target: target.to_string(),
                },
            },
        );
    }
}

#[test]
fn the_text_only_commands_agree() {
    let differential = Differential::new();
    differential.step("help", Invocation::Help);
    differential.step("help --compact", Invocation::HelpCompact);
    differential.step("help --all", Invocation::HelpAll);
    differential.step("version", Invocation::Version);
    differential.step(
        "help new",
        Invocation::HelpTopic {
            topic: "new".into(),
        },
    );
    differential.step(
        "help nonsense",
        Invocation::HelpTopic {
            topic: "nonsense".into(),
        },
    );
}

// --- init ------------------------------------------------------------------
//
// `init` needs a directory with no project in it, which the shared harness
// does not build, so each leg is driven from its own.

/// Runs `story init` through the legacy path in a fresh directory.
fn legacy_init(
    root: &Path,
    prefix: Option<&str>,
    no_agents_md: bool,
) -> Result<Response, AppError> {
    app::run(
        root,
        CliOptions {
            json: false,
            quiet: false,
            no_hooks: false,
            invocation: Invocation::Init {
                prefix: prefix.map(str::to_string),
                no_agents_md,
            },
        },
    )
}

/// Runs `story init` through the store in a fresh directory.
fn store_init(
    root: &Path,
    store: &SqliteStore,
    prefix: Option<&str>,
    no_agents_md: bool,
) -> Result<Response, AppError> {
    dispatch_unscoped(
        store,
        root,
        &storage::now(),
        Invocation::Init {
            prefix: prefix.map(str::to_string),
            no_agents_md,
        },
    )
}

/// An empty checkout directory and a migrated, empty store.
fn init_fixture() -> (tempfile::TempDir, tempfile::TempDir, SqliteStore) {
    let db = scratch_dir();
    let store = SqliteStore::open(db.path().join("store.db")).expect("opening the store");
    store.migrate().expect("migrating the store");
    (scratch_dir(), db, store)
}

#[test]
fn init_agrees_on_what_it_reports() {
    let legacy_root = scratch_dir();
    let (store_root, _db, store) = init_fixture();

    let legacy = legacy_init(legacy_root.path(), None, false);
    let new = store_init(store_root.path(), &store, None, false);
    assert_eq!(
        canonical(&legacy),
        canonical(&new),
        "init's answer diverged\n legacy: {legacy:?}\n  store: {new:?}"
    );
    assert!(
        canonical(&legacy)["ok"]["message"]
            .as_str()
            .expect("a message")
            .contains("Generated AGENTS.md"),
        "the row would not have noticed a missing AGENTS.md"
    );
}

#[test]
fn init_agrees_when_agents_md_is_suppressed() {
    let legacy_root = scratch_dir();
    let (store_root, _db, store) = init_fixture();

    let legacy = legacy_init(legacy_root.path(), None, true);
    let new = store_init(store_root.path(), &store, None, true);
    assert_eq!(canonical(&legacy), canonical(&new));
    assert!(!legacy_root.path().join("AGENTS.md").exists());
    assert!(
        !store_root
            .path()
            .canonicalize()
            .unwrap()
            .join("AGENTS.md")
            .exists()
    );
}

#[test]
fn init_agrees_on_the_catalog_it_creates() {
    // The defaults exist twice — inline in `storage::init_project` and in
    // `service::project` — for the same reason the templates do. This is what
    // stops them drifting.
    let legacy_root = scratch_dir();
    let (store_root, _db, store) = init_fixture();
    legacy_init(legacy_root.path(), None, true).expect("legacy init");
    store_init(store_root.path(), &store, None, true).expect("store init");

    let legacy_states: Vec<StateDef> =
        storage::load_states(legacy_root.path()).expect("legacy states");
    let legacy_types: Vec<TypeDef> = storage::load_types(legacy_root.path()).expect("legacy types");
    let project = store
        .read(|tx| tx.projects())
        .expect("reading projects")
        .pop()
        .expect("one project");

    assert_eq!(
        store.read(|tx| tx.states(project.id)).expect("states"),
        legacy_states
    );
    assert_eq!(
        store.read(|tx| tx.types(project.id)).expect("types"),
        legacy_types
    );
    assert_eq!(
        project.prefix,
        storage::load_project_prefix(legacy_root.path()).expect("legacy prefix")
    );
    assert!(
        legacy_states
            .iter()
            .any(|state| state.super_state == SuperState::Open),
        "the fixture would pass over an empty catalog"
    );
}

#[test]
fn init_agrees_on_the_agents_md_it_generates() {
    for prefix in [None, Some("PROJ")] {
        let legacy_root = scratch_dir();
        let (store_root, _db, store) = init_fixture();
        legacy_init(legacy_root.path(), prefix, false).expect("legacy init");
        store_init(store_root.path(), &store, prefix, false).expect("store init");

        let legacy = std::fs::read_to_string(legacy_root.path().join("AGENTS.md"))
            .expect("legacy AGENTS.md");
        let new =
            std::fs::read_to_string(store_root.path().canonicalize().unwrap().join("AGENTS.md"))
                .expect("store AGENTS.md");
        assert_eq!(legacy, new, "AGENTS.md diverged for prefix {prefix:?}");
        assert!(legacy.contains(prefix.unwrap_or("SH")), "{legacy}");
    }
}

#[test]
fn a_second_init_agrees_that_it_changes_nothing() {
    let legacy_root = scratch_dir();
    let (store_root, _db, store) = init_fixture();
    legacy_init(legacy_root.path(), Some("AB"), true).expect("legacy init");
    store_init(store_root.path(), &store, Some("AB"), true).expect("store init");

    let legacy = legacy_init(legacy_root.path(), Some("ZZ"), true);
    let new = store_init(store_root.path(), &store, Some("ZZ"), true);
    assert_eq!(canonical(&legacy), canonical(&new));

    assert_eq!(
        storage::load_project_prefix(legacy_root.path()).expect("legacy prefix"),
        "AB"
    );
    let projects = store.read(|tx| tx.projects()).expect("reading projects");
    assert_eq!(
        projects.len(),
        1,
        "the second init created a second project"
    );
    assert_eq!(projects[0].prefix, "AB");
}

#[test]
fn init_leaves_no_pointer_file_before_the_flip() {
    // Two identities for one project is exactly the divergence this
    // rearchitecture exists to end, so while `.storyhook/` is still the
    // identity of record the store leg must not write a competing one.
    let (store_root, _db, store) = init_fixture();
    store_init(store_root.path(), &store, None, true).expect("store init");
    assert!(!pointer_path(&store_root.path().canonicalize().unwrap()).exists());
}

// --- phases and epics ------------------------------------------------------

fn phase(action: storyhook::cli::PhaseAction) -> Invocation {
    Invocation::Phase { action }
}

fn epic(action: storyhook::cli::EpicAction) -> Invocation {
    Invocation::Epic { action }
}

#[test]
fn the_phase_family_agrees_from_an_empty_project_onward() {
    use storyhook::cli::PhaseAction;
    let differential = Differential::new();
    differential.step("phase list on an empty project", phase(PhaseAction::List));
    differential.step(
        "phase show on an empty project",
        phase(PhaseAction::Show { phase: "1".into() }),
    );

    let created = differential.step_id(
        "phase create with a title",
        phase(PhaseAction::Create {
            phase: "1".into(),
            title: Some("the migration".into()),
        }),
    );
    differential.step(
        "phase create without a title",
        phase(PhaseAction::Create {
            phase: "2".into(),
            title: None,
        }),
    );
    differential.show("the phase story", &created);
    differential.step("phase list", phase(PhaseAction::List));
    differential.step("phase show", phase(PhaseAction::Show { phase: "1".into() }));
}

#[test]
fn assigning_and_clearing_a_phase_agrees() {
    use storyhook::cli::PhaseAction;
    let differential = Differential::new();
    let id = differential.step_id("new", new_story("needs a phase"));

    differential.step(
        "phase remove before there is one",
        phase(PhaseAction::Remove { id: id.clone() }),
    );
    differential.step(
        "phase add",
        phase(PhaseAction::Add {
            id: id.clone(),
            phase: "1".into(),
        }),
    );
    differential.show("after being assigned", &id);
    differential.step(
        "phase add again, to a different phase",
        phase(PhaseAction::Add {
            id: id.clone(),
            phase: "2".into(),
        }),
    );
    differential.show("a story belongs to one phase", &id);
    differential.step(
        "phase remove",
        phase(PhaseAction::Remove { id: id.clone() }),
    );
    differential.show("after being cleared", &id);
    differential.step(
        "phase add to an unknown story",
        phase(PhaseAction::Add {
            id: "SH-999".into(),
            phase: "1".into(),
        }),
    );
    differential.step(
        "phase remove from an unknown story",
        phase(PhaseAction::Remove {
            id: "SH-999".into(),
        }),
    );
}

#[test]
fn phasing_a_closed_story_agrees_that_it_is_refused() {
    use storyhook::cli::PhaseAction;
    let differential = Differential::new();
    let id = differential.step_id("new", new_story("already done"));
    differential.step("close", move_to(&id, "done"));
    differential.step(
        "phase add",
        phase(PhaseAction::Add {
            id: id.clone(),
            phase: "1".into(),
        }),
    );
    differential.step(
        "phase remove",
        phase(PhaseAction::Remove { id: id.clone() }),
    );
}

#[test]
fn a_phase_rollup_agrees_across_every_bucket() {
    use storyhook::cli::PhaseAction;
    let differential = Differential::new();
    let ids: Vec<String> = ["one", "two", "three", "four", "five"]
        .iter()
        .map(|title| differential.step_id("new", new_story(title)))
        .collect();
    for id in &ids {
        differential.step(
            "phase add",
            phase(PhaseAction::Add {
                id: id.clone(),
                phase: "1".into(),
            }),
        );
    }
    differential.step("in progress", move_to(&ids[1], "in-progress"));
    differential.step("closed", move_to(&ids[2], "done"));
    differential.step(
        "blocked by an open story",
        Invocation::Relate {
            a: ids[4].clone(),
            relation: "blocks".into(),
            b: ids[3].clone(),
            remove: false,
        },
    );
    differential.step("phase list", phase(PhaseAction::List));
    differential.step("phase show", phase(PhaseAction::Show { phase: "1".into() }));
}

#[test]
fn phases_agree_on_their_ordering_including_ten_before_two() {
    use storyhook::cli::PhaseAction;
    let differential = Differential::new();
    for number in ["2", "10", "1"] {
        differential.step(
            "phase create",
            phase(PhaseAction::Create {
                phase: number.to_string(),
                title: None,
            }),
        );
    }
    differential.step("phase list", phase(PhaseAction::List));
}

#[test]
fn the_epic_family_agrees_including_every_rejection() {
    use storyhook::cli::EpicAction;
    let differential = Differential::new();
    differential.step("epic list on an empty project", epic(EpicAction::List));
    let id = differential.step_id(
        "epic create",
        epic(EpicAction::Create {
            title: "the big one".into(),
        }),
    );
    differential.step("epic list", epic(EpicAction::List));
    differential.step("epic show", epic(EpicAction::Show { id: id.clone() }));
    differential.step(
        "epic show an unknown story",
        epic(EpicAction::Show {
            id: "SH-999".into(),
        }),
    );

    let child = differential.step_id("new", new_story("a child"));
    differential.step(
        "epic add",
        epic(EpicAction::Add {
            epic_id: id.clone(),
            story_id: child.clone(),
        }),
    );
    differential.show("the epic", &id);
    differential.show("the child", &child);
    differential.step(
        "epic add the same child again",
        epic(EpicAction::Add {
            epic_id: id.clone(),
            story_id: child.clone(),
        }),
    );
    differential.step(
        "epic add a story to itself",
        epic(EpicAction::Add {
            epic_id: id.clone(),
            story_id: id.clone(),
        }),
    );
    differential.step(
        "epic add an unknown story",
        epic(EpicAction::Add {
            epic_id: id.clone(),
            story_id: "SH-999".into(),
        }),
    );
    differential.step(
        "epic add to an unknown epic",
        epic(EpicAction::Add {
            epic_id: "SH-999".into(),
            story_id: child.clone(),
        }),
    );
}

#[test]
fn a_second_parent_agrees_that_it_is_refused() {
    use storyhook::cli::EpicAction;
    let differential = Differential::new();
    let first = differential.step_id(
        "epic create",
        epic(EpicAction::Create {
            title: "first".into(),
        }),
    );
    let second = differential.step_id(
        "epic create",
        epic(EpicAction::Create {
            title: "second".into(),
        }),
    );
    let child = differential.step_id("new", new_story("a child"));
    differential.step(
        "epic add",
        epic(EpicAction::Add {
            epic_id: first.clone(),
            story_id: child.clone(),
        }),
    );
    differential.step(
        "epic add to a second parent",
        epic(EpicAction::Add {
            epic_id: second.clone(),
            story_id: child.clone(),
        }),
    );
    differential.show("the child has one parent", &child);
}

#[test]
fn creating_an_epic_without_the_epic_type_agrees() {
    use storyhook::cli::EpicAction;
    let differential = Differential::new();
    differential.step("type remove epic", remove_type("epic"));
    differential.step(
        "epic create",
        epic(EpicAction::Create {
            title: "no type for this".into(),
        }),
    );
}
