//! The Invoker seam's equivalence contract.
//!
//! [`LegacyInvoker`] exists to be *provably* the same thing as calling
//! `app::run`, so that adopting it across the CLI and the web server is a
//! no-op that later implementations can be swapped into. It forwards
//! verbatim, so equivalence is true by construction — this file pins it
//! anyway, because "forwards verbatim" is exactly the kind of property a
//! well-meaning refactor breaks quietly.

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
    let env = TestEnv::shared();
    let project = env.project().build();
    let id = project.new_story("A story to look at");

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
            project.path(),
            CliOptions {
                json: false,
                quiet: false,
                no_hooks: false,
                invocation: invocation.clone(),
            },
        )
        .expect("the direct call must succeed");

        let seamed = LegacyInvoker::new(project.path())
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
    let env = TestEnv::shared();
    let project = env.project().build();
    let invocation = Invocation::Show {
        id: "SH-404".to_string(),
    };

    let direct = app::run(
        project.path(),
        CliOptions {
            json: false,
            quiet: false,
            no_hooks: false,
            invocation: invocation.clone(),
        },
    )
    .expect_err("showing a missing story must fail");

    let seamed = LegacyInvoker::new(project.path())
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
