//! The shared legacy-versus-store harness.
//!
//! Two projects seeded from one catalog, one invocation driven through both,
//! and an assertion that the answers agree. It lives in its own module because
//! more than one differential file needs it: the story lifecycle has its own
//! rows, and so do the project, configuration and system families.
//!
//! # What is normalized, and why
//!
//! Exactly one thing: **timestamps**. Both legs read the system clock, they
//! run microseconds apart, and storyhook's timestamps have second precision —
//! so `created_at` agrees almost always and disagrees exactly when a test
//! straddles a second boundary. That is a flake, not a finding. Every
//! RFC3339-shaped string is replaced with a marker before comparison, in both
//! legs, by the same function.
//!
//! Nothing else is normalized. Ids, titles, states, superstates, comments,
//! relationships, labels, priorities, derived relationships, progress
//! rollups, flagged reasons, error messages, error variants and exit codes are
//! all compared verbatim.

#![allow(dead_code)]

use serde_json::{Value, json};
use storyhook::app;
use storyhook::cli::{CliOptions, Invocation};
use storyhook::domain::Member;
use storyhook::error::{AppError, WireError};
use storyhook::invoke::dispatch;
use storyhook::output::Response;
use storyhook::service::{Clock, Ctx};
use storyhook::storage;
use storyhook::store::{
    NewProject, PathKind, ProjectId, SqliteStore, Store, WriteOps, diff_read_model,
};
use storyhook_test_support::{Project, TestEnv, scratch_dir};
use tempfile::TempDir;

// --- the harness -----------------------------------------------------------

/// A legacy project and a store-backed project, seeded from the same catalog.
pub struct Differential {
    legacy: Project<'static>,
    store: SqliteStore,
    project: ProjectId,
    _dir: TempDir,
}

impl Differential {
    /// Two projects with the same states, types, and prefix.
    ///
    /// The store's catalog is *read out of the legacy project* rather than
    /// transcribed, so the two cannot drift apart the day `story init`'s
    /// defaults change.
    pub fn new() -> Self {
        Self::seeded(TestEnv::shared().project().build())
    }

    /// [`new`](Self::new), with the legacy project inside a real git
    /// repository.
    ///
    /// `commit-sync` shells out to `git` in the project directory, and both
    /// legs run against the *same* directory — so one repository, one log, and
    /// any disagreement is about what the two legs did with it rather than
    /// about what they read.
    pub fn with_git() -> Self {
        Self::seeded(TestEnv::shared().project().git().build())
    }

    /// Builds the store leg's project from the catalog the legacy leg already
    /// has.
    fn seeded(legacy: Project<'static>) -> Self {
        let root = legacy.path();
        let prefix = storage::load_project_prefix(root).expect("reading the legacy prefix");
        let states = storage::load_states(root).expect("reading the legacy states");
        let types = storage::load_types(root).expect("reading the legacy types");

        let dir = scratch_dir();
        let store = SqliteStore::open(dir.path().join("store.db")).expect("opening the store");
        store.migrate().expect("migrating the store");
        let project = store
            .write(|tx| {
                let project = tx.create_project(&NewProject {
                    uuid: "differential".into(),
                    slug: "differential".into(),
                    name: "differential".into(),
                    prefix,
                    created_at: "2026-01-01T00:00:00Z".into(),
                })?;
                tx.touch_project_path(project, root, PathKind::Main)?;
                tx.put_states(project, &states)?;
                tx.put_types(project, &types)?;
                Ok(project)
            })
            .expect("seeding the store project");

        Self {
            legacy,
            store,
            project,
            _dir: dir,
        }
    }

