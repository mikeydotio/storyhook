//! `CatalogService` — the dashboard's project list, over the store.
//!
//! The headline property is the one the legacy registry could not have: because
//! the catalog *is* the projects table, deregistering a checkout cannot lose
//! anything. The registry got that for free by holding no data; here it has to
//! be a rule, and these tests are the rule.
//!
//! The origin sweep's rule is younger and is stated on both sides of a commit
//! (SH-275): *no failure, wherever it lands, leaves an observable half-sweep*.
//! It is asserted in-process rather than through the CLI because arming a fault
//! is thread-local, and a `story` command's writer is a daemon in another
//! process — reachable only by killing it, which is `tests/crash_matrix.rs`'s
//! subject rather than this file's.

use std::path::Path;
use std::process::Command;

use storyhook::service::{CatalogService, InitOptions, OriginFinding, ProjectService};
use storyhook::store::fault::{FaultAction, arm};
use storyhook::store::{FaultPoint, ReadOps, SqliteStore, Store};
use storyhook_test_support::scratch_dir;

/// An empty store, and a scratch directory to anchor projects in.
fn store() -> (SqliteStore, tempfile::TempDir) {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).expect("opening the store");
    store.migrate().expect("migrating");
    (store, dir)
}

/// A checkout directory with a project initialized in it.
fn project(store: &SqliteStore, parent: &Path, name: &str) -> std::path::PathBuf {
    let root = parent.join(name);
    std::fs::create_dir_all(&root).expect("creating the checkout");
    ProjectService::new(store, &root)
        .init(&InitOptions {
            agents_md: false,
            ..InitOptions::default()
        })
        .expect("initializing");
    root.canonicalize().expect("canonicalizing")
}

fn git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("running git");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        cwd.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A project whose checkout owns an origin the store has never recorded — the
/// one finding [`CatalogService::register_found_origins`] ever writes for.
///
/// The order is the whole point and is not interchangeable: the project is
/// initialized **while the directory is still a plain directory**, so `init`
/// finds no origin to adopt, and only then does the directory become a git
/// repository with a remote. Reversing the two produces a project that already
/// holds its origin, which classifies as nothing at all and would leave every
/// test below passing over an empty sweep.
fn registrable(store: &SqliteStore, parent: &Path, name: &str, url: &str) -> std::path::PathBuf {
    let root = project(store, parent, name);
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["config", "user.email", "t@t"]);
    git(&root, &["config", "user.name", "t"]);
    git(&root, &["remote", "add", "origin", url]);
    root
}

/// Three projects, three distinct unregistered origins, and the assertion that
/// the sweep really does see all three.
///
/// Distinct urls on purpose: identical ones collide, and the second write would
/// answer [`OriginFinding::HeldBy`] and write nothing — which is SH-274's
/// subject, pinned at the CLI level, and would silently halve the number of
/// writes these tests are about.
fn three_registrable_projects() -> (SqliteStore, tempfile::TempDir) {
    let (store, dir) = store();
    for name in ["alpha", "bravo", "charlie"] {
        registrable(
            &store,
            dir.path(),
            name,
            &format!("https://github.com/acme/{name}.git"),
        );
    }
    assert_registrable_findings(&store, 3);
    (store, dir)
}

/// The precondition every test below states before it acts: the sweep has
/// exactly `expected` origins to write, and every one of them is writable.
///
/// A fixture that silently degrades — a checkout that stopped computing
/// `Owned`, a project that adopted its own origin at `init` — leaves an armed
/// test green over a sweep with nothing to do, which is the SH-258 failure mode
/// this repo has already paid for once. Asserting it here makes that loud.
fn assert_registrable_findings(store: &SqliteStore, expected: usize) {
    let found = CatalogService::new(store)
        .unregistered_origins()
        .expect("probing for unregistered origins");
    assert_eq!(
        found.len(),
        expected,
        "the fixture must offer exactly {expected} findings, or the tests below assert nothing"
    );
    for item in &found {
        assert!(
            matches!(item.finding, OriginFinding::Registrable(_)),
            "`{}` is not registrable, so no write would be attempted for it: {:?}",
            item.slug,
            item.finding
        );
    }
}

/// How many origins the store has recorded, across every project.
fn recorded_origins(store: &SqliteStore) -> usize {
    store
        .read(|tx| {
            let mut total = 0;
            for project in tx.projects()? {
                total += tx.project_remotes(project.id)?.len();
            }
            Ok(total)
        })
        .expect("counting recorded origins")
}

#[test]
fn an_empty_store_lists_nothing() {
    let (store, _dir) = store();
    assert!(
        CatalogService::new(&store)
            .list()
            .expect("listing")
            .is_empty()
    );
}

