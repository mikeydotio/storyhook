//! A store-backed project fixture for service tests — with a drift guard.
//!
//! # Why the guard
//!
//! The read model is folded from events inside the transaction that appends
//! them, and [`storyhook::store::diff_read_model`] can prove the two still
//! agree. That oracle is also `story doctor`'s integrity check, which means it
//! has to be *trusted* before it ships — and an oracle exercised only by the
//! tests written to exercise it is not trusted, it is merely green.
//!
//! So every fixture built here checks itself on the way out. [`ServiceFixture`]
//! runs the diff over every project it touched when it drops, and fails the
//! test if the read model has drifted from its events. A service test that
//! never mentions drift is still a drift test, and the oracle accumulates a few
//! hundred independent confirmations per run instead of a handful.
//!
//! The check is skipped while a panic is unwinding: a test that has already
//! failed should report the assertion it failed on, not a drift check that
//! failed because of it.

use std::path::Path;

use storyhook::domain::remote::RemoteUrl;
use storyhook::domain::{Member, StateDef, SuperState, TypeDef};
use storyhook::env::Environment;
use storyhook::service::{Clock, Ctx};
use storyhook::store::{NewProject, ProjectId, SqliteStore, Store, WriteOps, diff_read_model};
use tempfile::TempDir;

use crate::scratch::scratch_dir;

/// The fixed instant every fixture's clock reports unless a test changes it.
///
/// A pinned clock is what lets a service test assert on a timestamp at all;
/// the differential harness, which has to compare against a legacy run using
/// the real clock, redacts timestamps instead.
pub const FIXTURE_NOW: &str = "2026-01-01T00:00:00Z";

/// A migrated store, a seeded project, and a drift check on the way out.
pub struct ServiceFixture {
    store: SqliteStore,
    project: ProjectId,
    cwd: TempDir,
    clock: Clock,
    env: Environment,
    /// Set by [`ServiceFixture::expects_drift`]; disarms the drop-time check.
    ///
    /// Atomic rather than a `Cell` because two concurrency tests share a
    /// `&ServiceFixture` across `thread::scope` (`tests/service_story.rs`,
    /// `tests/service_config.rs`), and a `Cell` would make the whole fixture
    /// `!Sync`.
    drift_expected: std::sync::atomic::AtomicBool,
    _dir: TempDir,
}

impl ServiceFixture {
    /// A fixture with the default catalog: `todo` (open), `in-progress` (open,
    /// active), `verifying` and `blocked` (open), `done` and `closed` (both closed); types
    /// `feature` and `bug`; no members.
    #[must_use]
    pub fn new() -> Self {
        Self::with_states(&default_states())
    }

    /// A fixture whose project has exactly these states, in this order.
    #[must_use]
    pub fn with_states(states: &[StateDef]) -> Self {
        let dir = scratch_dir();
        // The environment is rooted in the fixture's own directory, so a
        // service that writes beside the store — github-sync's backups, the
        // daemon's runtime files — writes there too rather than into the
        // developer's real state home. An in-process test cannot redirect an
        // environment variable; this is how it redirects the same thing.
        let env = Environment::at(dir.path());
        let store = SqliteStore::open(env.store_path()).expect("opening the fixture store");
        store.migrate().expect("migrating the fixture store");
        let project = store
            .write(|tx| {
                let project = tx.create_project(&NewProject {
                    uuid: "fixture-uuid".into(),
                    slug: "fixture".into(),
                    name: "fixture".into(),
                    prefix: "SH".into(),
                    created_at: FIXTURE_NOW.into(),
                })?;
                tx.set_checkout_path(project, Some(Path::new("/checkouts/fixture")))?;
                tx.put_states(project, states)?;
                tx.put_types(project, &default_types())?;
                Ok(project)
            })
            .expect("seeding the fixture project");

        Self {
            store,
            project,
            cwd: scratch_dir(),
            clock: Clock::Fixed(FIXTURE_NOW.to_string()),
            env,
            drift_expected: std::sync::atomic::AtomicBool::new(false),
            _dir: dir,
        }
    }

    /// Adds a member the assignment paths can find.
    pub fn add_member(&self, id: &str, display_name: &str, github: Option<&str>) {
        self.store
            .write(|tx| {
                tx.put_member(
                    self.project,
                    &Member {
                        id: id.to_string(),
                        display_name: display_name.to_string(),
                        email: None,
                        github: github.map(str::to_string),
                        created_at: FIXTURE_NOW.to_string(),
                    },
                )
            })
            .expect("adding a member");
    }

    /// Registers a git origin on the fixture's project.
    ///
    /// The store-backed replacement for `story project link origin`, used by
    /// every test that needs `pr-check`/`link-pr`'s cross-repo guard to see a
    /// registered GitHub repository since SH-408 stopped both readers from
    /// consulting the (deleted) sync-engine config document — they read
    /// `ReadOps::project_remotes` now, which is exactly what this writes.
    pub fn link_origin(&self, url: &str) {
        let remote = RemoteUrl::normalize(url).expect("a well-formed remote url");
        self.store
            .write(|tx| tx.link_remote(self.project, &remote, FIXTURE_NOW))
            .expect("registering an origin");
    }

