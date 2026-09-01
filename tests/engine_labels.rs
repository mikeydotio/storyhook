//! SH-453 — the two reserved labels of the Full Auto engine (SH-452, D12).
//!
//! | Label | `story next` returns it | Engine dispatches it |
//! |---|---|---|
//! | `no-auto` | yes | no — skipped, listed as needing a human |
//! | `human-only` | **no** — filtered in the ready queue | no |
//!
//! `no-auto` carries no filtering logic at all in this story. It is a
//! reservation plus documentation: the engine's skip is `--exclude-label`'s
//! job (SH-455) and the reconciler's (SH-465), and the constant exists now so
//! those two halves cannot disagree with this one about the spelling. The
//! test below that asserts `no-auto` is *still* offered is therefore the
//! load-bearing half of the pair — it is what fails if someone widens the
//! `human-only` filter to cover both.
//!
//! The whole subtlety of `human-only` is **where** it filters, which is
//! assumption A1 of `docs/spec/full-auto-engine.md`: the ready *queue* only.
//! It must not make a story `!is_ready`, because a human can still progress
//! it. Fold it into readiness instead and four things break at once — the
//! board shows the card as blocked, every ready count drops, `story list
//! --blocked` claims it, and `domain::compute_display_state` promotes an epic
//! whose only incomplete child is `human-only` to `"blocked"` when nothing is
//! stopping anyone from picking that child up. Half of the assertions here
//! exist for exactly that: they pass today for free, and their job is to fail
//! the moment a future implementer decides readiness is the tidier home.
//!
//! One filter site covers both doors onto the queue, because there is only
//! one queue: `StoryService::claim_next` selects through `QueryService::next`
//! inside its own write transaction, so filtering what the engine *looks at*
//! and filtering what it *takes* are the same edit. The spec asks for that
//! explicitly — the alternative leaves the half that matters unguarded.
//!
//! Every label spelling below comes from `domain::LABEL_*`. That is not
//! ceremony: it is what makes a rename reach the tests, and it is also the
//! `tests/` call site that keeps `tests/dead_public_surface.rs` from
//! reporting the constants as unreachable public surface (SH-198).

use assert_cmd::Command;
use storyhook::domain::{LABEL_HUMAN_ONLY, LABEL_NO_AUTO, RESERVED_LABELS};
use storyhook::help_topics::get_help_topic;
use storyhook::service::templates;
use storyhook_test_support::{TestEnv, scratch_dir};
use tempfile::TempDir;

/// Every `story` this file runs is the one THIS build produced, in the shared
/// test environment's private `HOME`, XDG directories and store — so nothing
/// here can reach the developer's own storyhook state, with or without a
/// wrapper script supplying one.
fn story(dir: &std::path::Path) -> Command {
    TestEnv::shared().story(dir)
}

/// Run `story <args>` and return its stdout, asserting it succeeded.
fn run(dir: &std::path::Path, args: &[&str]) -> String {
    let out = story(dir).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "story {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("story speaks UTF-8")
}

/// Run `story <args> --json` and parse the envelope.
fn json(dir: &std::path::Path, args: &[&str]) -> serde_json::Value {
    let mut with_json = args.to_vec();
    with_json.push("--json");
    serde_json::from_str(&run(dir, &with_json)).expect("--json emits a document")
}

/// The `StoryView` inside a `story show --json` envelope — the envelope's
/// `story` field, whose own `story` field is the snapshot underneath it.
fn view(dir: &std::path::Path, id: &str) -> serde_json::Value {
    json(dir, &["show", id])["story"].clone()
}

