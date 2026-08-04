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
use storyhook::domain::StoryEvent;
use storyhook::store::test_support::inject_events;
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

#[test]
fn doctor_does_not_flag_known_story_type() {
    let env = TestEnv::shared();
    let project = env.project().build();
    project.run(&["type", "add", "feature"]).success();
    project.run(&["new", "A"]).success();
    project.run(&["set", "SH-1", "--type", "feature"]).success();

    project.run(&["doctor"]).success();
}
