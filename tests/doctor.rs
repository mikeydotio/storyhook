//! `story doctor` — what it finds when the storage is already wrong.
//!
//! These fixtures fabricate states the public API refuses to produce, which is
//! the point: the doctor's whole job is to describe damage nothing was supposed
//! to be able to do. They used to fabricate it by writing raw JSONL into
//! `.storyhook/open/stories/`; they fabricate it through
//! [`storyhook::store::test_support`] now, which bypasses the *service* layer
//! without bypassing the schema.
//!
//! Note what that distinction buys, and what it costs. Two of the three shapes
//! below — a dangling relation, a second parent — the schema now refuses
//! outright, so the doctor cannot be shown them at all and the defect *class*
//! is gone (`service_integrity.rs::the_shapes_doctor_used_to_find_are_now_
//! refused_by_the_schema` pins that). What remains reachable is the
//! genuinely-still-possible kind: a relation whose two ends' *histories*
//! disagree, and a story whose type names something the project's catalog does
//! not define.

use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use storyhook::domain::{StoryEvent, TypeDef};
use storyhook::store::test_support::inject_events;
use storyhook::store::{ReadOps, Store, WriteOps};
use storyhook_test_support::TestEnv;

/// `at` for injected events: fixed, so a rendering never depends on the clock.
const AT: &str = "2026-03-11T00:00:01Z";

#[test]
fn doctor_reports_a_relation_only_one_end_records() {
    let env = TestEnv::shared();
    let project = env.project().seed_story("A").seed_story("B").build();
    let store = project.open_store();
    let id = project.project_id(&store);

    // `story relate` writes both ends' events in one transaction, so this is
    // unreachable through the CLI — which is exactly why the doctor has to be
    // able to report it: a tree migrated from `.storyhook/` can contain it
    // (SH-60), and so can a database written by an older storyhook.
    inject_events(
        &store,
        id,
        project.story_no(&store, "SH-1"),
        &[StoryEvent::StoryRelationshipAdded {
            at: AT.to_string(),
            other_id: "SH-2".to_string(),
            relation: "blocks".to_string(),
        }],
    )
    .expect("injecting a one-sided relation");

    project
        .run(&["doctor"])
        .code(5)
        .stderr(contains("missing inverse relation"));
}

/// The same asymmetry on an **obviation** edge, which `doctor` used to answer
/// `0` for (SH-268).
///
/// At the CLI because the change is an exit code, and no service-level test can
/// see one. `is_suppressed` filtered any finding whose sentence contained
/// "obviated" — which the expected inverse of `obviates` does — so a project
/// holding this shape was pronounced healthy while `--fix` went on repairing
/// it. A scripted operator gating on `story doctor` saw green and then watched
/// `story doctor --fix` change the store.
#[test]
fn doctor_reports_a_one_sided_obviation_edge() {
    let env = TestEnv::shared();
    let project = env.project().seed_story("A").seed_story("B").build();
    let store = project.open_store();
    let id = project.project_id(&store);

    inject_events(
        &store,
        id,
        project.story_no(&store, "SH-1"),
        &[StoryEvent::StoryRelationshipAdded {
            at: AT.to_string(),
            other_id: "SH-2".to_string(),
            relation: "obviates".to_string(),
        }],
    )
    .expect("injecting a one-sided obviation edge");

    project.run(&["doctor"]).code(5).stderr(contains(
        "SH-1: missing inverse relation `obviated-by` on story `SH-2`",
    ));

    project.run(&["doctor", "--fix"]).success();
    project.run(&["doctor"]).success();
}

/// SH-164: a label written before the write-path guard existed — `web,sse` as
/// one label, the SH-145 shape — is unreachable through any service today, so
/// the doctor's coverage of it is pinned the same way as the relation
/// asymmetry above: injected straight past every service.
#[test]
fn doctor_reports_and_fixes_a_malformed_label() {
    let env = TestEnv::shared();
    let project = env.project().seed_story("A").build();
    let store = project.open_store();
    let id = project.project_id(&store);

    inject_events(
        &store,
        id,
        project.story_no(&store, "SH-1"),
        &[StoryEvent::StoryLabelsSet {
            at: AT.to_string(),
            labels: vec!["web,sse".to_string()],
        }],
    )
    .expect("injecting a malformed label");

    project
        .run(&["doctor"])
        .code(5)
        .stderr(contains("malformed labels"));

    project.run(&["doctor", "--fix"]).success();
    project
        .run(&["show", "SH-1"])
        .success()
        .stdout(contains("labels: sse, web"));
}