    /// Makes an empty commit with `subject` in the legacy project's
    /// repository.
    ///
    /// Empty on purpose: `commit-sync` reads subjects, so what a commit touches
    /// is irrelevant and a fixture that writes files would only add ways to
    /// fail.
    pub fn commit(&self, subject: &str) {
        let mut command = std::process::Command::new("git");
        command
            .current_dir(self.legacy.path())
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(["commit", "-q", "--allow-empty", "-m", subject]);
        TestEnv::shared().apply(&mut command);
        let output = command.output().expect("running git commit");
        assert!(
            output.status.success(),
            "`git commit -m {subject}` failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Adds the same member to both projects.
    pub fn add_member(&self, id: &str, github: Option<&str>) {
        let member = Member {
            id: id.to_string(),
            display_name: id.to_string(),
            email: None,
            github: github.map(str::to_string),
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };
        storage::store_member(self.legacy.path(), &member).expect("adding the legacy member");
        self.store
            .write(|tx| tx.put_member(self.project, &member))
            .expect("adding the store member");
    }

    /// The service context for the store leg.
    ///
    /// The system clock on purpose: the legacy leg has no other option, and a
    /// comparison between a pinned clock and a real one would only ever be
    /// testing the redaction.
    pub fn ctx(&self) -> Ctx<'_, SqliteStore> {
        Ctx::new(&self.store, self.project, self.legacy.path()).clock(Clock::System)
    }

    /// Runs one invocation through both legs and asserts they agree.
    pub fn step(&self, label: &str, invocation: Invocation) {
        let _ = self.step_inner(label, invocation);
    }

    /// [`step`](Self::step), handing back the id of the story the legacy leg
    /// answered with — the two legs having just been asserted to agree on it.
    pub fn step_id(&self, label: &str, invocation: Invocation) -> String {
        story_id(&self.step_inner(label, invocation))
    }

    pub fn step_inner(&self, label: &str, invocation: Invocation) -> Result<Response, AppError> {
        let legacy = app::run(
            self.legacy.path(),
            CliOptions {
                json: false,
                quiet: false,
                no_hooks: false,
                invocation: invocation.clone(),
            },
        );
        let new = dispatch(&self.ctx(), invocation);

        let left = canonical(&legacy);
        let right = canonical(&new);
        assert_eq!(
            left,
            right,
            "`{label}` diverged\n legacy: {}\n  store: {}",
            serde_json::to_string_pretty(&left).unwrap(),
            serde_json::to_string_pretty(&right).unwrap(),
        );
        legacy
    }

    /// Runs one invocation through the legacy leg only.
    ///
    /// For the handful of rows where the two legs are *expected* to differ and
    /// the divergence is the finding: each leg is driven separately and the
    /// difference asserted, rather than compared and normalized away.
    pub fn legacy_only(&self, invocation: Invocation) -> Result<Response, AppError> {
        app::run(
            self.legacy.path(),
            CliOptions {
                json: false,
                quiet: false,
                no_hooks: false,
                invocation,
            },
        )
    }

    /// Runs one invocation through the store leg only. See [`legacy_only`](Self::legacy_only).
    pub fn store_only(&self, invocation: Invocation) -> Result<Response, AppError> {
        dispatch(&self.ctx(), invocation)
    }

    /// The legacy leg's project directory.
    pub fn legacy_path(&self) -> &std::path::Path {
        self.legacy.path()
    }

    /// The store leg's store and project, for assertions about stored state.
    pub fn store(&self) -> (&SqliteStore, ProjectId) {
        (&self.store, self.project)
    }

    /// Compares the *view* of one story through `story show`.
    ///
    /// The view is the thing almost every write answers with, and asserting it
    /// after a write is how a divergence in stored state gets caught even when
    /// the write's own answer happened to agree. It goes through `dispatch`
    /// like any other row — the query wave gave `show` an arm — so this is
    /// [`step`](Self::step) with a label that names the story.
    pub fn show(&self, label: &str, id: &str) {
        self.step(
            &format!("{label} (show {id})"),
            Invocation::Show { id: id.to_string() },
        );
    }

    /// Fails unless the store leg's read model still matches its events.
    pub fn assert_no_drift(&self) {
        let diff = diff_read_model(&self.store, self.project).expect("diffing the read model");
        assert!(
            diff.is_clean() && diff.asymmetric_relations.is_empty(),
            "the store leg drifted:\n{}",
            diff.describe()
        );
    }
}

impl Drop for Differential {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            self.assert_no_drift();
        }
    }
}

/// One comparable document per outcome, success or failure.
///
/// Errors are compared as their wire variant *and* their rendered message
/// *and* their exit code, which together are everything a caller can observe:
/// the variant is what an RPC client switches on, the message is what a human
/// reads, and the exit code is what a script tests.
pub fn canonical(result: &Result<Response, AppError>) -> Value {
    match result {
        Ok(response) => json!({
            "ok": redact_timestamps(serde_json::to_value(response).expect("serializing")),
        }),
        Err(error) => json!({
            "error": {
                "wire": serde_json::to_value(WireError::from(error)).expect("serializing"),
                "message": error.to_string(),
                "exit_code": error.exit_code(),
            },
        }),
    }
}

/// Replaces every RFC3339-shaped string with a marker.
///
/// The *only* deliberate normalization in this file. Both legs stamp their own
/// events from the system clock at second precision, so any test that happens
/// to straddle a second boundary would otherwise fail for a reason that has
/// nothing to do with behaviour.
pub fn redact_timestamps(value: Value) -> Value {
    match value {
        Value::String(text) if is_timestamp(&text) => Value::String("<timestamp>".into()),
        Value::Array(items) => Value::Array(items.into_iter().map(redact_timestamps).collect()),
        Value::Object(fields) => Value::Object(
            fields
                .into_iter()
                .map(|(key, value)| (key, redact_timestamps(value)))
                .collect(),
        ),
        other => other,
    }
}

/// `2026-01-01T00:00:00Z` and its offset spellings, and nothing else.
fn is_timestamp(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() < 20 {
        return false;
    }
    let shape = |index: usize, expected: char| bytes.get(index) == Some(&(expected as u8));
    let digits = |range: std::ops::Range<usize>| {
        range.clone().all(|i| bytes[i].is_ascii_digit()) && range.end <= bytes.len()
    };
    digits(0..4)
        && shape(4, '-')
        && digits(5..7)
        && shape(7, '-')
        && digits(8..10)
        && shape(10, 'T')
        && digits(11..13)
        && shape(13, ':')
        && digits(14..16)
        && shape(16, ':')
        && digits(17..19)
}

/// The id of the story a `Response::Story` describes.
pub fn story_id(response: &Result<Response, AppError>) -> String {
    match response {
        Ok(Response::Story(view)) => view.story.id.clone(),
        other => panic!("expected a story response, got {other:?}"),
    }
}
