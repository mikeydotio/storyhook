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

use storyhook::domain::{Member, StateDef, SuperState, TypeDef};
use storyhook::env::Environment;
use storyhook::service::{Clock, Ctx};
use storyhook::store::{
    NewProject, PathKind, ProjectId, SqliteStore, Store, WriteOps, diff_read_model,
};
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
    _dir: TempDir,
}

impl ServiceFixture {
    /// A fixture with the default catalog: `todo` (open), `in-progress` (open,
    /// active), `done` (closed); types `feature` and `bug`; no members.
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
                tx.touch_project_path(project, Path::new("/checkouts/fixture"), PathKind::Main)?;
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
        if std::thread::panicking() {
            return;
        }
        self.assert_no_drift();
    }
}

/// The default state set: two open states and one closed, with an active role.
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
            slug: "done".into(),
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
        },
        TypeDef {
            slug: "bug".into(),
            description: Some("Something is broken".into()),
        },
    ]
}