/// SH-225: the same malformed label on a **closed** story. `--fix` cannot
/// append to a closed history and must not pretend otherwise — so the refusal
/// arrives with the story to reopen and the three commands that clear it,
/// rather than as a repeat of the finding the operator already read.
#[test]
fn doctor_fix_names_the_closed_story_whose_repair_it_declined() {
    let env = TestEnv::shared();
    let project = env.project().seed_story("A").build();
    let store = project.open_store();
    let id = project.project_id(&store);

    inject_events(
        &store,
        id,
        project.story_no(&store, "SH-1"),
        &[StoryEvent::StoryLabelsSet {
            at: AT.to_string(),
            labels: vec!["web,sse".to_string()],
        }],
    )
    .expect("injecting a malformed label");

    project.run(&["move", "SH-1", "done"]).success();

    project.run(&["doctor", "--fix"]).code(5).stderr(
        contains("SH-1: malformed labels")
            .and(contains("SH-1: normalize its labels to [\"sse\", \"web\"]"))
            .and(contains("`story reopen <id>`")),
    );
}

/// SH-285, at the CLI: `--fix` must not retract a **valid** relation because
/// the far end's read-model row is missing.
///
/// The service-level tests pin the mechanisms; this pins the thing an operator
/// actually experiences — that a `story doctor --fix` over a store holding
/// nothing worse than a rebuildable cache miss exits zero and leaves the
/// project whole, rather than exiting zero having destroyed an edge and then
/// reporting the asymmetry it had just created. `story doctor --fix` is
/// precisely what is run against an already-damaged store, so the trigger and
/// the audience coincide.
#[test]
fn doctor_fix_keeps_a_valid_relation_whose_far_end_lost_its_row() {
    let env = TestEnv::shared();
    let project = env.project().seed_story("A").seed_story("B").build();
    let store = project.open_store();
    let id = project.project_id(&store);

    project.run(&["relate", "SH-1", "blocks", "SH-2"]).success();

    storyhook::store::test_support::forget_read_model_row(
        &store,
        id,
        project.story_no(&store, "SH-2"),
    )
    .expect("forgetting a read-model row");

    project
        .run(&["doctor", "--fix"])
        .success()
        .stdout(contains("doctor repaired supported integrity issues"));
    project.run(&["doctor"]).success();
    project
        .run(&["show", "SH-1"])
        .success()
        .stdout(contains("blocks SH-2"));
}

#[test]
fn doctor_flags_a_story_type_the_catalog_does_not_define() {
    let env = TestEnv::shared();
    let project = env.project().seed_story("A").build();
    let store = project.open_store();
    let id = project.project_id(&store);

    inject_events(
        &store,
        id,
        project.story_no(&store, "SH-1"),
        &[StoryEvent::StoryTypeSet {
            at: AT.to_string(),
            story_type: "nonexistent-type".to_string(),
        }],
    )
    .expect("injecting an unknown story type");

    project
        .run(&["doctor"])
        .code(5)
        .stderr(contains("unknown type `nonexistent-type`"));
}

/// SH-134: a type slug nothing can address is reported, and never repaired.
///
/// `add_type` refuses these now, so a catalog can only hold one if it was
/// written by an older storyhook or arrived in a document — which SH-134 left
/// raw on purpose, making this finding the only way such a slug is ever
/// noticed. The fixture writes it the way those paths do: through the store,
/// past the service that now refuses it.
///
/// `--fix` must not touch it. The only two automatic repairs available are
/// renaming the type — which `TypeChanges` bans outright, because every
/// `StoryTypeSet` event names the slug it set and a rename orphans them — and
/// retyping stories the user never asked about. So the finding survives the
/// repair and the command still fails, which is the doctor's existing habit of
/// saying what it cannot fix rather than claiming a repair it did not make.
#[test]
fn doctor_reports_an_unaddressable_type_slug_and_cannot_fix_it() {
    let env = TestEnv::shared();
    let project = env.project().seed_story("A").build();
    let store = project.open_store();
    let id = project.project_id(&store);

    let mut types = store.read(|tx| tx.types(id)).expect("reading the catalog");
    types.push(TypeDef {
        slug: "in review".to_string(),
        description: None,
        emoji: None,
    });
    store
        .write(|tx| tx.put_types(id, &types))
        .expect("seeding a catalog written before the rule existed");

    project
        .run(&["doctor"])
        .code(5)
        .stderr(contains("type `in review` cannot be addressed"));

    // The repair runs, changes nothing about this, and reports it again.
    project
        .run(&["doctor", "--fix"])
        .code(5)
        .stderr(contains("type `in review` cannot be addressed"));

    let after = store.read(|tx| tx.types(id)).expect("reading the catalog");
    assert!(
        after.iter().any(|t| t.slug == "in review"),
        "`--fix` must not delete a type the user may still want to rename"
    );
}