#[test]
fn init_puts_a_project_in_the_catalog_without_a_separate_register() {
    let (store, dir) = store();
    let root = project(&store, dir.path(), "alpha");

    let entries = CatalogService::new(&store).list().expect("listing");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, "alpha");
    assert_eq!(entries[0].path.as_deref(), Some(root.as_path()));
}

#[test]
fn the_catalog_lists_projects_in_slug_order() {
    let (store, dir) = store();
    project(&store, dir.path(), "zulu");
    project(&store, dir.path(), "alpha");
    project(&store, dir.path(), "mike");

    let ids: Vec<String> = CatalogService::new(&store)
        .list()
        .expect("listing")
        .into_iter()
        .map(|entry| entry.id)
        .collect();
    assert_eq!(ids, ["alpha", "mike", "zulu"]);
}

/// **The SH-275 discriminator**, and the only test here whose passing arm fires
/// the fault it arms.
///
/// The sweep used to open one write transaction *per registration*, so a
/// failure part-way along committed the earlier ones, propagated the error and
/// returned nothing — the operator was told nothing about registrations that
/// had actually landed. Partial mutation reported as total failure.
///
/// [`FaultPoint::AfterCommitBeforeAck`] is the lever, and no other point works:
/// it fires in the window *after* a commit, which is the only window in which
/// one transaction and N differ by **outcome** rather than merely by count.
/// `BeforeCommit` fires on every write, kills the first one, and answers zero
/// under both shapes — see the companion below, which says so about itself.
///
/// The assertion is `0` or `3` — never a strict subset — rather than a count of
/// transactions. A future reader who reintroduces a per-registration write for
/// any reason (reporting progress, most plausibly) puts this straight back to
/// `1`, and it fails naming the harm rather than the plumbing.
#[test]
fn a_failure_after_a_commit_never_leaves_an_observable_half_sweep() {
    let (store, _dir) = three_registrable_projects();

    let outcome = {
        let _fault = arm(
            FaultPoint::AfterCommitBeforeAck,
            FaultAction::Fail("acknowledgement lost".to_string()),
        );
        CatalogService::new(&store).register_found_origins()
    };

    let error = outcome.expect_err("the armed fault must fail the sweep");
    assert!(
        error.to_string().contains("acknowledgement lost"),
        "the failure must be the injected one, or this test is reporting a broken fixture as \
         the defect: {error}"
    );

    let landed = recorded_origins(&store);
    assert!(
        landed == 0 || landed == 3,
        "a failed origin sweep left {landed} of 3 registrations committed — the sweep must be \
         one transaction (SH-275)"
    );
}

/// The rollback side of the same promise: before the commit, nothing.
///
/// **Not the discriminator, and its own doc comment is where that is recorded.**
/// It passes against a per-registration sweep too, because `BeforeCommit` fires
/// on the first write and no registration ever commits. It is kept because
/// "before the commit, none; after it, all" is the operational statement of
/// atomicity and half of it would otherwise go unasserted — and because it is
/// what fails if the report is ever built outside the transaction that earns it.
#[test]
fn a_failure_before_the_commit_registers_nothing() {
    let (store, _dir) = three_registrable_projects();

    let outcome = {
        let _fault = arm(
            FaultPoint::BeforeCommit,
            FaultAction::Fail("interrupted".to_string()),
        );
        CatalogService::new(&store).register_found_origins()
    };

    let error = outcome.expect_err("the armed fault must fail the sweep");
    assert!(
        error.to_string().contains("interrupted"),
        "the failure must be the injected one: {error}"
    );
    assert_eq!(
        recorded_origins(&store),
        0,
        "a sweep that rolled back must leave the store exactly as it found it"
    );
}

/// The ordinary run, which nothing else in the suite pins.
///
/// SH-274's anchor in `tests/project_path_hygiene.rs` covers one origin and a
/// collision; this covers several writes all succeeding — the case the single
/// transaction has to keep working, and the one a rollback-only pair of tests
/// would let regress to "registers nothing, always".
#[test]
fn a_sweep_records_every_registrable_origin_it_finds() {
    let (store, _dir) = three_registrable_projects();

    let sweep = CatalogService::new(&store)
        .register_found_origins()
        .expect("sweeping");

    let mut recorded: Vec<String> = sweep
        .recorded
        .iter()
        .map(|item| item.origin.raw().to_string())
        .collect();
    recorded.sort();
    assert_eq!(
        recorded,
        [
            "https://github.com/acme/alpha.git",
            "https://github.com/acme/bravo.git",
            "https://github.com/acme/charlie.git",
        ]
    );
    assert!(
        sweep.left_alone.is_empty(),
        "every finding was registrable and every write landed: {:?}",
        sweep.left_alone
    );
    assert_eq!(
        recorded_origins(&store),
        3,
        "the report must describe rows the store actually holds"
    );
}
