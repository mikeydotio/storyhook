//! Legacy versus store, invocation for invocation.
//!
//! The structural byte-compatibility argument is that `app::run` already
//! returns a `Response`/`AppError` envelope and every renderer lives
//! client-side, so a new implementation that produces the same envelope
//! produces the same output *by construction*. This file is what makes that
//! argument checkable: it drives the same `Invocation` sequence through
//! `app::run` on a legacy `.storyhook` project and through `invoke::dispatch`
//! on a store-backed project seeded from the same catalog, and compares the
//! two answers after every step.
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

use std::collections::BTreeMap;

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
struct Differential {
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
    fn new() -> Self {
        let legacy = TestEnv::shared().project().build();
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

    /// Adds the same member to both projects.
    fn add_member(&self, id: &str, github: Option<&str>) {
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
    fn ctx(&self) -> Ctx<'_, SqliteStore> {
        Ctx::new(&self.store, self.project, self.legacy.path()).clock(Clock::System)
    }

    /// Runs one invocation through both legs and asserts they agree.
    fn step(&self, label: &str, invocation: Invocation) {
        let _ = self.step_inner(label, invocation);
    }

    /// [`step`](Self::step), handing back the id of the story the legacy leg
    /// answered with — the two legs having just been asserted to agree on it.
    fn step_id(&self, label: &str, invocation: Invocation) -> String {
        story_id(&self.step_inner(label, invocation))
    }

    fn step_inner(&self, label: &str, invocation: Invocation) -> Result<Response, AppError> {
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

    /// Compares the *view* of one story, without going through dispatch.
    ///
    /// `story show` belongs to the query service a later wave builds, so there
    /// is no dispatch arm for it yet. The view it renders is nonetheless the
    /// thing almost every write in this wave answers with, and asserting it
    /// after a write is how a divergence in stored state gets caught even when
    /// the write's own answer happened to agree. So this leg calls the
    /// service's own view builder directly; the envelope compared is the same
    /// `Response::Story` in both cases.
    fn show(&self, label: &str, id: &str) {
        let legacy = app::run(
            self.legacy.path(),
            CliOptions {
                json: false,
                quiet: false,
                no_hooks: false,
                invocation: Invocation::Show { id: id.to_string() },
            },
        );
        let new = self.ctx().story_view(id);
        let left = canonical(&legacy);
        let right = canonical(&new);
        assert_eq!(
            left,
            right,
            "`{label}` (show {id}) diverged\n legacy: {}\n  store: {}",
            serde_json::to_string_pretty(&left).unwrap(),
            serde_json::to_string_pretty(&right).unwrap(),
        );
    }

    /// Fails unless the store leg's read model still matches its events.
    fn assert_no_drift(&self) {
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
fn canonical(result: &Result<Response, AppError>) -> Value {
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
fn redact_timestamps(value: Value) -> Value {
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
fn story_id(response: &Result<Response, AppError>) -> String {
    match response {
        Ok(Response::Story(view)) => view.story.id.clone(),
        other => panic!("expected a story response, got {other:?}"),
    }
}

/// A `story new` invocation with only a title.
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

/// A `story set` invocation with every field absent.
fn set_fields(id: &str) -> Invocation {
    Invocation::SetFields {
        id: id.to_string(),
        title: None,
        state: None,
        priority: None,
        assignee: None,
        labels: None,
        blocked: None,
        unblocked: false,
        json: None,
        story_type: None,
        description: None,
    }
}

// --- creation --------------------------------------------------------------

#[test]
fn creating_stories_agrees_including_the_ids_allocated() {
    let differential = Differential::new();
    for index in 1..=3 {
        let id = differential.step_id("new", new_story(&format!("story {index}")));
        assert_eq!(id, format!("SH-{index}"));
    }
}

#[test]
fn creating_an_enriched_story_agrees_field_for_field() {
    let differential = Differential::new();
    differential.add_member("ada", Some("ada-gh"));
    differential.step(
        "new --everything",
        Invocation::New {
            title: "enriched".into(),
            state: Some("in-progress".into()),
            story_type: Some("bug".into()),
            description: Some("a description".into()),
            priority: Some("high".into()),
            labels: Some(vec!["z".into(), "a".into(), "z".into()]),
            assignee: Some("ada-gh".into()),
        },
    );
}

#[test]
fn creation_rejections_agree() {
    let differential = Differential::new();
    let cases = [
        (
            "closed initial state",
            Invocation::New {
                title: "x".into(),
                state: Some("done".into()),
                story_type: None,
                description: None,
                priority: None,
                labels: None,
                assignee: None,
            },
        ),
        (
            "undefined initial state",
            Invocation::New {
                title: "x".into(),
                state: Some("limbo".into()),
                story_type: None,
                description: None,
                priority: None,
                labels: None,
                assignee: None,
            },
        ),
        (
            "unknown type",
            Invocation::New {
                title: "x".into(),
                state: None,
                story_type: Some("not-a-type".into()),
                description: None,
                priority: None,
                labels: None,
                assignee: None,
            },
        ),
        (
            "invalid priority",
            Invocation::New {
                title: "x".into(),
                state: None,
                story_type: None,
                description: None,
                priority: Some("urgent".into()),
                labels: None,
                assignee: None,
            },
        ),
        (
            "unknown assignee",
            Invocation::New {
                title: "x".into(),
                state: None,
                story_type: None,
                description: None,
                priority: None,
                labels: None,
                assignee: Some("nobody".into()),
            },
        ),
    ];
    for (label, invocation) in cases {
        differential.step(label, invocation);
    }
}

#[test]
fn a_rejection_that_never_reached_storage_burns_no_story_number_in_either_leg() {
    // Type, priority and assignee are all validated before the legacy path
    // asks for an id, so both legs agree that a rejected creation costs
    // nothing. `state` is the exception, and it has its own test below.
    let differential = Differential::new();
    differential.add_member("ada", None);
    let rejections = [Some("not-a-type"), None];
    for story_type in rejections {
        differential.step(
            "rejected creation",
            Invocation::New {
                title: "x".into(),
                state: None,
                story_type: story_type.map(str::to_string),
                description: None,
                priority: story_type.map_or(Some("urgent".to_string()), |_| None),
                labels: None,
                assignee: None,
            },
        );
    }
    let id = differential.step_id("the first real one", new_story("real"));
    assert_eq!(id, "SH-1");
}

/// A rejected `--state` costs the legacy tracker a story number. The store
/// does not have that defect, so this is the one place in the wave where the
/// two legs deliberately disagree.
///
/// The mechanism is an ordering bug in `storage::create_story_with_events`: it
/// calls `next_story_id`, which increments the on-disk counter, and validates
/// the requested state *afterwards*. So `story new --state nonsense` fails and
/// still consumes `SH-7`, leaving a permanent gap in the numbering. Every other
/// enrichment field is validated in `app.rs` before that call, which is why
/// only `--state` shows it.
///
/// The store allocates inside the transaction that uses the number, so a
/// rollback returns it. Pinned rather than normalized: this is a real,
/// user-visible improvement, and it should be a deliberate line in the flip's
/// behaviour-change notes rather than a surprise.
#[test]
fn a_rejected_initial_state_burns_a_story_number_in_the_legacy_leg_only() {
    let differential = Differential::new();
    differential.step(
        "rejected state",
        Invocation::New {
            title: "x".into(),
            state: Some("limbo".into()),
            story_type: None,
            description: None,
            priority: None,
            labels: None,
            assignee: None,
        },
    );

    let legacy = app::run(
        differential.legacy.path(),
        CliOptions {
            json: false,
            quiet: false,
            no_hooks: false,
            invocation: new_story("after the rejection"),
        },
    );
    let new = dispatch(&differential.ctx(), new_story("after the rejection"));

    assert_eq!(
        story_id(&legacy),
        "SH-2",
        "the legacy path burns the number it allocated before validating"
    );
    assert_eq!(
        story_id(&new),
        "SH-1",
        "the store returns a rolled-back allocation to the pool"
    );
}

#[test]
fn a_blank_description_and_an_empty_label_list_agree() {
    let differential = Differential::new();
    differential.step(
        "new --description '   ' --labels ''",
        Invocation::New {
            title: "blank".into(),
            state: None,
            story_type: None,
            description: Some("   ".into()),
            priority: None,
            labels: Some(Vec::new()),
            assignee: None,
        },
    );
}

// --- single-field edits ----------------------------------------------------

#[test]
fn comments_agree() {
    let differential = Differential::new();
    let id = differential.step_id("new", new_story("commented"));
    differential.step(
        "comment",
        Invocation::Comment {
            id: id.clone(),
            text: "first".into(),
        },
    );
    differential.step(
        "comment again",
        Invocation::Comment {
            id,
            text: "second".into(),
        },
    );
}

#[test]
fn assignment_agrees_by_id_and_by_handle() {
    let differential = Differential::new();
    differential.add_member("ada", Some("ada-gh"));
    let id = differential.step_id("new", new_story("assignable"));
    differential.step(
        "assign by id",
        Invocation::Assign {
            id: id.clone(),
            member: "ada".into(),
        },
    );
    differential.step(
        "assign by handle",
        Invocation::Assign {
            id: id.clone(),
            member: "ada-gh".into(),
        },
    );
    differential.step(
        "assign to a stranger",
        Invocation::Assign {
            id,
            member: "nobody".into(),
        },
    );
}

#[test]
fn priority_edits_agree_including_the_rejection() {
    let differential = Differential::new();
    let id = differential.step_id("new", new_story("prioritised"));
    for priority in ["critical", "high", "medium", "low", "none", "urgent"] {
        differential.step(
            priority,
            Invocation::SetPriority {
                id: id.clone(),
                priority: priority.into(),
            },
        );
    }
}

#[test]
fn label_edits_agree() {
    let differential = Differential::new();
    let id = differential.step_id("new", new_story("labelled"));
    let cases: [(Vec<String>, Vec<String>); 4] = [
        (vec!["zeta".into(), "alpha".into()], vec![]),
        (vec!["alpha".into()], vec![]),
        (vec!["mid".into()], vec!["zeta".into()]),
        (vec![], vec!["never-there".into()]),
    ];
    for (add, remove) in cases {
        differential.step(
            "labels",
            Invocation::SetLabels {
                id: id.clone(),
                add,
                remove,
            },
        );
    }
}

#[test]
fn awaiting_edits_agree() {
    let differential = Differential::new();
    let id = differential.step_id("new", new_story("blocked"));
    differential.step(
        "clear before set",
        Invocation::ClearAwaiting { id: id.clone() },
    );
    differential.step(
        "set awaiting",
        Invocation::SetAwaiting {
            id: id.clone(),
            awaiting: "  review  ".into(),
        },
    );
    differential.step(
        "set awaiting blank",
        Invocation::SetAwaiting {
            id: id.clone(),
            awaiting: "   ".into(),
        },
    );
    differential.step("clear awaiting", Invocation::ClearAwaiting { id });
}

#[test]
fn edits_to_a_story_that_does_not_exist_agree() {
    let differential = Differential::new();
    differential.step("new", new_story("a real one"));
    let missing = "SH-99";
    differential.step(
        "comment on a ghost",
        Invocation::Comment {
            id: missing.into(),
            text: "hello".into(),
        },
    );
    differential.step(
        "assign a ghost",
        Invocation::Assign {
            id: missing.into(),
            member: "ada".into(),
        },
    );
    differential.step(
        "move a ghost",
        Invocation::SetState {
            id: missing.into(),
            state: "done".into(),
            comment: None,
            if_state: None,
        },
    );
    differential.step("set a ghost's fields", {
        let mut invocation = set_fields(missing);
        if let Invocation::SetFields { title, .. } = &mut invocation {
            *title = Some("x".into());
        }
        invocation
    });
}

#[test]
fn a_malformed_story_id_agrees() {
    let differential = Differential::new();
    differential.step("new", new_story("a real one"));
    for id in ["OTHER-1", "SH-", "SH-x", "SH-007", "nonsense", ""] {
        differential.step(
            id,
            Invocation::Comment {
                id: id.to_string(),
                text: "hello".into(),
            },
        );
    }
}

// --- state transitions -----------------------------------------------------

#[test]
fn moving_between_open_states_agrees() {
    let differential = Differential::new();
    let id = differential.step_id("new", new_story("moving"));
    differential.step(
        "move to in-progress",
        Invocation::SetState {
            id: id.clone(),
            state: "in-progress".into(),
            comment: None,
            if_state: None,
        },
    );
    differential.step(
        "move back to todo",
        Invocation::SetState {
            id,
            state: "todo".into(),
            comment: None,
            if_state: None,
        },
    );
}

#[test]
fn closing_a_story_agrees_including_the_archive() {
    let differential = Differential::new();
    let id = differential.step_id("new", new_story("closing"));
    differential.step(
        "close",
        Invocation::SetState {
            id: id.clone(),
            state: "done".into(),
            comment: Some("shipped".into()),
            if_state: None,
        },
    );
    // Everything after a close is the interesting part: the legacy leg has
    // moved the story into `archive.db` and the store leg has flipped a
    // column, and both must answer identically.
    differential.step(
        "comment on the closed story",
        Invocation::Comment {
            id: id.clone(),
            text: "too late".into(),
        },
    );
    differential.step(
        "move the closed story",
        Invocation::SetState {
            id,
            state: "todo".into(),
            comment: None,
            if_state: None,
        },
    );
}

#[test]
fn closing_a_blocked_story_agrees() {
    let differential = Differential::new();
    let id = differential.step_id("new", new_story("blocked then closed"));
    differential.step(
        "block",
        Invocation::SetAwaiting {
            id: id.clone(),
            awaiting: "review".into(),
        },
    );
    differential.step(
        "close",
        Invocation::SetState {
            id,
            state: "done".into(),
            comment: None,
            if_state: None,
        },
    );
}

#[test]
fn moving_to_an_undefined_state_agrees() {
    let differential = Differential::new();
    let id = differential.step_id("new", new_story("nowhere"));
    differential.step(
        "move to limbo",
        Invocation::SetState {
            id,
            state: "limbo".into(),
            comment: None,
            if_state: None,
        },
    );
}

// --- compare-and-swap ------------------------------------------------------

#[test]
fn an_if_state_claim_agrees_when_it_wins_and_when_it_loses() {
    let differential = Differential::new();
    let id = differential.step_id("new", new_story("claimed"));
    differential.step(
        "claim from todo",
        Invocation::SetState {
            id: id.clone(),
            state: "in-progress".into(),
            comment: None,
            if_state: Some("todo".into()),
        },
    );
    differential.step(
        "claim from todo again",
        Invocation::SetState {
            id: id.clone(),
            state: "in-progress".into(),
            comment: None,
            if_state: Some("todo".into()),
        },
    );
    differential.step(
        "claim naming a state that does not exist",
        Invocation::SetState {
            id,
            state: "todo".into(),
            comment: None,
            if_state: Some("limbo".into()),
        },
    );
}

#[test]
fn an_if_state_claim_against_a_closed_story_agrees() {
    let differential = Differential::new();
    let id = differential.step_id("new", new_story("closed under the claim"));
    differential.step(
        "close",
        Invocation::SetState {
            id: id.clone(),
            state: "done".into(),
            comment: None,
            if_state: None,
        },
    );
    differential.step(
        "claim the closed story",
        Invocation::SetState {
            id,
            state: "in-progress".into(),
            comment: None,
            if_state: Some("todo".into()),
        },
    );
}

#[test]
fn an_if_state_claim_against_a_deleted_story_agrees() {
    let differential = Differential::new();
    let id = differential.step_id("new", new_story("deleted under the claim"));
    differential.step(
        "delete",
        Invocation::Delete {
            id: id.clone(),
            reason: "duplicate".into(),
        },
    );
    differential.step(
        "claim the deleted story",
        Invocation::SetState {
            id,
            state: "in-progress".into(),
            comment: None,
            if_state: Some("todo".into()),
        },
    );
}

// --- set fields ------------------------------------------------------------

#[test]
fn every_set_fields_flag_agrees() {
    let differential = Differential::new();
    differential.add_member("ada", None);
    let id = differential.step_id("new", new_story("editable"));

    differential.step("set nothing", set_fields(&id));

    differential.step(
        "set everything",
        Invocation::SetFields {
            id: id.clone(),
            title: Some("edited".into()),
            state: None,
            priority: Some("high".into()),
            assignee: Some("ada".into()),
            labels: Some("x, y ,".into()),
            blocked: Some("waiting".into()),
            unblocked: false,
            json: None,
            story_type: Some("bug".into()),
            description: Some("described".into()),
        },
    );
    differential.step(
        "unblock",
        Invocation::SetFields {
            id: id.clone(),
            title: None,
            state: None,
            priority: None,
            assignee: None,
            labels: None,
            blocked: None,
            unblocked: true,
            json: None,
            story_type: None,
            description: None,
        },
    );
    differential.step(
        "reject an unknown assignee",
        Invocation::SetFields {
            id: id.clone(),
            title: Some("should not stick".into()),
            state: None,
            priority: None,
            assignee: Some("nobody".into()),
            labels: None,
            blocked: None,
            unblocked: false,
            json: None,
            story_type: None,
            description: None,
        },
    );
    differential.show("confirm nothing stuck", &id);
}

#[test]
fn every_json_patch_key_agrees() {
    let differential = Differential::new();
    differential.add_member("ada", None);
    let id = differential.step_id("new", new_story("patchable"));

    let patches = [
        r#"{"title":"patched"}"#,
        r#"{"title":""}"#,
        r#"{"priority":"low"}"#,
        r#"{"assignee":"ada"}"#,
        r#"{"assignee":null}"#,
        r#"{"assignee":""}"#,
        r#"{"labels":["p","q"]}"#,
        r#"{"blocked":"waiting"}"#,
        r#"{"blocked":null}"#,
        r#"{"blocked":""}"#,
        r#"{"story_type":"bug"}"#,
        r#"{"description":"new"}"#,
        r#"{"state":"in-progress"}"#,
    ];
    for patch in patches {
        differential.step(
            patch,
            Invocation::SetFields {
                id: id.clone(),
                title: None,
                state: None,
                priority: None,
                assignee: None,
                labels: None,
                blocked: None,
                unblocked: false,
                json: Some(patch.into()),
                story_type: None,
                description: None,
            },
        );
    }
}

#[test]
fn every_json_patch_rejection_agrees() {
    let differential = Differential::new();
    let id = differential.step_id("new", new_story("unpatchable"));

    let patches = [
        r#"{"#,
        r#"[1,2]"#,
        r#"{"title":7}"#,
        r#"{"state":7}"#,
        r#"{"state":"limbo"}"#,
        r#"{"priority":"urgent"}"#,
        r#"{"assignee":7}"#,
        r#"{"assignee":"nobody"}"#,
        r#"{"labels":"x"}"#,
        r#"{"blocked":7}"#,
        r#"{"story_type":"chore"}"#,
        r#"{"description":7}"#,
        r#"{"nope":"x"}"#,
    ];
    for patch in patches {
        differential.step(
            patch,
            Invocation::SetFields {
                id: id.clone(),
                title: None,
                state: None,
                priority: None,
                assignee: None,
                labels: None,
                blocked: None,
                unblocked: false,
                json: Some(patch.into()),
                story_type: None,
                description: None,
            },
        );
    }
    differential.show("nothing stuck", &id);
}

#[test]
fn set_fields_closing_a_story_agrees_by_flag_and_by_patch() {
    let differential = Differential::new();
    let by_flag = differential.step_id("new", new_story("closed by flag"));
    let by_patch = differential.step_id("new", new_story("closed by patch"));

    differential.step(
        "close by flag",
        Invocation::SetFields {
            id: by_flag.clone(),
            title: None,
            state: Some("done".into()),
            priority: None,
            assignee: None,
            labels: None,
            blocked: None,
            unblocked: false,
            json: None,
            story_type: None,
            description: None,
        },
    );
    differential.step(
        "close by patch",
        Invocation::SetFields {
            id: by_patch.clone(),
            title: None,
            state: None,
            priority: None,
            assignee: None,
            labels: None,
            blocked: None,
            unblocked: false,
            json: Some(r#"{"state":"done"}"#.into()),
            story_type: None,
            description: None,
        },
    );
    differential.show("the flag one", &by_flag);
    differential.show("the patch one", &by_patch);
}

// --- bulk update -----------------------------------------------------------

#[test]
fn bulk_update_agrees_line_for_line() {
    let differential = Differential::new();
    let first = differential.step_id("new", new_story("one"));
    let second = differential.step_id("new", new_story("two"));
    let third = differential.step_id("new", new_story("three"));
    differential.step(
        "close the third",
        Invocation::SetState {
            id: third.clone(),
            state: "done".into(),
            comment: None,
            if_state: None,
        },
    );

    differential.step(
        "bulk",
        Invocation::BulkUpdate {
            updates: vec![
                (first.clone(), "in-progress".to_string()),
                (second.clone(), "done".to_string()),
                (third.clone(), "todo".to_string()),
                ("SH-99".to_string(), "todo".to_string()),
                (first.clone(), "limbo".to_string()),
            ],
        },
    );
    for id in [first, second, third] {
        differential.show("after bulk", &id);
    }
}

#[test]
fn an_empty_bulk_update_agrees() {
    let differential = Differential::new();
    differential.step(
        "bulk of nothing",
        Invocation::BulkUpdate { updates: vec![] },
    );
}

// --- delete and reopen -----------------------------------------------------

#[test]
fn deleting_a_story_agrees() {
    let differential = Differential::new();
    let id = differential.step_id("new", new_story("deletable"));
    differential.step(
        "delete",
        Invocation::Delete {
            id: id.clone(),
            reason: "created in error".into(),
        },
    );
    differential.show("show the deleted story", &id);
    differential.step(
        "delete it again",
        Invocation::Delete {
            id,
            reason: "again".into(),
        },
    );
    differential.step(
        "delete one that never was",
        Invocation::Delete {
            id: "SH-99".into(),
            reason: "why".into(),
        },
    );
}

#[test]
fn reopening_a_closed_story_agrees() {
    // The one place the two implementations genuinely differ: the legacy path
    // rewrites the event log to strip the close markers, and the store leg
    // appends a state change. The *snapshot* they produce has to be identical
    // anyway, and this is the test that says so.
    let differential = Differential::new();
    let id = differential.step_id("new", new_story("reopenable"));
    differential.step(
        "close",
        Invocation::SetState {
            id: id.clone(),
            state: "done".into(),
            comment: Some("done for now".into()),
            if_state: None,
        },
    );
    differential.step(
        "reopen",
        Invocation::Reopen {
            id: id.clone(),
            force: false,
        },
    );
    differential.step(
        "reopen the open story",
        Invocation::Reopen {
            id: id.clone(),
            force: false,
        },
    );
    differential.step(
        "comment on the reopened story",
        Invocation::Comment {
            id: id.clone(),
            text: "still here".into(),
        },
    );
    differential.show("show it", &id);
}

#[test]
fn undeleting_agrees_with_and_without_force() {
    let differential = Differential::new();
    let without = differential.step_id("new", new_story("no force"));
    let with = differential.step_id("new", new_story("forced"));
    for id in [&without, &with] {
        differential.step(
            "delete",
            Invocation::Delete {
                id: id.clone(),
                reason: "an error".into(),
            },
        );
    }

    differential.step(
        "reopen without force",
        Invocation::Reopen {
            id: without.clone(),
            force: false,
        },
    );
    differential.step(
        "reopen with force",
        Invocation::Reopen {
            id: with.clone(),
            force: true,
        },
    );
    differential.show("the untouched one", &without);
    differential.show("the undeleted one", &with);
}

#[test]
fn reopening_a_story_that_does_not_exist_agrees() {
    let differential = Differential::new();
    differential.step(
        "reopen a ghost",
        Invocation::Reopen {
            id: "SH-99".into(),
            force: false,
        },
    );
}

// --- relations -------------------------------------------------------------

#[test]
fn every_relation_spelling_agrees_in_both_directions() {
    let relations = [
        "relates-to",
        "related-to",
        "blocks",
        "blocked-by",
        "parent-of",
        "child-of",
        "duplicate-of",
        "obviates",
        "obviated-by",
    ];
    for relation in relations {
        let differential = Differential::new();
        let a = differential.step_id("new", new_story("a"));
        let b = differential.step_id("new", new_story("b"));

        differential.step(
            relation,
            Invocation::Relate {
                a: a.clone(),
                relation: relation.into(),
                b: b.clone(),
                remove: false,
            },
        );
        differential.show("show a", &a);
        differential.show("show b", &b);
        differential.step(
            "add again",
            Invocation::Relate {
                a: a.clone(),
                relation: relation.into(),
                b: b.clone(),
                remove: false,
            },
        );
        differential.step(
            "remove",
            Invocation::Relate {
                a: a.clone(),
                relation: relation.into(),
                b: b.clone(),
                remove: true,
            },
        );
        differential.show("show a after removal", &a);
        differential.show("show b after removal", &b);
        differential.step(
            "remove again",
            Invocation::Relate {
                a: a.clone(),
                relation: relation.into(),
                b: "SH-2".into(),
                remove: true,
            },
        );
    }
}

#[test]
fn relation_rejections_agree() {
    let differential = Differential::new();
    let a = differential.step_id("new", new_story("a"));
    let b = differential.step_id("new", new_story("b"));
    let closed = differential.step_id("new", new_story("closed"));
    differential.step(
        "close",
        Invocation::SetState {
            id: closed.clone(),
            state: "done".into(),
            comment: None,
            if_state: None,
        },
    );

    let cases = [
        ("self relation", a.clone(), "relates-to", a.clone()),
        ("missing first", "SH-99".into(), "relates-to", b.clone()),
        ("missing second", a.clone(), "relates-to", "SH-99".into()),
        (
            "missing both, same id",
            "SH-99".into(),
            "relates-to",
            "SH-99".into(),
        ),
        ("unsupported relation", a.clone(), "sort-of-like", b.clone()),
        ("closed second", a.clone(), "relates-to", closed.clone()),
        ("closed first", closed, "relates-to", b),
    ];
    for (label, left, relation, right) in cases {
        differential.step(
            label,
            Invocation::Relate {
                a: left,
                relation: relation.into(),
                b: right,
                remove: false,
            },
        );
    }
}

#[test]
fn the_single_parent_rule_agrees() {
    let differential = Differential::new();
    let parent = differential.step_id("new", new_story("parent"));
    let other = differential.step_id("new", new_story("other parent"));
    let child = differential.step_id("new", new_story("child"));

    differential.step(
        "first parent",
        Invocation::Relate {
            a: parent.clone(),
            relation: "parent-of".into(),
            b: child.clone(),
            remove: false,
        },
    );
    differential.step(
        "second parent from the parent",
        Invocation::Relate {
            a: other.clone(),
            relation: "parent-of".into(),
            b: child.clone(),
            remove: false,
        },
    );
    differential.step(
        "second parent from the child",
        Invocation::Relate {
            a: child.clone(),
            relation: "child-of".into(),
            b: other,
            remove: false,
        },
    );
    differential.show("the child", &child);
    differential.show("the parent", &parent);
}

#[test]
fn parent_cycles_agree() {
    let differential = Differential::new();
    let a = differential.step_id("new", new_story("a"));
    let b = differential.step_id("new", new_story("b"));
    let c = differential.step_id("new", new_story("c"));

    differential.step(
        "a parents b",
        Invocation::Relate {
            a: a.clone(),
            relation: "parent-of".into(),
            b: b.clone(),
            remove: false,
        },
    );
    differential.step(
        "b parents c",
        Invocation::Relate {
            a: b.clone(),
            relation: "parent-of".into(),
            b: c.clone(),
            remove: false,
        },
    );
    differential.step(
        "c parents a",
        Invocation::Relate {
            a: c.clone(),
            relation: "parent-of".into(),
            b: a.clone(),
            remove: false,
        },
    );
    differential.step(
        "a is a child of c",
        Invocation::Relate {
            a: a.clone(),
            relation: "child-of".into(),
            b: c,
            remove: false,
        },
    );
    differential.show("a", &a);
    differential.show("b", &b);
}

#[test]
fn a_relation_across_a_closure_agrees() {
    let differential = Differential::new();
    let a = differential.step_id("new", new_story("a"));
    let b = differential.step_id("new", new_story("b"));
    differential.step(
        "relate",
        Invocation::Relate {
            a: a.clone(),
            relation: "blocks".into(),
            b: b.clone(),
            remove: false,
        },
    );
    differential.step(
        "close the blocked one",
        Invocation::SetState {
            id: b.clone(),
            state: "done".into(),
            comment: None,
            if_state: None,
        },
    );
    differential.show("the blocker", &a);
    differential.show("the blocked", &b);
}

// --- long mixed sequences --------------------------------------------------

#[test]
fn a_long_mixed_sequence_agrees_at_every_step() {
    let differential = Differential::new();
    differential.add_member("ada", Some("ada-gh"));
    let mut ids = Vec::new();
    for index in 0..4 {
        ids.push(differential.step_id("new", new_story(&format!("story {index}"))));
    }

    let script: Vec<(&str, Invocation)> = vec![
        (
            "assign",
            Invocation::Assign {
                id: ids[0].clone(),
                member: "ada".into(),
            },
        ),
        (
            "priority",
            Invocation::SetPriority {
                id: ids[0].clone(),
                priority: "critical".into(),
            },
        ),
        (
            "labels",
            Invocation::SetLabels {
                id: ids[0].clone(),
                add: vec!["phase:1".into(), "urgent".into()],
                remove: vec![],
            },
        ),
        (
            "parent",
            Invocation::Relate {
                a: ids[0].clone(),
                relation: "parent-of".into(),
                b: ids[1].clone(),
                remove: false,
            },
        ),
        (
            "second child",
            Invocation::Relate {
                a: ids[0].clone(),
                relation: "parent-of".into(),
                b: ids[2].clone(),
                remove: false,
            },
        ),
        (
            "blocks",
            Invocation::Relate {
                a: ids[1].clone(),
                relation: "blocks".into(),
                b: ids[3].clone(),
                remove: false,
            },
        ),
        (
            "block the child",
            Invocation::SetAwaiting {
                id: ids[1].clone(),
                awaiting: "review".into(),
            },
        ),
        (
            "close the child",
            Invocation::SetState {
                id: ids[1].clone(),
                state: "done".into(),
                comment: Some("finished".into()),
                if_state: Some("todo".into()),
            },
        ),
        (
            "close the other child",
            Invocation::SetState {
                id: ids[2].clone(),
                state: "done".into(),
                comment: None,
                if_state: None,
            },
        ),
        (
            "reopen the child",
            Invocation::Reopen {
                id: ids[1].clone(),
                force: false,
            },
        ),
        (
            "delete the last",
            Invocation::Delete {
                id: ids[3].clone(),
                reason: "obsolete".into(),
            },
        ),
        (
            "bulk",
            Invocation::BulkUpdate {
                updates: vec![
                    (ids[0].clone(), "in-progress".to_string()),
                    (ids[3].clone(), "todo".to_string()),
                ],
            },
        ),
    ];

    for (label, invocation) in script {
        differential.step(label, invocation);
        for id in &ids {
            differential.show("show", id);
        }
    }
}

#[test]
fn the_story_view_agrees_on_derived_relationships_and_progress() {
    // `Response::Story` carries cross-story derivations — a parent's progress
    // rollup, the family relationships derived from the graph, the integrity
    // flags. They are computed from the whole project, so they are the part of
    // the view most likely to diverge and the part a single-story test would
    // never reach.
    let differential = Differential::new();
    let parent = differential.step_id("new", new_story("epic"));
    let children: Vec<String> = (0..3)
        .map(|index| differential.step_id("new", new_story(&format!("child {index}"))))
        .collect();

    for child in &children {
        differential.step(
            "adopt",
            Invocation::Relate {
                a: parent.clone(),
                relation: "parent-of".into(),
                b: child.clone(),
                remove: false,
            },
        );
    }
    differential.step(
        "close one child",
        Invocation::SetState {
            id: children[0].clone(),
            state: "done".into(),
            comment: None,
            if_state: None,
        },
    );
    differential.step(
        "start another",
        Invocation::SetState {
            id: children[1].clone(),
            state: "in-progress".into(),
            comment: None,
            if_state: None,
        },
    );
    differential.step(
        "obviate the third",
        Invocation::Relate {
            a: parent.clone(),
            relation: "obviates".into(),
            b: children[2].clone(),
            remove: false,
        },
    );

    differential.show("the parent", &parent);
    for child in children {
        differential.show("a child", &child);
    }
}

// --- the dispatcher's completeness gate ------------------------------------

#[test]
fn an_unported_invocation_fails_loudly_rather_than_silently() {
    let differential = Differential::new();
    let error = dispatch(&differential.ctx(), Invocation::Summary)
        .expect_err("summary is not ported in this wave");
    let message = error.to_string();
    assert!(message.contains("not yet ported"), "{message}");
    assert!(message.contains("summary"), "{message}");
}

#[test]
fn the_ported_arms_are_exactly_the_ones_this_wave_claims() {
    // A roster, so that porting an arm without updating this list — or
    // accidentally un-porting one — is a failing test rather than a surprise
    // three waves later.
    let ported = [
        "new",
        "comment",
        "assign",
        "set-priority",
        "set-labels",
        "set-awaiting",
        "clear-awaiting",
        "set-state",
        "set-fields",
        "bulk-update",
        "delete",
        "reopen",
        "relate",
        "init",
    ];
    let differential = Differential::new();
    let ctx = differential.ctx();

    let mut unported: BTreeMap<&str, bool> = BTreeMap::new();
    for (name, invocation) in unported_probes() {
        let is_unported = dispatch(&ctx, invocation)
            .err()
            .is_some_and(|error| error.to_string().contains("not yet ported"));
        unported.insert(name, is_unported);
    }

    for (name, is_unported) in &unported {
        assert!(
            *is_unported,
            "`{name}` answered as if it were ported, but it is not on the roster"
        );
        assert!(
            !ported.contains(name),
            "`{name}` is on the ported roster but reports itself unported"
        );
    }
    assert_eq!(ported.len(), 14);
}

/// One probe per invocation this wave does *not* port.
fn unported_probes() -> Vec<(&'static str, Invocation)> {
    vec![
        ("help", Invocation::Help),
        ("summary", Invocation::Summary),
        ("export", Invocation::Export),
        ("version", Invocation::Version),
        ("session-start", Invocation::SessionStart),
        ("help-compact", Invocation::HelpCompact),
        ("help-all", Invocation::HelpAll),
        ("show", Invocation::Show { id: "SH-1".into() }),
        ("search", Invocation::Search { query: "x".into() }),
        ("doctor", Invocation::Doctor { fix: false }),
        ("report", Invocation::Report { html: false }),
        (
            "next",
            Invocation::Next {
                count: 1,
                phase: None,
            },
        ),
        ("context", Invocation::Context { format: None }),
        ("handoff", Invocation::Handoff { since: None }),
        ("import", Invocation::Import { file: None }),
        (
            "import-project",
            Invocation::ImportProject { file: "x".into() },
        ),
        ("scaffold", Invocation::Scaffold { kind: "x".into() }),
        ("commit-sync", Invocation::CommitSync { since: None }),
        (
            "github-sync",
            Invocation::GithubSync {
                id: None,
                dry_run: true,
            },
        ),
        ("help-topic", Invocation::HelpTopic { topic: "x".into() }),
        (
            "update",
            Invocation::Update {
                check: true,
                force: false,
            },
        ),
    ]
}