#[test]
fn show_suppresses_the_derived_halves_of_a_relationship() {
    // The second half of what the old parent-cycle test asserted, and the half
    // that is still reachable: a real `parent-of` edge is reported, and the
    // *virtual* relations the graph derives from it are not — they are a view,
    // not a fact about the story.
    //
    // The cycle itself is gone: `story_relations` has a foreign key at both
    // ends and a partial unique index on `child-of`, so a story cannot be its
    // own ancestor. The schema refuses the write instead of the doctor
    // reporting it afterwards.
    let env = TestEnv::shared();
    let project = env.project().seed_story("A").seed_story("B").build();
    project
        .run(&["relate", "SH-1", "parent-of", "SH-2"])
        .success();

    project
        .run(&["--json", "show", "SH-1"])
        .success()
        .stdout(contains("\"parent-of\""))
        .stdout(contains("\"ancestor-of\"").not())
        .stdout(contains("\"descendent-of\"").not());
}

/// SH-211: `hidden_at` carries no schema CHECK against `superstate` —
/// `schema/0010_story_hidden.sql` explains why SQLite cannot express one
/// without the full table-rebuild migration 4 used for `archived`. "Hidden
/// implies closed" is instead kept true by the service layer (`hide` refuses
/// an open story) and by the fold (`fold_story` clears `hidden_at` the moment
/// a superstate resolves back to OPEN) — neither of which a write that reaches
/// the `hidden_at` *column* directly, the way a raw migration or an admin
/// script would, ever goes through. Fabricated the same way: a second
/// connection, one column, no event behind it.
#[test]
fn doctor_reports_and_fixes_a_hidden_story_whose_superstate_reads_open() {
    let env = TestEnv::shared();
    let project = env.project().seed_story("A story").build();
    let store = project.open_store();
    let id = project.project_id(&store);
    let story = project.story_no(&store, "SH-1");

    let conn = rusqlite::Connection::open(store.path()).expect("opening the store");
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .expect("setting a busy timeout");
    conn.execute(
        "UPDATE stories SET hidden_at = ?3 WHERE project_id = ?1 AND story_no = ?2",
        rusqlite::params![id.get(), story.get(), AT],
    )
    .expect("fabricating a hidden-but-open row");

    project
        .run(&["doctor"])
        .code(5)
        .stderr(contains("SH-1: hidden_at is").and(contains("but the events say `<none>`")));

    project.run(&["doctor", "--fix"]).success();
    project.run(&["doctor"]).success();
    project
        .run(&["--json", "show", "SH-1"])
        .success()
        .stdout(contains("\"hidden_at\"").not());
}

/// The sibling gap found alongside SH-211: `draft` (SH-175) is the only other
/// dedicated `stories` column `diff()` never compared against the fold, for
/// the identical reason — nothing but `StoryPublished` and the fold itself
/// ever kept it in sync, and a write that reaches the column directly skips
/// both.
#[test]
fn doctor_reports_and_fixes_a_draft_flag_that_disagrees_with_its_events() {
    let env = TestEnv::shared();
    let project = env.project().seed_story("A story").build();
    let store = project.open_store();
    let id = project.project_id(&store);
    let story = project.story_no(&store, "SH-1");

    let conn = rusqlite::Connection::open(store.path()).expect("opening the store");
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .expect("setting a busy timeout");
    conn.execute(
        "UPDATE stories SET draft = 1 WHERE project_id = ?1 AND story_no = ?2",
        rusqlite::params![id.get(), story.get()],
    )
    .expect("fabricating an unpublished flag with no event behind it");

    project
        .run(&["doctor"])
        .code(5)
        .stderr(contains("SH-1: draft is `true` but the events say `false`"));

    project.run(&["doctor", "--fix"]).success();
    project.run(&["doctor"]).success();
}

