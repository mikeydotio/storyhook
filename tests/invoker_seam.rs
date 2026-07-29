//! The Invoker seam's equivalence contract, and root resolution.
//!
//! [`LegacyInvoker`] exists to be *provably* the same thing as calling
//! `app::run`, so that adopting it across the CLI and the web server is a
//! no-op that later implementations can be swapped into. It forwards
//! verbatim, so equivalence is true by construction — this file pins it
//! anyway, because "forwards verbatim" is exactly the kind of property a
//! well-meaning refactor breaks quietly.
//!
//! **`LegacyInvoker` no longer serves the CLI or the TUI.** It is reachable
//! only from the web dashboard, which still reads `.storyhook/` directly until
//! the daemon wave absorbs it — so these two tests build their fixture through
//! `app::run` itself rather than through `ProjectBuilder`, whose `story init`
//! now creates a project in the store and no directory at all.

use storyhook::app;
use storyhook::cli::{CliOptions, Invocation};
use storyhook::error::AppError;
use storyhook::invoke::{InvokeRequest, Invoker, LegacyInvoker};
use storyhook::output::{render_error, render_response};
use storyhook_test_support::TestEnv;

/// The same four rendering modes `tests/wire_envelope.rs` checks.
const RENDER_MODES: [(bool, bool); 4] =
    [(false, false), (true, false), (false, true), (true, true)];

/// Reading and writing invocations both produce, through the seam, exactly
/// what a direct `app::run` produces — compared as rendered bytes, in every
/// rendering mode, because rendered bytes are what a caller sees.
#[test]
fn the_seam_renders_what_a_direct_app_run_renders() {
    let project = TestEnv::shared()
        .project()
        .legacy()
        .seed_story("A story to look at")
        .build();
    let project = project.path();
    let id = "SH-1".to_string();

    let cases = [
        Invocation::Show { id: id.clone() },
        Invocation::List {
            state: None,
            assignee: None,
            flagged: false,
            priority: None,
            label: None,
            created_after: None,
            updated_after: None,
            blocked: false,
            ready: false,
            stale: None,
            phase: None,
            story_type: None,
        },
        Invocation::Summary,
        // A mutation as well as reads: the seam must not change what a write
        // returns either. Idempotent, so running it twice is safe.
        Invocation::SetPriority {
            id: id.clone(),
            priority: "high".to_string(),
        },
    ];

    for invocation in cases {
        let direct = app::run(
            project,
            CliOptions {
                json: false,
                quiet: false,
                no_hooks: false,
                invocation: invocation.clone(),
            },
        )
        .expect("the direct call must succeed");

        let seamed = LegacyInvoker::new(project)
            .invoke(InvokeRequest::new(invocation.clone()))
            .expect("the seam must succeed");

        for (json, quiet) in RENDER_MODES {
            assert_eq!(
                render_response(&direct, json, quiet),
                render_response(&seamed, json, quiet),
                "{invocation:?} rendered differently through the seam \
                 (json={json}, quiet={quiet})"
            );
        }
    }
}

/// Errors take the same path: same variant, same exit code, same rendering.
#[test]
fn the_seam_reports_errors_unchanged() {
    let project = TestEnv::shared().project().legacy().build();
    let project = project.path();
    let invocation = Invocation::Show {
        id: "SH-404".to_string(),
    };

    let direct = app::run(
        project,
        CliOptions {
            json: false,
            quiet: false,
            no_hooks: false,
            invocation: invocation.clone(),
        },
    )
    .expect_err("showing a missing story must fail");

    let seamed = LegacyInvoker::new(project)
        .invoke(InvokeRequest::new(invocation))
        .expect_err("showing a missing story must fail through the seam too");

    assert!(matches!(direct, AppError::NotFound(_)));
    assert!(matches!(seamed, AppError::NotFound(_)));
    assert_eq!(direct.exit_code(), seamed.exit_code());
    for json in [false, true] {
        assert_eq!(render_error(&direct, json), render_error(&seamed, json));
    }
}

/// `no_hooks` is the only execution setting that crosses the seam, so its
/// default matters: a request built without saying otherwise must run hooks,
/// which is what an un-flagged `story` command does.
#[test]
fn a_request_runs_hooks_unless_told_not_to() {
    let request = InvokeRequest::new(Invocation::Summary);
    assert!(!request.no_hooks, "hooks are on by default");
    assert!(request.clone().no_hooks(true).no_hooks);
    assert!(!request.no_hooks(false).no_hooks);
}

