//! Legacy versus store, across the whole read surface.
//!
//! `list` (every filter the CLI grammar allows), `show`, `search`, `next`,
//! `summary`, `report`, `graph`, `context` and `handoff`, driven through
//! `app::run` on a legacy `.storyhook` project and through `invoke::dispatch`
//! on a store-backed project seeded from the same catalog, compared envelope
//! for envelope. The harness — and its single deliberate normalization,
//! timestamps — lives in [`differential_support`].
//!
//! # Why the fixtures give ready stories distinct priorities
//!
//! `next`, `summary`, `report` and `context` order their ready lists by
//! `priority ASC, created_at ASC`, and `created_at` has **second** precision.
//! Two stories created inside one second tie on both keys, and the stable sort
//! then falls back to the order the list arrived in. The two legs are seeded
//! microseconds apart, so a fixture that relies on ties would sometimes have
//! them in one leg and not the other — a flake that says nothing about
//! behaviour. Distinct priorities remove the tie instead of sleeping through
//! it, which is both faster and a stronger assertion: the order is then a
//! property of the data rather than of the clock.

use storyhook::cli::{GraphMode, Invocation};
use storyhook::service::ListFilters;

mod differential_support;
use differential_support::Differential;

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

/// A `story new` invocation with a priority, which is how these fixtures keep
/// the ready-list orderings tie-free.
fn new_story_with_priority(title: &str, priority: &str) -> Invocation {
    Invocation::New {
        title: title.to_string(),
        state: None,
        story_type: None,
        description: None,
        priority: Some(priority.to_string()),
        labels: None,
        assignee: None,
    }
}

/// `story list` with every filter absent.
fn list() -> Invocation {
    list_of(ListFilters::default())
}

/// A `story list` invocation built from a filter set.
///
/// The CLI grammar spells the filters as twelve fields on an enum variant, and
/// Rust has no functional-update syntax for those — so the tests build a
/// [`ListFilters`], which does, and this widens it back out.
fn list_of(filters: ListFilters) -> Invocation {
    Invocation::List {
        state: filters.state,
        assignee: filters.assignee,
        flagged: filters.flagged,
        priority: filters.priority,
        label: filters.label,
        created_after: filters.created_after,
        updated_after: filters.updated_after,
        blocked: filters.blocked,
        ready: filters.ready,
        stale: filters.stale,
        phase: filters.phase,
        story_type: filters.story_type,
    }
}

/// A project with one story per priority, a type, labels, an assignee, a
/// blocking edge, an awaiting story and a closed one — enough for every
/// filter to have something to keep and something to drop.
fn seeded() -> Differential {
    let differential = Differential::new();
    differential.add_member("ada", Some("ada-gh"));

    differential.step("critical", new_story_with_priority("alpha", "critical"));
    differential.step("high", new_story_with_priority("beta", "high"));
    differential.step("medium", new_story_with_priority("gamma", "medium"));
    differential.step("low", new_story_with_priority("delta", "low"));
    differential.step("unprioritised", new_story("epsilon"));

    differential.step(
        "type + labels + assignee",
        Invocation::SetFields {
            id: "SH-1".into(),
            title: None,
            state: None,
            priority: None,
            assignee: Some("ada-gh".into()),
            labels: Some("infra,phase:2".into()),
            blocked: None,
            unblocked: false,
            json: None,
            story_type: Some("bug".into()),
            description: None,
        },
    );
    differential.step(
        "awaiting",
        Invocation::SetAwaiting {
            id: "SH-2".into(),
            awaiting: "a decision".into(),
        },
    );
    differential.step(
        "a blocking edge",
        Invocation::Relate {
            a: "SH-3".into(),
            relation: "blocks".into(),
            b: "SH-4".into(),
            remove: false,
        },
    );
    differential.step(
        "a closed story",
        Invocation::SetState {
            id: "SH-5".into(),
            state: "done".into(),
            comment: None,
            if_state: None,
        },
    );
    differential
}