    /// Pins the clock every context this fixture builds will report.
    pub fn set_clock(&mut self, clock: Clock) {
        self.clock = clock;
    }

    /// The store behind the fixture.
    #[must_use]
    pub fn store(&self) -> &SqliteStore {
        &self.store
    }

    /// The fixture project's id.
    #[must_use]
    pub fn project(&self) -> ProjectId {
        self.project
    }

    /// The environment every context this fixture builds runs under — the one
    /// place to look for anything a service writes outside the store.
    #[must_use]
    pub fn env(&self) -> &Environment {
        &self.env
    }

    /// The directory contexts run from — where `hooks.toml` goes.
    #[must_use]
    pub fn cwd(&self) -> &Path {
        self.cwd.path()
    }

    /// A fresh service context over this fixture.
    ///
    /// Rebuilt per call rather than stored, because a `Ctx` borrows the store
    /// and a struct holding both would be self-referential.
    #[must_use]
    pub fn ctx(&self) -> Ctx<'_, SqliteStore> {
        Ctx::new(&self.store, self.project, self.cwd.path(), self.env.clone())
            .clock(self.clock.clone())
    }

    /// Installs a `hooks.toml` in the fixture's working directory.
    pub fn write_hooks_toml(&self, contents: &str) {
        let dir = self.cwd.path().join(".storyhook");
        std::fs::create_dir_all(&dir).expect("creating .storyhook");
        std::fs::write(dir.join("hooks.toml"), contents).expect("writing hooks.toml");
    }

    /// Disarms the drop-time drift check for a test whose subject *is*
    /// irreparable drift.
    ///
    /// Nearly every fixture here damages the read model and then repairs it, so
    /// the drop-time check is the guarantee that a test which forgot to repair
    /// says so. A handful cannot repair by construction: a story carrying an
    /// event this build cannot decode is withheld from `repair_read_model`
    /// entirely (SH-410), so a stale or missing row on one stays diverged
    /// however many times `--fix` runs — that is the behaviour under test, not
    /// a test that forgot.
    ///
    /// Deliberately explicit and deliberately narrow: it names the exemption at
    /// the call site, where a reader can weigh it, rather than making the whole
    /// check opt-in.
    pub fn expects_drift(&self) {
        self.drift_expected
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Fails unless the read model still matches a fresh fold of the events.
    ///
    /// Called automatically on drop; public so a test can also assert it at a
    /// chosen point — for instance immediately after the operation under test,
    /// before any teardown could obscure which write was responsible.
    pub fn assert_no_drift(&self) {
        let diff = diff_read_model(&self.store, self.project).expect("diffing the read model");
        assert!(
            diff.is_clean(),
            "the read model has drifted from its events:\n{}",
            diff.describe()
        );
        assert!(
            diff.asymmetric_relations.is_empty(),
            "a relation is asserted by one story and not by the other:\n{}",
            diff.describe()
        );
    }
}

impl Default for ServiceFixture {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ServiceFixture {
    fn drop(&mut self) {
        if std::thread::panicking()
            || self
                .drift_expected
                .load(std::sync::atomic::Ordering::Relaxed)
        {
            return;
        }
        self.assert_no_drift();
    }
}

/// The default state set: three open states and one closed, with an active
/// role.
///
/// Carries `blocked` because [`storyhook::domain::REQUIRED_STATES`] requires it
/// of every project (SH-125). A fixture seeded below that floor cannot have its
/// catalog edited at all, so a state test written against one would be testing
/// the refusal rather than the edit.
#[must_use]
pub fn default_states() -> Vec<StateDef> {
    vec![
        StateDef {
            slug: "todo".into(),
            super_state: SuperState::Open,
            role: None,
            description: Some("Not started".into()),
        },
        StateDef {
            slug: "in-progress".into(),
            super_state: SuperState::Open,
            role: Some("active".into()),
            description: None,
        },
        StateDef {
            slug: "verifying".into(),
            super_state: SuperState::Open,
            role: None,
            description: None,
        },
        StateDef {
            slug: "blocked".into(),
            super_state: SuperState::Open,
            role: None,
            description: None,
        },
        StateDef {
            slug: "done".into(),
            super_state: SuperState::Closed,
            role: None,
            description: None,
        },
        StateDef {
            slug: "closed".into(),
            super_state: SuperState::Closed,
            role: None,
            description: None,
        },
    ]
}

/// The default type set.
#[must_use]
pub fn default_types() -> Vec<TypeDef> {
    vec![
        TypeDef {
            slug: "feature".into(),
            description: None,
            emoji: None,
        },
        TypeDef {
            slug: "bug".into(),
            description: Some("Something is broken".into()),
            emoji: None,
        },
    ]
}