/// The request envelope crosses a process boundary in later waves, so it has
/// to survive one now. (`tests/wire_envelope.rs` proves the same for the
/// `Invocation` it carries.)
#[test]
fn a_request_survives_a_wire_hop() {
    let request = InvokeRequest::new(Invocation::Show {
        id: "SH-1".to_string(),
    })
    .no_hooks(true);
    let encoded = serde_json::to_string(&request).expect("an InvokeRequest must serialize");
    let decoded: InvokeRequest =
        serde_json::from_str(&encoded).expect("an InvokeRequest must deserialize");
    assert_eq!(request, decoded);
}

// ---------------------------------------------------------------------------
// Root resolution: the upward walk
// ---------------------------------------------------------------------------

mod resolution {
    use std::path::Path;

    use storyhook::cli::Invocation;
    use storyhook::error::AppError;
    use storyhook::invoke::{InvokeRequest, Invoker, StoreInvoker};
    use storyhook::output::Response;
    use storyhook::service::project::{ProjectPointer, write_pointer};
    use storyhook::service::{InitOptions, ProjectService};
    use storyhook::store::{ProjectId, ReadOps, SqliteStore, Store};
    use storyhook_test_support::scratch_dir;
    use tempfile::TempDir;

    /// A store, and a project rooted at a scratch directory.
    fn project(pointer: bool) -> (TempDir, SqliteStore, TempDir, ProjectId) {
        let dir = scratch_dir();
        let store = SqliteStore::open(dir.path().join("store.db")).expect("opening the store");
        store.migrate().expect("migrating");
        let root = scratch_dir();
        let outcome = ProjectService::new(&store, root.path())
            .init(&InitOptions {
                agents_md: false,
                pointer,
                ..InitOptions::default()
            })
            .expect("initializing");
        (dir, store, root, outcome.project)
    }

    /// `story summary` from `cwd`, which needs a resolved project to answer.
    fn summary(store: &SqliteStore, cwd: &Path) -> Result<Response, AppError> {
        StoreInvoker::new(store, cwd).invoke(InvokeRequest::new(Invocation::Summary))
    }

    #[test]
    fn a_command_run_in_a_subdirectory_finds_the_project_above_it() {
        let (_dir, store, root, _project) = project(true);
        let deep = root.path().join("src/service/nested");
        std::fs::create_dir_all(&deep).expect("creating a subdirectory");

        summary(&store, &deep).expect(
            "a command run from a subdirectory must resolve the project above it — \
             this is the behaviour change the flip makes, and the reason the plugin's \
             cd-to-the-repo-root subshell exists",
        );
    }

    #[test]
    fn the_nearest_project_wins_over_an_outer_one() {
        let (_dir, store, outer, outer_id) = project(true);
        let inner_root = outer.path().join("vendor/embedded");
        std::fs::create_dir_all(&inner_root).expect("creating the inner checkout");
        let inner_id = ProjectService::new(&store, &inner_root)
            .init(&InitOptions {
                agents_md: false,
                pointer: true,
                ..InitOptions::default()
            })
            .expect("initializing the inner project")
            .project;
        assert_ne!(outer_id, inner_id);

        let deep = inner_root.join("a/b");
        std::fs::create_dir_all(&deep).expect("creating a subdirectory");

        // Both would answer; the walk stops at the first that does.
        let ctx_project = storyhook_test_support::project_id_at(&store, &deep);
        assert_eq!(
            ctx_project, None,
            "the leaf itself identifies nothing — the answer has to come from the walk"
        );
        summary(&store, &deep).expect("the inner project answers");
        let paths = store
            .read(|tx| tx.project_paths(inner_id))
            .expect("reading checkouts");
        assert_eq!(paths.len(), 1, "resolution must not register new checkouts");
    }

    #[test]
    fn a_directory_under_no_project_at_all_still_refuses() {
        let (_dir, store, _root, _project) = project(true);
        let stranger = scratch_dir();
        let error = summary(&store, stranger.path()).expect_err("nothing to resolve");
        assert!(matches!(error, AppError::NotFound(_)), "{error}");
    }