// --- list ------------------------------------------------------------------

#[test]
fn listing_agrees_unfiltered() {
    let differential = seeded();
    differential.step("list", list());
}

#[test]
fn listing_agrees_for_every_single_filter() {
    let differential = seeded();
    let cases: Vec<(&str, Invocation)> = vec![
        (
            "--state todo",
            list_of(ListFilters {
                state: Some("todo".into()),
                ..Default::default()
            }),
        ),
        (
            "--state nonexistent",
            list_of(ListFilters {
                state: Some("limbo".into()),
                ..Default::default()
            }),
        ),
        (
            "--assignee",
            list_of(ListFilters {
                assignee: Some("ada".into()),
                ..Default::default()
            }),
        ),
        (
            "--assignee unknown",
            list_of(ListFilters {
                assignee: Some("nobody".into()),
                ..Default::default()
            }),
        ),
        (
            "--flagged",
            list_of(ListFilters {
                flagged: true,
                ..Default::default()
            }),
        ),
        (
            "--priority high",
            list_of(ListFilters {
                priority: Some("high".into()),
                ..Default::default()
            }),
        ),
        (
            "--priority high,low",
            list_of(ListFilters {
                priority: Some("high, low".into()),
                ..Default::default()
            }),
        ),
        (
            "--priority nonsense (filters nothing)",
            list_of(ListFilters {
                priority: Some("urgent".into()),
                ..Default::default()
            }),
        ),
        (
            "--label",
            list_of(ListFilters {
                label: Some("infra".into()),
                ..Default::default()
            }),
        ),
        (
            "--label a,b",
            list_of(ListFilters {
                label: Some("infra, missing".into()),
                ..Default::default()
            }),
        ),
        (
            "--label empty (filters nothing)",
            list_of(ListFilters {
                label: Some(" , ".into()),
                ..Default::default()
            }),
        ),
        (
            "--created-after the epoch",
            list_of(ListFilters {
                created_after: Some("1970-01-01T00:00:00Z".into()),
                ..Default::default()
            }),
        ),
        (
            "--created-after the far future",
            list_of(ListFilters {
                created_after: Some("2999-01-01T00:00:00Z".into()),
                ..Default::default()
            }),
        ),
        (
            "--updated-after the epoch",
            list_of(ListFilters {
                updated_after: Some("1970-01-01T00:00:00Z".into()),
                ..Default::default()
            }),
        ),
        (
            "--blocked",
            list_of(ListFilters {
                blocked: true,
                ..Default::default()
            }),
        ),
        (
            "--ready",
            list_of(ListFilters {
                ready: true,
                ..Default::default()
            }),
        ),
        (
            "--phase 2",
            list_of(ListFilters {
                phase: Some("2".into()),
                ..Default::default()
            }),
        ),
        (
            "--phase 9",
            list_of(ListFilters {
                phase: Some("9".into()),
                ..Default::default()
            }),
        ),
        (
            "--type bug",
            list_of(ListFilters {
                story_type: Some("bug".into()),
                ..Default::default()
            }),
        ),
        (
            "--type none",
            list_of(ListFilters {
                story_type: Some("none".into()),
                ..Default::default()
            }),
        ),
    ];
    for (label, invocation) in cases {
        differential.step(label, invocation);
    }
}

/// `--stale` both filters and annotates, and the annotation reads each
/// surviving story's event log for the *kind* of its last activity. `0h`
/// keeps every open story, so the row exercises the annotation rather than
/// just the empty case a real threshold would produce on a fresh fixture.
#[test]
fn listing_agrees_on_staleness_including_the_annotation() {
    let differential = seeded();
    differential.step(
        "--stale 0h",
        list_of(ListFilters {
            stale: Some("0h".into()),
            ..Default::default()
        }),
    );
    differential.step(
        "--stale 2h",
        list_of(ListFilters {
            stale: Some("2h".into()),
            ..Default::default()
        }),
    );
    differential.step(
        "--stale nonsense",
        list_of(ListFilters {
            stale: Some("soon".into()),
            ..Default::default()
        }),
    );
}

