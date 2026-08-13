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
    ///
    /// The checkout carries a pointer file, because an attaching run always
    /// writes one (SH-119); a test whose subject is a checkout without one
    /// removes the file itself, and says why.
    fn project() -> (TempDir, SqliteStore, TempDir, ProjectId) {
        let dir = scratch_dir();
        let store =
            SqliteStore::open(Environment::at(dir.path()).store_path()).expect("opening the store");
        store.migrate().expect("migrating");
        let root = scratch_dir();
        let outcome = ProjectService::new(&store, root.path())
            .init(&InitOptions {
                agents_md: false,
                ..InitOptions::default()
            })
            .expect("initializing");
        (dir, store, root, outcome.project)
    }

    /// Removes a checkout's pointer file, leaving only its recorded path row.
    fn forget_the_pointer(root: &Path) {
        std::fs::remove_file(root.join(".storyhook.toml")).expect("removing the pointer file");
    }

    /// `story summary` from `cwd`, which needs a resolved project to answer.
    fn summary(store: &SqliteStore, cwd: &Path) -> Result<Response, AppError> {
        StoreInvoker::new(store, cwd, Environment::at(cwd))
            .invoke(InvokeRequest::new(Invocation::Summary))
    }

    #[test]
    fn a_command_run_in_a_subdirectory_finds_the_project_above_it() {
        let (_dir, store, root, _project) = project();
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
        let (_dir, store, outer, outer_id) = project();
        let inner_root = outer.path().join("vendor/embedded");
        std::fs::create_dir_all(&inner_root).expect("creating the inner checkout");
        let inner_id = ProjectService::new(&store, &inner_root)
            .init(&InitOptions {
                agents_md: false,
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
        assert_eq!(
            store
                .read(|tx| tx.checkout_path(inner_id))
                .expect("reading the linked checkout")
                .as_deref(),
            Some(inner_root.canonicalize().expect("canonicalizing").as_path()),
            "resolving from a subdirectory must not re-point the project at it"
        );
    }

    #[test]
    fn a_directory_under_no_project_at_all_still_refuses() {
        let (_dir, store, _root, _project) = project();
        let stranger = scratch_dir();
        let error = summary(&store, stranger.path()).expect_err("nothing to resolve");
        assert!(matches!(error, AppError::NotFound(_)), "{error}");
    }

    #[test]
    fn a_checkout_that_lost_its_pointer_file_refuses_rather_than_resolving() {
        // The premise this replaces was `the_walk_resolves_by_a_recorded_path
        // _when_there_is_no_pointer`, and it was true for as long as the store
        // kept an index of directories. SH-119 deleted it: a recorded path is a
        // fact about one machine, and the epic's invariant is that nothing
        // about the filesystem is ever *required* to say which project this is.
        //
        // What answers for a checkout with no pointer file now is its origin,
        // which this scratch directory does not have — so it refuses, naming
        // both ways out.
        let (_dir, store, root, _project) = project();
        forget_the_pointer(root.path());
        let deep = root.path().join("deep/er/still");
        std::fs::create_dir_all(&deep).expect("creating a subdirectory");

        let error = summary(&store, &deep).expect_err("a directory is not an identity");
        assert!(matches!(error, AppError::NotFound(_)), "{error}");
    }

    #[test]
    fn a_pointer_naming_an_unknown_project_refuses_by_naming_it() {
        // There is no path row left for a stale pointer to fall through *to*,
        // and falling through to nothing would report "not initialized" about a
        // checkout that states its identity in a committed file. The refusal
        // names the project instead, which is what a fresh clone needs to hear.
        let (_dir, store, root, _project) = project();
        write_pointer(
            root.path(),
            &ProjectPointer::new("no-such-uuid".to_string(), "SH".to_string()),
        )
        .expect("writing a stale pointer");

        let error = summary(&store, root.path()).expect_err("the store has no such project");
        assert!(
            error.to_string().contains("no-such-uuid"),
            "the refusal must name the project the checkout claims: {error}"
        );
    }

    #[test]
    fn the_climb_stops_at_the_repository_it_is_standing_in() {
        // Unbounded, this is how a scratch directory made under a checkout of
        // storyhook answers as storyhook: the enclosing pointer file is simply
        // the nearest one above. A repository is the unit an identity belongs
        // to, so a directory inside a *different* repository must not inherit
        // the one outside it.
        let (_dir, store, outer, _project) = project();
        let inner = outer.path().join("vendored");
        std::fs::create_dir_all(inner.join(".git")).expect("creating a repository top level");
        let deep = inner.join("src");
        std::fs::create_dir_all(&deep).expect("creating a subdirectory");

        let error = summary(&store, &deep).expect_err(
            "the climb must stop at `vendored`, which identifies no project, rather \
             than answering with the project outside it",
        );
        assert!(matches!(error, AppError::NotFound(_)), "{error}");
    }

    #[test]
    fn a_linked_worktrees_git_file_does_not_stop_the_climb() {
        // The other side of the bound, and the reason it tests for a `.git`
        // *directory*. A linked worktree holds a `.git` file, and its tree is a
        // commit made before `story project new` ever ran — so the main
        // checkout's pointer is the only one there is. This is what keeps
        // `dispatch` and `worktree_truth` resolving.
        let (_dir, store, root, project_id) = project();
        let worktree = root.path().join(".claude/worktrees/wt");
        std::fs::create_dir_all(&worktree).expect("creating a worktree directory");
        std::fs::write(
            worktree.join(".git"),
            "gitdir: /elsewhere/.git/worktrees/wt\n",
        )
        .expect("writing a worktree's .git file");

        let resolved =
            summary(&store, &worktree).expect("a worktree resolves by its main checkout");
        assert!(matches!(resolved, Response::Summary(_)), "{resolved:?}");
        assert_eq!(
            storyhook_test_support::project_id_at(&store, root.path()),
            Some(project_id),
            "and it is the main checkout's project it resolved to"
        );
    }

    #[test]
    fn two_checkouts_of_one_project_resolve_to_the_same_project() {
        // The rearchitecture's headline property, at the level resolution owns
        // it: the pointer file is committed, so a second checkout carries it and
        // answers with the same project id.
        let (_dir, store, root, project_id) = project();
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
    use storyhook::cli::{HooksAction, PluginAction, StoreAction, WebAction};
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
        // SH-135: `story store backup` snapshots the *ambient* store, so — like
        // `web status` and `update --check` before it — it must answer with no
        // project resolvable, never with "not initialized in this directory".
        (
            "store backup",
            Invocation::Store {
                action: StoreAction::Backup { label: None },
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
            .run(&["project", "new", "--prefix", "SH"])
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
        // The ordinary refusal, which SH-116 rewrote: it used to be one line
        // naming a single command and now names all three ways out. What
        // this test is about is unchanged and is the *second* assertion — that
        // a directory carrying only legacy *config* is not accused of being an
        // unmigrated tree. The first is here to prove it got the ordinary
        // refusal rather than some other failure on the way.
        assert!(
            stderr.contains("story project new"),
            "expected the ordinary refusal, got: {stderr}"
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

/// **An origin enters the store through exactly one function** (SH-151).
///
/// `service::project::register_origin` is the only `src/` caller of
/// `WriteOps::link_remote`, and the only thing that can hand it an origin is an
/// `OwnedOrigin` — a type with no public constructor except the audited
/// `explicit`, which `story project link origin <url>` uses and which is itself
/// gated by `claim_stated`.
///
/// The funnel is what makes the rule survive its author. Before SH-151 there
/// were four call sites, three of them inside one `init` transaction, and every
/// one of them independently decided whether the directory it stood in was
/// entitled to the URL it had read — which is how a subdirectory came to
/// register the enclosing repository's identity in the first place. A fifth
/// site cannot be added now without answering the ownership question, because
/// the argument type will not construct itself.
///
/// Greps the source for the same reason
/// [`the_legacy_write_path_is_gone`] does: it is what a reviewer would do, it
/// needs no build graph, and it cannot be satisfied by a re-export.
#[test]
fn an_origin_is_registered_in_exactly_one_place() {
    use std::path::Path;

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

    // The trait's own declaration, and the SQLite implementation of it, are not
    // call sites; the funnel is the only thing that *invokes* it.
    const ALLOWED: [&str; 5] = [
        "src/service/project.rs",
        "src/store/mod.rs",
        "src/store/sqlite/mod.rs",
        "src/store/sqlite/write.rs",
        "src/store/conformance.rs",
    ];

    let mut callers = Vec::new();
    for (path, text) in &files {
        let relative = path
            .rsplit_once("/src/")
            .map_or_else(|| path.clone(), |(_, rest)| format!("src/{rest}"));
        if ALLOWED.contains(&relative.as_str()) {
            continue;
        }
        for (number, line) in text.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            // `unlink_remote` is a different verb with a different rule:
            // removing a registration asks nothing about entitlement, so it
            // needs no funnel and must not trip this.
            if code.contains("link_remote(") && !code.contains("unlink_remote(") {
                callers.push(format!("{relative}:{}: {}", number + 1, code.trim()));
            }
        }
    }

    assert!(
        callers.is_empty(),
        "an origin is registered through `service::project::register_origin` and nowhere else, \
         so that ownership is asked once rather than at each site. Route these through \
         it:\n  {}",
        callers.join("\n  ")
    );

    // And the funnel is really there — a rename that emptied the grep would
    // otherwise pass this test by deleting the thing it protects.
    let funnel = std::fs::read_to_string(root.join("service/project.rs"))
        .expect("reading src/service/project.rs");
    assert_eq!(
        funnel.matches("tx.link_remote(").count(),
        1,
        "`register_origin` holds the single call; if it moved, move this assertion with it"
    );
}

/// **Exactly two writes may reach a closed story on a person's behalf**
/// (SH-261, widened by SH-279).
///
/// `Intent::Append` resolves through `resolve_appendable_story`, which permits a
/// closed story where `resolve_open_story` refuses one. That relaxation was
/// argued for, and granted to, a single verb at first: `story comment`. The
/// argument was specific — a comment reaches nothing but the comment list and
/// `updated_at`, so it cannot touch the state, scope or rollups that closing a
/// story is supposed to freeze. `commit-sync`'s commit link joined it in
/// SH-279 on the identical argument: `StoryCommitLinked` reaches only
/// `referenced_by_commits` and `updated_at`. Nothing about either argument
/// generalizes to a verb that has not made its own case.
///
/// The failure this prevents is not someone deliberately widening the rule. It
/// is someone writing a seventh single-story write, seeing two intents, and
/// picking the one that does not refuse — which is exactly how the relaxation
/// stops being a decision and becomes an ambient permission. Adding a third
/// `Intent::Append` fails here until whoever added it comes and says so on
/// purpose.
///
/// Greps the source for the reason [`the_legacy_write_path_is_gone`] does: it is
/// what a reviewer would do, it needs no build graph, and it cannot be satisfied
/// by a re-export.
#[test]
fn only_comment_and_commit_link_append_to_a_closed_story() {
    use std::path::Path;

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

    // `service/mod.rs` declares the intent and owns the only call of the
    // resolver it maps to; it is the definition, not a call site.
    const DEFINITION: &str = "src/service/mod.rs";

    let mut appenders = Vec::new();
    let mut resolvers = Vec::new();
    for (path, text) in &files {
        let relative = path
            .rsplit_once("/src/")
            .map_or_else(|| path.clone(), |(_, rest)| format!("src/{rest}"));
        if relative == DEFINITION {
            continue;
        }
        for (number, line) in text.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            let site = format!("{relative}:{}: {}", number + 1, code.trim());
            if code.contains("Intent::Append") {
                appenders.push(site.clone());
            }
            // Reaching the resolver directly would route around the intent
            // entirely, which is the same widening by a quieter road.
            if code.contains("resolve_appendable_story") {
                resolvers.push(site);
            }
        }
    }

    assert_eq!(
        appenders.len(),
        2,
        "`Intent::Append` reaches exactly two writes — `story comment` (SH-261) and \
         `commit-sync`'s commit link (SH-279) — each granted on its own argument about what \
         derived state the write can touch. A third needs its own argument, and this assertion \
         updated to record that it was made:\n  {}",
        appenders.join("\n  ")
    );
    assert!(
        appenders
            .iter()
            .any(|site| site.starts_with("src/service/story.rs:")),
        "one append must be the story service's `comment`, got:\n  {}",
        appenders.join("\n  ")
    );
    assert!(
        appenders
            .iter()
            .any(|site| site.starts_with("src/service/git.rs:")),
        "one append must be `commit-sync`'s commit link, got:\n  {}",
        appenders.join("\n  ")
    );
    assert!(
        resolvers.is_empty(),
        "`resolve_appendable_story` is reached through `Intent::Append`, never directly, so the \
         guard cannot be picked up without the intent that names it:\n  {}",
        resolvers.join("\n  ")
    );

    // And the seam is really there — a rename that emptied both greps would
    // otherwise pass this test by deleting the thing it protects.
    let definition =
        std::fs::read_to_string(root.join("service/mod.rs")).expect("reading src/service/mod.rs");
    assert!(
        definition.contains("fn resolve_appendable_story("),
        "`resolve_appendable_story` holds the closed-story relaxation; if it moved, move this \
         assertion with it"
    );
    assert_eq!(
        definition
            .matches("resolve_appendable_story(tx, project, prefix, id)")
            .count(),
        1,
        "`Intent::resolve` holds the single call to it"
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

/// Nothing outside the bind site may probe the machine for a host to print.
///
/// SH-110's defect was a client naming the dashboard from a fresh `tailscale`
/// probe rather than from what the daemon bound. The type system carries most
/// of the weight now — `advertise_host` exists only on `TailnetBind` and
/// `BoundAddress`, and neither can be built without a successful bind — but
/// `tailnet_identity` is still `pub`, and a future command could call it and
/// format a URL from the result without touching any of those types. That would
/// compile, read as reasonable in review, and quietly reintroduce the defect.
///
/// So the probe gets exactly one caller, enforced the same crude way
/// `the_legacy_write_path_is_gone` enforces its rule: by grep, over the tree,
/// on non-comment lines only. `serve.rs` is the one file allowed to call it,
/// because that is where the bind happens.
#[test]
fn no_client_process_probes_for_the_host_it_prints() {
    use std::path::Path;

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

    let mut found = Vec::new();
    for (path, text) in &files {
        let relative = path
            .rsplit_once("/src/")
            .map_or(path.as_str(), |(_, rest)| rest);
        // Where the probe is defined, and the one place a bind may call it.
        if relative == "daemon/tailnet.rs" || relative == "daemon/serve.rs" {
            continue;
        }
        for (number, line) in text.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            if code.contains("tailnet_identity") || code.contains("reachable_host") {
                found.push(format!("{relative}:{}: {}", number + 1, code.trim()));
            }
        }
    }

    assert!(
        found.is_empty(),
        "a host to advertise must be read from what the daemon bound \
         (`DaemonInfo::dashboard_url`), never derived from a probe of this machine — \
         that is SH-110, and `reachable_host` was deleted for it:\n  {}",
        found.join("\n  ")
    );
}

/// **The daemon never prompts, and this is what keeps it true.**
///
/// The whole architecture rests on one sentence: the work runs in a process
/// with no terminal and no way to reach one, so every question is asked by the
/// client and travels as an ordinary request. A second prompting site added
/// without a guard is how that becomes three, and the failure is silent — a
/// daemon sitting at a `dialoguer::Select` looks exactly like a slow command.
///
/// # Two axes, because one of them misses the worst offender
///
/// A `stdin()` grep alone used to pass while `src/github/conflict.rs` sat at an
/// interactive menu: `dialoguer` reads the terminal itself and never names
/// `std::io::stdin`. So this checks for both, and matches the *full* path
/// `std::io::stdin` so that `Ctx::stdin()` — an accessor for input the envelope
/// carried, not a process read — does not false-positive.
///
/// # The allowlist ships populated, and every entry names its story
///
/// Shipping it red would have meant weakening the assertion or fixing four
/// unrelated defects inside a story about project verbs. Shipping it populated
/// makes each existing violation a *recorded, story-linked exemption* instead
/// of a silent hole, and a new one a deliberate edit that shows up in review.
/// Removing a violation shrinks the list, which is why the count is asserted —
/// five to four when SH-152 deleted the menu in `src/github/conflict.rs`, then
/// four to three when SH-153 deleted the last three sites, in
/// `src/github/initial.rs`.
///
/// `src/service/questionnaire.rs` and `src/service/github_setup.rs` are
/// deliberately **not** here: both take `impl BufRead` and never name stdin at
/// all, which is the shape a prompting module is supposed to have.
///
/// # A live exemption and a stale one look identical to a name-only list
///
/// The original single `ALLOWED` list could check that no file outside it
/// prompts, and that its length matched a hand-counted expectation — it could
/// not check whether an entry still *needed* to be there. An allowlisted file
/// that later lost its last prompt stayed exempted silently; only a human
/// re-reading the ledger noticed (SH-194). The list is now two: `LEGITIMATE`
/// for the two sites that prompt by design and will not change without a
/// design change, and `EXEMPTED` for filed defects awaiting their story. Only
/// `EXEMPTED` entries are checked for staleness, immediately below the breach
/// check — removing the last prompt from an exempted file now turns this
/// suite red until the entry is deleted too.
#[test]
fn every_interactive_prompt_is_in_the_allowlist() {
    use std::path::Path;

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

    /// The two ways a file under `src/` can talk to a terminal.
    const PROMPTS: [&str; 3] = ["std::io::stdin", ".interact()", ".interact_text()"];

    /// Sites that prompt by design and always will — never a story's target.
    ///
    /// * `src/main.rs` — the client, and the one legitimate site. It owns the
    ///   `IsTerminal` decision for every prompt in the program.
    /// * `src/invoke.rs` — `Ctx`'s envelope-stdin fallback, documented as
    ///   deliberate for the TUI and in-process callers.
    const LEGITIMATE: [&str; 2] = ["src/main.rs", "src/invoke.rs"];

    /// Filed defects awaiting their story, exempted only as long as they still
    /// prompt. `every_exempted_entry_still_prompts` (below) fails the moment an
    /// entry's last `PROMPTS` token is removed, so a fix that forgets to delete
    /// its own exemption turns this suite red instead of leaving a silent,
    /// unenforced hole (SH-194).
    ///
    /// Empty right now: `src/service/story.rs` was the third entry LEGITIMATE
    /// ever needed to cover and is gone (SH-154's `reopen_plan`);
    /// `src/github/conflict.rs` was the fourth entry this list ever held, and
    /// the first it lost, when its `.interact().unwrap_or(2)` menu was deleted
    /// rather than guarded (SH-152); `src/github/initial.rs` was the second
    /// entry lost, three sites at once, replaced by `run_initial_setup`
    /// returning a plan and `src/service/github_setup.rs` asking the question
    /// from the one process with a terminal (SH-153).
    const EXEMPTED: [&str; 0] = [];

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&root, &mut files);
    assert!(
        files.len() > 20,
        "expected the whole tree, got {}",
        files.len()
    );

    fn still_prompts(text: &str) -> bool {
        text.lines().any(|line| {
            let code = line.trim_start();
            !code.starts_with("//") && PROMPTS.iter().any(|prompt| code.contains(prompt))
        })
    }

    let mut breaches = Vec::new();
    for (path, text) in &files {
        let relative = path
            .rsplit_once("/src/")
            .map(|(_, rest)| format!("src/{rest}"))
            .unwrap_or_else(|| path.clone());
        if LEGITIMATE.contains(&relative.as_str()) || EXEMPTED.contains(&relative.as_str()) {
            continue;
        }
        for (number, line) in text.lines().enumerate() {
            // A doc comment may describe a prompt; it cannot perform one.
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            for prompt in PROMPTS {
                if code.contains(prompt) {
                    breaches.push(format!("{relative}:{}: {}", number + 1, code.trim()));
                }
            }
        }
    }

    assert!(
        breaches.is_empty(),
        "the daemon never prompts, so a prompt under `src/` belongs in `main.rs` or in \
         LEGITIMATE/EXEMPTED above with the story that will remove it:\n  {}",
        breaches.join("\n  ")
    );

    for exempted in EXEMPTED {
        let (_, text) = files
            .iter()
            .find(|(path, _)| {
                path.rsplit_once("/src/")
                    .map(|(_, rest)| format!("src/{rest}"))
                    .as_deref()
                    == Some(exempted)
            })
            .unwrap_or_else(|| panic!("EXEMPTED names {exempted}, which is not under src/"));
        assert!(
            still_prompts(text),
            "{exempted} is in EXEMPTED but no longer contains a PROMPTS token — its defect is \
             already fixed, so the exemption is stale; delete this entry",
        );
    }

    // Asserted so that changing either list is a deliberate edit here.
    // LEGITIMATE names sites that prompt by design and should not change without
    // a design change; EXEMPTED is a ledger of filed defects and should only
    // ever get shorter — SH-154 closed the last one, and SH-194's own fix left
    // it at zero.
    assert_eq!(
        LEGITIMATE.len(),
        2,
        "LEGITIMATE changed; it names the sites that prompt by design"
    );
    assert_eq!(
        EXEMPTED.len(),
        0,
        "EXEMPTED changed; each entry is a filed exemption, so adding one needs a story and \
         removing one needs the defect to be gone"
    );
}

/// The resolution index is **gone**, and no code under `src/` can reach it.
///
/// SH-119's first acceptance criterion, in the shape that can be enforced: "no
/// symbol listed above remains in `src/`, and `grep project_paths src/` is
/// empty". The literal grep cannot be empty — migration 8 is *named* for the
/// table it drops, and several comments say what used to be where — so what is
/// checked is the thing the criterion means: that no line of code names the
/// deleted API. Comments are stripped before the check, which is exactly the
/// distinction between a mention and a call.
///
/// Crude on purpose, and for the same reasons `the_legacy_write_path_is_gone`
/// gives: it needs no build graph, and unlike a visibility change it cannot be
/// satisfied by a re-export. The table itself is checked where it can be — a
/// migrated store has no `project_paths` object at all
/// (`tests/store_migrations.rs::a_migrated_store_has_no_resolution_index_left`).
#[test]
fn the_resolution_index_is_gone() {
    use std::path::Path;

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

    /// A line with any `//` comment removed — including doc comments, which
    /// start with the same two characters.
    fn code(line: &str) -> &str {
        line.split_once("//").map_or(line, |(before, _)| before)
    }

    // Every name the index was reached by. `project_paths` itself is matched as
    // a *call* (`.project_paths(`) rather than as a word, because migration 8's
    // `name:` and `include_str!` both spell the table it deletes.
    const FORBIDDEN: [&str; 6] = [
        "touch_project_path",
        "forget_project_path",
        "project_by_path",
        "ProjectPathRecord",
        "PathKind",
        ".project_paths(",
    ];

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&root, &mut files);
    assert!(
        files.len() > 20,
        "expected the whole tree, got {}",
        files.len()
    );

    let mut breaches = Vec::new();
    for (path, text) in &files {
        for (number, line) in text.lines().enumerate() {
            for name in FORBIDDEN {
                if code(line).contains(name) {
                    breaches.push(format!("{path}:{}: {name}", number + 1));
                }
            }
        }
    }
    assert!(
        breaches.is_empty(),
        "the recorded-path index is deleted (SH-119). A project is identified by the \
         selector, its committed pointer file, or its registered origin — never by the \
         directory a command was run in:\n  {}",
        breaches.join("\n  ")
    );
}

/// `story tui` opens no store of its own (SH-150) — it is a client of the
/// daemon like every other command, through [`storyhook::invoke::HttpInvoker`].
///
/// The assertion is narrow on purpose: `crate::invoke::open_store` is the
/// specific function that made the TUI a second writer *and* a second
/// migrator — it runs `Store::migrate` and legacy-registry adoption behind a
/// pre-migration backup, unsupervised by the version/exe/mtime handshake
/// every other route to the store passes through
/// (`daemon::lifecycle::usable`). A grep for it, rather than for
/// `SqliteStore::open` more broadly, is deliberate: `src/tui/app.rs`,
/// `src/tui/data.rs` and `src/tui/event.rs` all open a `SqliteStore` directly
/// in their own `#[cfg(test)]` fixtures — to build a project the same way
/// `tests/tui_integration.rs` and `tests/tui_undo.rs` do, or (in
/// `event.rs`'s case) to write from a connection standing in for another
/// process — and none of that is the store handle this story removed. Same
/// idiom as `the_legacy_write_path_is_gone`: a grep needs no build graph and,
/// unlike a visibility change, cannot be satisfied by a re-export.
#[test]
fn the_tui_opens_no_store_of_its_own() {
    use std::path::Path;

    fn sources(dir: &Path, into: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).expect("reading src/tui/") {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                sources(&path, into);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let text = std::fs::read_to_string(&path).expect("reading a source file");
                into.push((path.to_string_lossy().into_owned(), text));
            }
        }
    }

    fn code(line: &str) -> &str {
        line.split_once("//").map_or(line, |(before, _)| before)
    }

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tui");
    let mut files = Vec::new();
    sources(&root, &mut files);
    assert!(
        files.len() > 5,
        "expected the whole src/tui tree, got {}",
        files.len()
    );

    let mut breaches = Vec::new();
    for (path, text) in &files {
        for (number, line) in text.lines().enumerate() {
            if code(line).contains("open_store") {
                breaches.push(format!("{path}:{}: {}", number + 1, line.trim()));
            }
        }
    }
    assert!(
        breaches.is_empty(),
        "src/tui/ must reach the store only through Invoker, never by opening one — \
         found:\n  {}",
        breaches.join("\n  ")
    );
}