    #[test]
    fn the_walk_resolves_by_a_recorded_path_when_there_is_no_pointer() {
        // Repositories migrated before the pointer existed, and the legacy web
        // daemon's checkouts, have a path row and no committed file.
        let (_dir, store, root, _project) = project(false);
        let deep = root.path().join("deep/er/still");
        std::fs::create_dir_all(&deep).expect("creating a subdirectory");
        summary(&store, &deep).expect("a recorded path is an identity too");
    }

    #[test]
    fn a_pointer_naming_an_unknown_project_does_not_shadow_a_valid_path_row() {
        let (_dir, store, root, _project) = project(false);
        write_pointer(
            root.path(),
            &ProjectPointer::new("no-such-uuid".to_string(), "SH".to_string()),
        )
        .expect("writing a stale pointer");

        summary(&store, root.path()).expect(
            "a pointer the store cannot resolve must fall through to the path rather \
             than making the directory unusable",
        );
    }

    #[test]
    fn two_checkouts_of_one_project_resolve_to_the_same_project() {
        // The rearchitecture's headline property, at the level resolution owns
        // it: the pointer file is committed, so a second checkout carries it and
        // answers with the same project id.
        let (_dir, store, root, project_id) = project(true);
        let pointer = std::fs::read_to_string(root.path().join(".storyhook.toml"))
            .expect("reading the pointer");
        let clone = scratch_dir();
        std::fs::write(clone.path().join(".storyhook.toml"), pointer).expect("cloning it");

        assert_eq!(
            storyhook_test_support::project_id_at(&store, clone.path()),
            Some(project_id)
        );
        summary(&store, clone.path()).expect("the clone answers");
    }
}

/// Every verb that must answer in a directory storyhook has never heard of.
///
/// The trap this guards is specific and it has already been sprung twice in one
/// wave: an arm answered without a project inside `dispatch_unscoped`, but was
/// missing from `is_project_less`, so `StoreInvoker` tried to resolve a project
/// first and the verb failed with "not initialized" in exactly the situation it
/// exists to serve. `story web status` and `story update --check` were both
/// broken that way, and neither had a test that would have noticed.
///
/// The assertion is deliberately weak about *what* each verb answers — some
/// succeed, some fail for their own reasons — and strict about the one thing
/// that must never happen: none of them may be refused for want of a project.
#[test]
fn the_project_less_verbs_all_answer_outside_a_project() {
    use storyhook::cli::{HooksAction, PluginAction, WebAction};
    use storyhook::invoke::{StoreInvoker, open_store};
    use storyhook_test_support::scratch_dir;

    let data = scratch_dir();
    let cwd = scratch_dir();
    // `open_store` reads the environment, so point it at a fixture directory
    // for the duration — this test is single-threaded within its own binary
    // slot and restores the variable before it returns.
    let previous = std::env::var_os("STORYHOOK_DATA_DIR");
    unsafe { std::env::set_var("STORYHOOK_DATA_DIR", data.path()) };
    let store = open_store().expect("opening a fixture store");

    let cases: Vec<(&str, Invocation)> = vec![
        ("help", Invocation::Help),
        ("help-compact", Invocation::HelpCompact),
        ("help-all", Invocation::HelpAll),
        (
            "help-topic",
            Invocation::HelpTopic {
                topic: "new".to_string(),
            },
        ),
        ("version", Invocation::Version),
        (
            "hooks list",
            Invocation::Hooks {
                action: HooksAction::List,
            },
        ),
        (
            "plugin uninstall",
            Invocation::Plugin {
                action: PluginAction::Uninstall {
                    target: "claude".to_string(),
                },
            },
        ),
        (
            "scaffold",
            Invocation::Scaffold {
                kind: "agents-md".to_string(),
            },
        ),
        ("session-start", Invocation::SessionStart),
        (
            "web status",
            Invocation::Web {
                action: WebAction::Status,
            },
        ),
        (
            "web list",
            Invocation::Web {
                action: WebAction::List,
            },
        ),
        (
            "update --check",
            Invocation::Update {
                check: true,
                force: false,
            },
        ),
        (
            "decompose --dry-run",
            Invocation::Decompose {
                file: None,
                stdin: false,
                dry_run: true,
            },
        ),
        (
            "migrate",
            Invocation::Migrate {
                path: None,
                dry_run: true,
            },
        ),
    ];

    let mut refused = Vec::new();
    for (name, invocation) in cases {
        if let Err(error) =
            StoreInvoker::new(&store, cwd.path()).invoke(InvokeRequest::new(invocation))
            && error
                .to_string()
                .contains("not initialized in this directory")
        {
            refused.push(name);
        }
    }

    match previous {
        Some(value) => unsafe { std::env::set_var("STORYHOOK_DATA_DIR", value) },
        None => unsafe { std::env::remove_var("STORYHOOK_DATA_DIR") },
    }

    assert!(
        refused.is_empty(),
        "these verbs answer without a project in `dispatch_unscoped` but are missing \
         from `is_project_less`, so they are refused before they are reached: {refused:?}"
    );
}