#[test]
fn listing_agrees_when_filters_combine() {
    let differential = seeded();
    differential.step(
        "--state todo --ready --type none",
        list_of(ListFilters {
            state: Some("todo".into()),
            ready: true,
            story_type: Some("none".into()),
            ..Default::default()
        }),
    );
    differential.step(
        "a combination nothing satisfies",
        list_of(ListFilters {
            state: Some("todo".into()),
            assignee: Some("ada".into()),
            priority: Some("low".into()),
            ..Default::default()
        }),
    );
}

#[test]
fn listing_agrees_on_an_empty_project() {
    let differential = Differential::new();
    differential.step("list", list());
    differential.step(
        "list --ready",
        list_of(ListFilters {
            ready: true,
            ..Default::default()
        }),
    );
}

/// The `flagged` flag needs a story the integrity pass actually flags.
/// `obviated-by` is the one such shape the public API can produce.
#[test]
fn listing_flagged_agrees_when_a_story_is_obviated() {
    let differential = seeded();
    differential.step(
        "obviate",
        Invocation::Relate {
            a: "SH-4".into(),
            relation: "obviated-by".into(),
            b: "SH-3".into(),
            remove: false,
        },
    );
    differential.step(
        "--flagged",
        list_of(ListFilters {
            flagged: true,
            ..Default::default()
        }),
    );
}

// --- show ------------------------------------------------------------------

#[test]
fn showing_agrees_for_open_archived_deleted_and_missing_stories() {
    let differential = seeded();
    differential.step(
        "delete one",
        Invocation::Delete {
            id: "SH-4".into(),
            reason: "obsolete".into(),
        },
    );

    for id in ["SH-1", "SH-2", "SH-3", "SH-4", "SH-5", "SH-99", "nonsense"] {
        differential.step("show", Invocation::Show { id: id.to_string() });
    }
}

// --- search ----------------------------------------------------------------

#[test]
fn searching_agrees_across_titles_comments_and_labels() {
    let differential = seeded();
    differential.step(
        "a comment to find",
        Invocation::Comment {
            id: "SH-3".into(),
            text: "mentions ZEBRA in passing".into(),
        },
    );
    differential.step(
        "close one, so the archive is searched too",
        Invocation::SetState {
            id: "SH-3".into(),
            state: "done".into(),
            comment: None,
            if_state: None,
        },
    );

    for query in ["alpha", "ALPHA", "zebra", "infra", "nothing-matches", ""] {
        differential.step(
            "search",
            Invocation::Search {
                query: query.to_string(),
            },
        );
    }
}

// --- next ------------------------------------------------------------------

#[test]
fn next_agrees_in_its_singular_plural_and_empty_forms() {
    let differential = seeded();
    for count in [1, 3, 100] {
        differential.step("next", Invocation::Next { count, phase: None });
    }
    differential.step(
        "next --phase 2",
        Invocation::Next {
            count: 5,
            phase: Some("2".into()),
        },
    );
    differential.step(
        "next --phase with no members",
        Invocation::Next {
            count: 5,
            phase: Some("9".into()),
        },
    );
}

#[test]
fn next_agrees_when_nothing_is_ready() {
    let differential = Differential::new();
    differential.step(
        "next",
        Invocation::Next {
            count: 1,
            phase: None,
        },
    );
}

/// A parent is not itself work: `next` returns leaves only.
#[test]
fn next_agrees_that_a_parent_is_not_offered() {
    let differential = Differential::new();
    differential.step("parent", new_story_with_priority("parent", "critical"));
    differential.step("child", new_story_with_priority("child", "high"));
    differential.step(
        "relate",
        Invocation::Relate {
            a: "SH-1".into(),
            relation: "parent-of".into(),
            b: "SH-2".into(),
            remove: false,
        },
    );
    differential.step(
        "next",
        Invocation::Next {
            count: 5,
            phase: None,
        },
    );
}