/// SH-269, and the half a same-prefix fixture cannot catch: the read-model
/// pass renders the id from the **project's own** prefix, so a literal `SH`
/// would pass every other test in this file and still be wrong for every
/// project that is not this one.
///
/// The bare number it used to print (`story 1:`) was at least honest about
/// knowing no prefix; a hardcoded one would be a confident lie.
#[test]
fn drift_findings_render_the_projects_own_prefix() {
    let env = TestEnv::shared();
    let project = env.project().prefix("ZZ").seed_story("A story").build();
    let store = project.open_store();
    let id = project.project_id(&store);
    let story = project.story_no(&store, "ZZ-1");

    let conn = rusqlite::Connection::open(store.path()).expect("opening the store");
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .expect("setting a busy timeout");
    conn.execute(
        "UPDATE stories SET draft = 1 WHERE project_id = ?1 AND story_no = ?2",
        rusqlite::params![id.get(), story.get()],
    )
    .expect("fabricating an unpublished flag with no event behind it");

    project
        .run(&["doctor"])
        .code(5)
        .stderr(contains("ZZ-1: draft is `true` but the events say `false`"))
        .stderr(contains("SH-1").not())
        .stderr(contains("story 1:").not());

    project.run(&["doctor", "--fix"]).success();
    project.run(&["doctor"]).success();
}

#[test]
fn doctor_does_not_flag_known_story_type() {
    let env = TestEnv::shared();
    let project = env.project().build();
    project.run(&["type", "add", "feature"]).success();
    project.run(&["new", "A"]).success();
    project.run(&["set", "SH-1", "--type", "feature"]).success();

    project.run(&["doctor"]).success();
}

/// **SH-244's acceptance criterion, end to end.** SH-243 audited 109 read-model
/// divergences by regexing them out of `story doctor --json`'s `.error` — one
/// unstructured ~1.68MB string. Every value that parser recovered is now a
/// field, so this reads all four with plain JSON access and no pattern matching
/// at all. If this test ever needs a regex again, the story has regressed.
#[test]
fn doctor_json_hands_a_divergence_over_as_data_not_prose() {
    let env = TestEnv::shared();
    let project = env.project().seed_story("A").build();
    let store = project.open_store();
    let id = project.project_id(&store);

    // Damage the read model behind storyhook's back: the row now disagrees
    // with the events it was folded from, which is the exact shape SH-243 met
    // 109 of.
    let story = project.story_no(&store, "SH-1");
    store
        .write(|tx| {
            let mut snapshot = tx.story(id, story)?.expect("the story exists").snapshot;
            let head = tx.head_seq(id, story)?;
            snapshot.title = "wrong".to_string();
            tx.put_story(id, &snapshot, head)?;
            Ok(())
        })
        .expect("damaging the read model");

    let output = project
        .run(&["doctor", "--json"])
        .code(5)
        .get_output()
        .clone();
    let envelope: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("`--json` must answer one JSON document");

    assert_eq!(
        envelope["result"], "error",
        "a damaged project is still a failure — a scripted caller must not read `ok`"
    );
    assert_eq!(envelope["exit_code"], 5);

    let divergence = envelope["findings"]
        .as_array()
        .expect("findings is an array")
        .iter()
        .find(|finding| finding["code"] == "read_model_divergence")
        .expect("a row that disagrees with its events is a divergence");

    assert_eq!(divergence["subject"], "SH-1");
    assert_eq!(divergence["data"]["divergence"]["field"], "title");
    assert_eq!(divergence["data"]["divergence"]["persisted"], "wrong");
    assert_eq!(divergence["data"]["divergence"]["rebuilt"], "A");

    // And the prose a human reads is unchanged — it is these findings' own
    // messages, so nothing was traded away to gain the structure.
    let joined: Vec<&str> = envelope["findings"]
        .as_array()
        .expect("an array")
        .iter()
        .map(|finding| finding["message"].as_str().expect("a message"))
        .collect();
    assert!(
        envelope["error"]
            .as_str()
            .expect("an error string")
            .starts_with(&joined.join("\n")),
        "`error` must be the findings' own sentences: {}",
        envelope["error"]
    );
}

/// A healthy project answers the same shape, so a consumer reads `.findings`
/// on both outcomes without branching on which one it got.
#[test]
fn doctor_json_answers_an_empty_findings_array_when_healthy() {
    let env = TestEnv::shared();
    let project = env.project().seed_story("A").build();

    let output = project
        .run(&["doctor", "--json"])
        .code(0)
        .get_output()
        .clone();
    let envelope: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("`--json` must answer one JSON document");

    assert_eq!(envelope["result"], "ok");
    assert_eq!(
        envelope["findings"],
        serde_json::json!([]),
        "always present, never null, so `jq '.findings[]'` needs no guard"
    );
    // `issues` is the deprecated spelling of `advice`, emitted unchanged for
    // one release; while both ship they must be the same list.
    assert_eq!(
        envelope["advice"], envelope["issues"],
        "the deprecated key and its successor must not drift apart"
    );
}