// ---------------------------------------------------------------------------
// The unmigrated-repository guard
// ---------------------------------------------------------------------------

mod unmigrated {
    use storyhook_test_support::TestEnv;

    /// A checkout that still keeps its stories in `.storyhook/`.
    fn legacy_checkout() -> storyhook_test_support::Project<'static> {
        TestEnv::shared()
            .project()
            .legacy()
            .seed_story("Still in the directory")
            .build()
    }

    #[test]
    fn an_unmigrated_repository_is_told_to_migrate_rather_than_to_init() {
        let project = legacy_checkout();
        project
            .run(&["list"])
            .code(3)
            .stderr(predicates::str::contains("story migrate"))
            .stderr(predicates::str::contains("`.storyhook/` directory"))
            .stderr(predicates::str::contains(
                "never writes to the directory it reads",
            ));
    }

    #[test]
    fn the_guard_reaches_up_from_a_subdirectory() {
        let project = legacy_checkout();
        let deep = project.path().join("src/inner");
        std::fs::create_dir_all(&deep).expect("creating a subdirectory");

        let out = project
            .env()
            .story(&deep)
            .args(["show", "SH-1"])
            .output()
            .expect("running story show");
        assert_eq!(out.status.code(), Some(3));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("story migrate"),
            "a subdirectory of an unmigrated repository must get the same diagnosis: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn init_refuses_rather_than_minting_an_empty_project_beside_real_data() {
        // The dangerous case. `story init` in an unmigrated repository would
        // create a project with no stories in it, leave the user's data in the
        // directory, and look like it worked.
        let project = legacy_checkout();
        project
            .run(&["init"])
            .code(3)
            .stderr(predicates::str::contains("story migrate"));
    }

    #[test]
    fn migrate_itself_is_never_refused_by_the_guard() {
        // The guard's whole job is to send people here, so it must not stand in
        // the way. `migrate` is project-less, which is what keeps it reachable.
        let project = legacy_checkout();
        project
            .run(&["migrate", "--dry-run"])
            .success()
            .stdout(predicates::str::contains("would import 1 stories"));
    }

    #[test]
    fn a_migrated_repository_stops_tripping_the_guard() {
        let project = legacy_checkout();
        project.run(&["migrate"]).success();

        // The directory is still there — `migrate` never writes to the tree it
        // reads — so the guard has to be answered by the pointer file rather
        // than by the directory's absence.
        assert!(project.path().join(".storyhook/project.toml").is_file());
        assert!(project.path().join(".storyhook.toml").is_file());
        project
            .run(&["list"])
            .success()
            .stdout(predicates::str::contains("Still in the directory"));
    }

    #[test]
    fn a_directory_with_only_legacy_config_is_not_reported_as_unmigrated() {
        // `.storyhook/hooks.toml` and `plugin-config.toml` are still read from
        // their old home. A repository that has those and no `project.toml` has
        // not been left behind by anything.
        let env = TestEnv::shared();
        let dir = storyhook_test_support::scratch_dir();
        std::fs::create_dir_all(dir.path().join(".storyhook")).expect("creating the config dir");
        std::fs::write(
            dir.path().join(".storyhook/hooks.toml"),
            "[on_create]\ncommand = \"true\"\n",
        )
        .expect("writing hooks");

        let out = env
            .story(dir.path())
            .args(["list"])
            .output()
            .expect("running story list");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("run `story init`"),
            "expected the ordinary not-initialized message, got: {stderr}"
        );
        assert!(!stderr.contains("story migrate"), "{stderr}");
    }
}