// --- summary and report ----------------------------------------------------

#[test]
fn summary_agrees_including_its_ready_preview() {
    let differential = seeded();
    differential.step("summary", Invocation::Summary);
}

#[test]
fn summary_agrees_on_an_empty_project() {
    let differential = Differential::new();
    differential.step("summary", Invocation::Summary);
}

#[test]
fn report_agrees_in_both_its_text_and_html_forms() {
    let differential = seeded();
    differential.step("report", Invocation::Report { html: false });
    differential.step("report --html", Invocation::Report { html: true });
}

#[test]
fn report_agrees_on_an_empty_project() {
    let differential = Differential::new();
    differential.step("report", Invocation::Report { html: false });
    differential.step("report --html", Invocation::Report { html: true });
}

// --- graph -----------------------------------------------------------------

#[test]
fn graph_agrees_in_every_mode() {
    let differential = seeded();
    differential.step(
        "a second edge, so there is a chain",
        Invocation::Relate {
            a: "SH-4".into(),
            relation: "blocks".into(),
            b: "SH-2".into(),
            remove: false,
        },
    );

    for (label, mode) in [
        ("overview", GraphMode::Overview),
        ("--critical-path", GraphMode::CriticalPath),
        ("--parallel-groups", GraphMode::ParallelGroups),
        ("--blocked-by SH-3", GraphMode::BlockedBy("SH-3".into())),
        ("--blocked-by a leaf", GraphMode::BlockedBy("SH-1".into())),
        ("--blocked-by missing", GraphMode::BlockedBy("SH-99".into())),
    ] {
        differential.step(label, Invocation::Graph { mode });
    }
}

#[test]
fn graph_agrees_on_a_project_with_no_edges() {
    let differential = Differential::new();
    differential.step("one story", new_story("lonely"));
    for mode in [
        GraphMode::Overview,
        GraphMode::CriticalPath,
        GraphMode::ParallelGroups,
    ] {
        differential.step("graph", Invocation::Graph { mode });
    }
}

// --- context ---------------------------------------------------------------

#[test]
fn context_agrees_in_both_formats_including_the_phase_table() {
    let differential = seeded();
    differential.step(
        "a phase grouping story",
        Invocation::Phase {
            action: storyhook::cli::PhaseAction::Create {
                phase: "2".into(),
                title: Some("Second".into()),
            },
        },
    );
    differential.step("context", Invocation::Context { format: None });
    differential.step(
        "context --format json",
        Invocation::Context {
            format: Some("json".into()),
        },
    );
    differential.step(
        "context --format markdown",
        Invocation::Context {
            format: Some("markdown".into()),
        },
    );
}

#[test]
fn context_agrees_on_an_empty_project() {
    let differential = Differential::new();
    differential.step("context", Invocation::Context { format: None });
    differential.step(
        "context --format json",
        Invocation::Context {
            format: Some("json".into()),
        },
    );
}

// --- handoff ---------------------------------------------------------------

#[test]
fn handoff_agrees_across_its_created_updated_and_closed_sections() {
    let differential = seeded();
    differential.step("handoff", Invocation::Handoff { since: None });
    differential.step(
        "handoff --since 1w",
        Invocation::Handoff {
            since: Some("1w".into()),
        },
    );
    differential.step(
        "handoff --since nonsense",
        Invocation::Handoff {
            since: Some("whenever".into()),
        },
    );
}

#[test]
fn handoff_agrees_on_an_empty_project() {
    let differential = Differential::new();
    differential.step("handoff", Invocation::Handoff { since: None });
}

// --- doctor ----------------------------------------------------------------

/// Every integrity shape the legacy doctor reports needs a project the public
/// API cannot produce, so what the differential can compare is the *healthy*
/// verdict — in both forms, and after enough real work to make "healthy" a
/// claim rather than a tautology. The damaged cases are store-only, in
/// `service_integrity.rs`, because the two legs cannot be damaged alike.
#[test]
fn doctor_agrees_on_a_healthy_project_in_both_its_forms() {
    let differential = seeded();
    differential.step("doctor", Invocation::Doctor { fix: false });
    differential.step("doctor --fix", Invocation::Doctor { fix: true });
    differential.step("doctor again", Invocation::Doctor { fix: false });
}

