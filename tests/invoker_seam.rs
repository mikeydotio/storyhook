//! The Invoker seam's execution contract, and root resolution.
//!
//! The seam was introduced with a single implementation — `LegacyInvoker`,
//! which forwarded to `app::run` — precisely so that adopting it across the
//! CLI, the TUI and the web dashboard was provably a no-op. Two tests here
//! pinned that equivalence: they ran an invocation through the seam and
//! directly, and compared the rendered bytes.
//!
//! **Both are gone, with their subject.** `app::run` and `LegacyInvoker` were
//! deleted once the dashboard moved onto the services, so there is no direct
//! call left to compare a seamed one against. What the equivalence bought —
//! that the flip changed no answers — is now held by the golden CLI corpus and
//! by `daemon_invoke.rs`'s byte comparison of the two live invokers.
//!
//! What is left here is what still has a subject: the request envelope, root
//! resolution, the project-less roster, the unmigrated-repository guard, and
//! the deletion itself.

use storyhook::cli::Invocation;
use storyhook::env::Environment;
use storyhook::invoke::{InvokeRequest, Invoker};

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
    use storyhook::env::Environment;
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
        let store =
            SqliteStore::open(Environment::at(dir.path()).store_path()).expect("opening the store");
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
        StoreInvoker::new(store, cwd, Environment::at(cwd))
            .invoke(InvokeRequest::new(Invocation::Summary))
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
    // The environment is a value, so the fixture store is named rather than
    // exported: no `set_var`, and therefore no window in which a sibling test
    // in this binary sees the wrong data directory.
    let environment = Environment::at(data.path());
    let store = open_store(&environment).expect("opening a fixture store");

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
            "project list",
            Invocation::Project {
                action: storyhook::cli::ProjectAction::List,
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
        if let Err(error) = StoreInvoker::new(&store, cwd.path(), environment.clone())
            .invoke(InvokeRequest::new(invocation))
            && error
                .to_string()
                .contains("not initialized in this directory")
        {
            refused.push(name);
        }
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
            .run(&["project", "init"])
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
            stderr.contains("run `story project init`"),
            "expected the ordinary not-initialized message, got: {stderr}"
        );
        assert!(!stderr.contains("story migrate"), "{stderr}");
    }
}

/// The legacy write path is **gone**, and nothing in `src/` can reach what is
/// left of it.
///
/// This test used to say something weaker. `app::run`, `storage.rs`'s write
/// half, `lock.rs` and `registry.rs` survived the flip because the web
/// dashboard still read `.storyhook/` directories, so the rule was "reachable
/// from `src/web.rs` and nowhere else" — a *quarantine*, which is a promise
/// that something will be deleted rather than a statement that it has been.
///
/// It has been. `app.rs`, `lock.rs` and `registry.rs` are deleted outright;
/// `storage.rs` keeps only what materializes a legacy tree from an export
/// document, because that is the far side of the rearchitecture's two-way door
/// (`docs/rearch/flip-checklist.md`'s rollback procedure, gated by
/// `tests/migrate_round_trip.rs`). Nothing under `src/` calls it, and this is
/// what says so.
///
/// The assertion is deliberately crude: it greps the source. That is what a
/// human reviewer would do, it needs no build graph, and — unlike a
/// `#[deprecated]` or a visibility change — it cannot be satisfied by a
/// re-export.
#[test]
fn the_legacy_write_path_is_gone() {
    use std::path::Path;

    /// Every `.rs` file under `src/`, with its path relative to the crate root.
    fn sources(dir: &Path, into: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).expect("reading src/") {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                sources(&path, into);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let text = std::fs::read_to_string(&path).expect("reading a source file");
                into.push((path.to_string_lossy().into_owned(), text));
            }
        }
    }

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&root, &mut files);
    assert!(
        files.len() > 20,
        "expected the whole tree, got {}",
        files.len()
    );

    // The three modules are gone as files, which is the part a grep cannot
    // fake.
    for gone in ["app.rs", "lock.rs", "registry.rs", "github/auto.rs"] {
        assert!(
            !root.join(gone).exists(),
            "src/{gone} was deleted by the git-features wave and must stay deleted"
        );
    }

    // `crate::storage` still compiles, for the rollback path. No production
    // file may name it — `storage.rs` itself excepted, and it is the only
    // exception there is.
    const FORBIDDEN: [&str; 5] = [
        "LegacyInvoker",
        "crate::app::",
        "crate::lock::",
        "crate::registry::",
        "crate::storage",
    ];
    const ALLOWED: [&str; 1] = ["src/storage.rs"];

    let mut breaches = Vec::new();
    for (path, text) in &files {
        let relative = path
            .rsplit_once("/src/")
            .map(|(_, rest)| format!("src/{rest}"))
            .unwrap_or_else(|| path.clone());
        if ALLOWED.contains(&relative.as_str()) {
            continue;
        }
        for (number, line) in text.lines().enumerate() {
            // Doc comments describe the deletion; they do not undo it.
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            for door in FORBIDDEN {
                if code.contains(door) {
                    breaches.push(format!("{relative}:{}: {}", number + 1, code.trim()));
                }
            }
        }
    }

    assert!(
        breaches.is_empty(),
        "the legacy storage path is deleted and `src/storage.rs` survives only as the \
         rollback writer `tests/migrate_round_trip.rs` drives. Production code must not \
         reach it:\n  {}",
        breaches.join("\n  ")
    );
}

/// `.storyhook` is a path literal, and after the flip a production module that
/// still holds one is a module that still writes into the user's repository.
///
/// `src/github/` was the last place they hid: the sync engine kept its
/// configuration in `.storyhook/github-sync.toml`, its merge bases in
/// `.storyhook/github-sync/bases/`, and — worst of the three — a pre-sync
/// backup of every story it was about to rewrite in
/// `.storyhook/github-sync/backups/`, so a sync dirtied the working tree with
/// files the user then had to decide whether to commit. All three moved into
/// the store or the state home behind `SyncStorage`, and this is what keeps
/// them there.
///
/// Scoped to `src/github/` deliberately. The rest of `src/` still names
/// `.storyhook` in the places it must: the reader, the unmigrated-repository
/// guard, and the pointer file's own name.
#[test]
fn no_storyhook_path_literal_survives_in_the_github_module() {
    use std::path::Path;

    fn sources(dir: &Path, into: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).expect("reading src/github/") {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                sources(&path, into);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let text = std::fs::read_to_string(&path).expect("reading a source file");
                into.push((path.to_string_lossy().into_owned(), text));
            }
        }
    }

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/github");
    let mut files = Vec::new();
    sources(&root, &mut files);
    assert!(files.len() > 5, "expected the module, got {}", files.len());

    let mut found = Vec::new();
    for (path, text) in &files {
        for (number, line) in text.lines().enumerate() {
            // Comments explain where the state used to live; they cannot open
            // a file. Same rule as `the_legacy_write_path_is_gone` — this test
            // is about literals the program acts on.
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            if code.contains(".storyhook") {
                found.push(format!(
                    "{}:{}: {}",
                    path.rsplit_once("/src/")
                        .map_or(path.as_str(), |(_, rest)| rest),
                    number + 1,
                    code.trim()
                ));
            }
        }
    }

    assert!(
        found.is_empty(),
        "github-sync must not name the legacy directory anywhere — its state lives in \
         the store and its backups in the state home:\n  {}",
        found.join("\n  ")
    );
}