/// The story ids a `next`/`list` envelope carries, in the order it gave them.
///
/// Both shapes are read on purpose: `story next --count 1` answers with a
/// singular `story` (it is a question with one answer, and its `--json`
/// consumers read `.story`), every other count and every `list` answers with
/// `stories`. A helper that knew only one of them would silently report an
/// empty queue for the other.
fn ids(envelope: &serde_json::Value) -> Vec<String> {
    let id_of = |view: &serde_json::Value| {
        view["story"]["id"]
            .as_str()
            .expect("a story view carries an id")
            .to_string()
    };
    if let Some(views) = envelope["stories"].as_array() {
        return views.iter().map(id_of).collect();
    }
    if envelope["story"].is_object() {
        // The envelope's `story` field IS the view; the view's own `story`
        // field is the snapshot underneath it.
        return vec![id_of(&envelope["story"])];
    }
    Vec::new()
}

/// The context `story session-start` injects — it prints Claude Code's
/// SessionStart envelope, so the human-readable block is one JSON string
/// field rather than the command's own lines.
fn session_context(dir: &std::path::Path) -> String {
    let raw = run(dir, &["session-start"]);
    let envelope: serde_json::Value =
        serde_json::from_str(&raw).expect("session-start prints a hook envelope");
    envelope["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_else(|| panic!("session-start carried no context: {raw}"))
        .to_string()
}

/// A fresh project whose stories are created in the given order, each with an
/// optional label. Story `n` (1-based) is `SH-n`.
fn project(stories: &[(&str, Option<&str>)]) -> TempDir {
    let dir = scratch_dir();
    let path = dir.path();
    story(path)
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    for (title, label) in stories {
        let mut args = vec!["new", *title];
        if let Some(label) = label {
            args.push("--labels");
            args.push(label);
        }
        story(path).args(&args).assert().success();
    }
    dir
}

// ---------------------------------------------------------------------------
// `human-only` never reaches the ready queue
// ---------------------------------------------------------------------------

/// The whole point, at the door an agent actually knocks on.
#[test]
fn story_next_never_offers_a_human_only_story() {
    let dir = project(&[
        ("needs a person", Some(LABEL_HUMAN_ONLY)),
        ("ordinary work", None),
    ]);
    let path = dir.path();

    // SH-1 is the lower story number, so `domain::ready_order` would offer it
    // first at equal priority — this asserts the filter, not the sort.
    let offered = ids(&json(path, &["next"]));
    assert_eq!(offered, vec!["SH-2"], "`story next` must skip `human-only`");
}

/// Priority is `ready_order`'s first key, so a `critical` `human-only` story
/// is the one the queue would hand out first of all. Filtering has to happen
/// before the ordering, not as a tiebreak inside it.
#[test]
fn a_critical_human_only_story_is_still_never_offered() {
    let dir = project(&[
        ("ordinary work", None),
        ("needs a person", Some(LABEL_HUMAN_ONLY)),
    ]);
    let path = dir.path();
    story(path)
        .args(["prioritize", "SH-2", "critical"])
        .assert()
        .success();

    assert_eq!(ids(&json(path, &["next"])), vec!["SH-1"]);
}

/// `--count N` walks the whole execution queue rather than truncating an
/// already-filtered head, so it is a genuinely separate path to check.
#[test]
fn no_count_reaches_a_human_only_story() {
    let dir = project(&[
        ("needs a person", Some(LABEL_HUMAN_ONLY)),
        ("first", None),
        ("second", None),
        ("third", None),
    ]);
    let path = dir.path();

    let offered = ids(&json(path, &["next", "--count", "50"]));
    assert_eq!(
        offered,
        vec!["SH-2", "SH-3", "SH-4"],
        "the whole queue must be free of `human-only`, at any count"
    );
}

/// The empty-queue case, which is the one a naive filter gets wrong by
/// falling back to "well, there is nothing else, so here it is".
#[test]
fn a_project_whose_only_ready_story_is_human_only_has_no_next() {
    let dir = project(&[("needs a person", Some(LABEL_HUMAN_ONLY))]);
    let path = dir.path();

    let envelope = json(path, &["next"]);
    assert_eq!(
        envelope["message"], "no ready stories",
        "an all-`human-only` backlog answers with the empty-queue message"
    );
    assert!(ids(&envelope).is_empty());
}

/// The half that matters: `story claim --next` reads *and takes*. It reaches
/// the queue through `QueryService::next`, so it inherits the filter — this
/// asserts that it still does.
#[test]
fn claim_next_never_takes_a_human_only_story() {
    let dir = project(&[
        ("needs a person", Some(LABEL_HUMAN_ONLY)),
        ("ordinary work", None),
    ]);
    let path = dir.path();

    let claimed = json(path, &["claim", "--next", "--no-comment"]);
    assert_eq!(ids(&claimed), vec!["SH-2"], "the claim must skip it too");

    assert_eq!(
        view(path, "SH-1")["story"]["state"],
        "todo",
        "the `human-only` story must be left exactly where it was"
    );
}

/// `story session-start` is the context an agent is handed before it does
/// anything else, and its `Next:` line is documented (`highest_priority`) as
/// naming the story `story next` would offer first. Offering a `human-only`
/// story there would defeat the filter one layer above the command it
/// guards — and would make that docstring false.
///
/// Its ready *count* on the line above is a separate claim and must not move
/// (A1): the story is ready, it just is not anyone's next assignment.
#[test]
fn session_start_never_names_a_human_only_story_as_next() {
    let dir = project(&[
        ("needs a person", Some(LABEL_HUMAN_ONLY)),
        ("ordinary work", None),
    ]);
    let context = session_context(dir.path());

    let next_line = context
        .lines()
        .find(|line| line.trim_start().starts_with("Next:"))
        .unwrap_or_else(|| panic!("`story session-start` names no next story:\n{context}"));
    assert!(
        next_line.contains("SH-2") && !next_line.contains("SH-1"),
        "session-start offered a `human-only` story: {next_line}"
    );
    assert!(
        context.contains("2 open stories, 2 ready"),
        "the ready count must still count the `human-only` story:\n{context}"
    );
}

// ---------------------------------------------------------------------------
// ...and is ready everywhere else (assumption A1)
// ---------------------------------------------------------------------------

/// A human can still pick it up, so every surface a *person* reads must go on
/// saying so. This is the assertion that fails if the filter is ever moved
/// into `is_ready`/`is_claimable`.
#[test]
fn a_human_only_story_is_still_ready_everywhere_a_person_looks() {
    let dir = project(&[
        ("needs a person", Some(LABEL_HUMAN_ONLY)),
        ("ordinary work", None),
    ]);
    let path = dir.path();

    let ready = ids(&json(path, &["list", "--ready"]));
    assert!(
        ready.contains(&"SH-1".to_string()),
        "`story list --ready` must still carry it, got {ready:?}"
    );

    let blocked = ids(&json(path, &["list", "--blocked"]));
    assert!(
        !blocked.contains(&"SH-1".to_string()),
        "`human-only` is not a blocked story, got {blocked:?}"
    );

    let summary = json(path, &["summary"]);
    assert_eq!(
        summary["summary"]["ready_count"], 2,
        "the ready count must still count it"
    );

    let human_only = view(path, "SH-1");
    assert!(
        human_only["display_state"].is_null(),
        "the board must place the card by its own state, not as blocked: {}",
        human_only["display_state"]
    );

    // `story load-context`'s own ready section is a third reader of the same
    // fact, and the one an agent is handed verbatim.
    let context = run(path, &["load-context"]);
    assert!(
        context.contains("## Ready to Work (2 total)") && context.contains("SH-1"),
        "load-context's ready section must still carry it:\n{context}"
    );
}

/// The epic case, spelled out because it is the one with a second mechanism
/// behind it: `compute_display_state` promotes any `!is_ready` story to
/// `"blocked"`, and `apply_computed_epic_states` projects children onto their
/// parent. A `human-only` child must move neither.
#[test]
fn an_epic_whose_only_open_child_is_human_only_is_not_blocked() {
    let dir = scratch_dir();
    let path = dir.path();
    story(path)
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    // Typed `epic` rather than left to acquire epic-ness from its edge, per
    // SH-499 and the fixtures in `tests/epic_deleted_children.rs`.
    story(path)
        .args(["new", "the epic", "--type", "epic"])
        .assert()
        .success();
    story(path)
        .args(["new", "needs a person", "--labels", LABEL_HUMAN_ONLY])
        .assert()
        .success();
    story(path)
        .args(["relate", "SH-1", "parent-of", "SH-2"])
        .assert()
        .success();

    let epic = view(path, "SH-1");
    assert_eq!(
        epic["story"]["superstate"], "OPEN",
        "the epic must stay open"
    );
    assert!(
        epic["display_state"].is_null() || epic["display_state"] != "blocked",
        "a `human-only` child must not make its epic read as blocked: {}",
        epic["display_state"]
    );

    let blocked = ids(&json(path, &["list", "--blocked"]));
    assert!(
        blocked.is_empty(),
        "nothing in this project is blocked, got {blocked:?}"
    );
}

// ---------------------------------------------------------------------------
// `no-auto` changes nothing
// ---------------------------------------------------------------------------

/// The mutation fence for the pair: widen the `human-only` filter to cover
/// both reserved labels and this is what goes red.
#[test]
fn no_auto_is_still_offered_and_still_claimable() {
    let dir = project(&[("human in the loop", Some(LABEL_NO_AUTO)), ("other", None)]);
    let path = dir.path();

    assert_eq!(
        ids(&json(path, &["next"])),
        vec!["SH-1"],
        "`no-auto` is skipped by the engine, never by `story next`"
    );

    let claimed = json(path, &["claim", "--next", "--no-comment"]);
    assert_eq!(ids(&claimed), vec!["SH-1"], "and it is claimable by hand");
}

/// A story carrying both labels is `human-only`: the stricter reservation
/// wins, because `no-auto` says nothing at all about this path.
#[test]
fn both_labels_at_once_reads_as_human_only() {
    let dir = project(&[
        (
            "both",
            Some(&format!("{LABEL_NO_AUTO},{LABEL_HUMAN_ONLY}") as &str),
        ),
        ("other", None),
    ]);
    assert_eq!(ids(&json(dir.path(), &["next"])), vec!["SH-2"]);
}

// ---------------------------------------------------------------------------
// The reservation is documented wherever label guidance lives
// ---------------------------------------------------------------------------

/// Every document that teaches labels must name both reserved names, and the
/// names come from `RESERVED_LABELS` rather than from string literals here —
/// so a rename either reaches all four documents or fails this test.
#[test]
fn every_label_guidance_surface_names_both_reserved_labels() {
    let surfaces: [(&str, String); 3] = [
        (
            "`story help label`",
            get_help_topic("label")
                .expect("the `label` topic must exist")
                .to_string(),
        ),
        (
            "the scaffolded AGENTS.md",
            templates::agents_md("SH", "done"),
        ),
        ("the scaffolded .cursorrules", templates::cursor_rules()),
    ];

    for (name, body) in &surfaces {
        for label in RESERVED_LABELS {
            assert!(
                body.contains(label),
                "{name} must document the reserved label `{label}`"
            );
        }
        assert!(
            body.to_lowercase().contains("reserved"),
            "{name} must say the labels are reserved, not merely mention them"
        );
    }
}

/// The `label` topic has to say the thing that is easy to get wrong, not just
/// list the names: `human-only` does not block a story.
#[test]
fn the_label_topic_says_human_only_does_not_block() {
    let body = get_help_topic("label").expect("the `label` topic must exist");
    let lowered = body.to_lowercase();
    assert!(
        lowered.contains("not blocked") || lowered.contains("does not block"),
        "`story help label` must state that `{LABEL_HUMAN_ONLY}` leaves a story ready"
    );
}