#[test]
fn doctor_agrees_on_an_empty_project() {
    let differential = Differential::new();
    differential.step("doctor", Invocation::Doctor { fix: false });
    differential.step("doctor --fix", Invocation::Doctor { fix: true });
}

/// An obviated story is flagged everywhere except in the doctor, which has
/// always suppressed that reason: it is an authoring decision, not damage.
#[test]
fn doctor_agrees_that_an_obviated_story_is_not_an_integrity_failure() {
    let differential = seeded();
    differential.step(
        "obviate",
        Invocation::Relate {
            a: "SH-4".into(),
            relation: "obviated-by".into(),
            b: "SH-3".into(),
            remove: false,
        },
    );
    differential.step("doctor", Invocation::Doctor { fix: false });
    differential.step("doctor --fix", Invocation::Doctor { fix: true });
}

/// **`story doctor --fix` destroys relationships to archived stories, and the
/// store leg deliberately does not.**
///
/// The legacy repair loop asks "does the other end of this edge exist?" of the
/// *open* stories only. Relate two stories, delete one — a completely ordinary
/// sequence — and the survivor's edges are treated as dangling and retracted.
/// The repair then reports the asymmetry it has just created, so the command
/// exits 5 and can never again exit 0: the data is gone and the diagnosis is
/// permanent.
///
/// Pinned rather than reproduced. This is the same call W2a made about the
/// burnt story number: the store does not have the defect, the difference is a
/// user-visible improvement, and it belongs in the flip's behaviour-change
/// notes as a deliberate line rather than as a surprise.
#[test]
fn doctor_fix_retracts_edges_to_deleted_stories_in_the_legacy_leg_only() {
    let differential = seeded();
    differential.step(
        "delete a story SH-4 is related to",
        Invocation::Delete {
            id: "SH-3".into(),
            reason: "superseded".into(),
        },
    );
    differential.step(
        "doctor is clean beforehand",
        Invocation::Doctor { fix: false },
    );

    let legacy = differential.legacy_only(Invocation::Doctor { fix: true });
    let store = differential.store_only(Invocation::Doctor { fix: true });

    let legacy_error = legacy.expect_err("the legacy repair breaks the project");
    assert_eq!(legacy_error.exit_code(), 5);
    assert!(
        legacy_error
            .to_string()
            .contains("missing inverse relation `blocked-by` on story `SH-4`"),
        "{legacy_error}"
    );
    assert_eq!(
        differential
            .legacy_only(Invocation::Show { id: "SH-4".into() })
            .map(relationship_count)
            .expect("showing SH-4"),
        0,
        "the legacy repair erased SH-4's relationships"
    );

    assert!(store.is_ok(), "the store repair leaves the project healthy");
    assert_eq!(
        differential
            .store_only(Invocation::Show { id: "SH-4".into() })
            .map(relationship_count)
            .expect("showing SH-4"),
        1,
        "SH-4 still knows it is blocked by the story that was deleted"
    );
    assert!(
        differential
            .store_only(Invocation::Doctor { fix: false })
            .is_ok(),
        "the store leg is still healthy afterwards"
    );
    assert_eq!(
        differential
            .legacy_only(Invocation::Doctor { fix: false })
            .expect_err("the legacy leg never recovers")
            .exit_code(),
        5,
        "the legacy diagnosis is permanent: repairing again cannot undo the retraction"
    );
}

/// How many relationships a `Response::Story` carries.
fn relationship_count(response: storyhook::output::Response) -> usize {
    match response {
        storyhook::output::Response::Story(view) => view.story.relationships.len(),
        other => panic!("expected a story response, got {other:?}"),
    }
}
