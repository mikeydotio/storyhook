//! The wire envelope's round-trip contract.
//!
//! The data-layer rearchitecture moves command execution out of the CLI
//! process and behind a transport, which is only safe because rendering never
//! moves with it: `app::run` returns an unrendered
//! [`Response`]/[`AppError`] pair, and `output.rs` turns that into bytes on
//! the caller's side. So *if* the envelope survives a serialize/deserialize
//! hop with its rendering unchanged, then carrying it over a wire cannot
//! change a single byte of CLI output — by construction, not by inspection.
//!
//! This file is the proof of that "if". It is deliberately about rendering
//! equality rather than value equality: `Response` has no `PartialEq` (its
//! view types carry `f64`-adjacent rollups and deeply nested snapshots), and
//! rendered bytes are the thing users and scripts actually depend on.
//!
//! Sibling files: `tests/golden_cli.rs` pins what the renderers produce,
//! `tests/error_contract.rs` pins exit codes and stream placement. This one
//! pins that a hop through JSON changes neither.

use storyhook::cli::{
    AbandonedAction, Attach, ConflictSide, DaemonAction, EpicAction, GraphMode, HistoryAction,
    HooksAction, Invocation, MemberInput, NewProjectRequest, NewProjectSpec, PhaseAction,
    PluginAction, ProjectAction, SettingsAction, SetupMode, SetupStrategy, StateAction,
    StoreAction, TypeAction, WebAction,
};
use storyhook::domain::{
    CommitReference, Member, Priority, ProgressRollup, StateDef, StoryComment, StoryEvent,
    StoryRelation, StorySnapshot, SuperState,
};
use storyhook::error::{AppError, WireError};
use storyhook::output::{
    BlockedChainView, ConfirmationPlan, DeletePlan, GraphOverview, GraphView, PhaseView,
    ProjectSnapshotView, PurgePlan, ReferencedBy, Response, SetPrefixPlan, SettingKind,
    SettingSource, SettingView, SetupPlan, StaleInfo, StoryView, SummaryView, UndeletePlan,
    render_error, render_response,
};
use storyhook::store::PrLink;

/// The four ways a `Response` can be rendered. Every case in this file is
/// checked in all of them, because `--quiet` and `--json` route through
/// different arms of `render_response` and `RawJson` deliberately ignores
/// both.
const RENDER_MODES: [(bool, bool); 4] =
    [(false, false), (true, false), (false, true), (true, true)];

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// A `Response` after a full JSON serialize/deserialize hop — the exact
/// treatment `/api/v1/invoke` will give it.
fn hop(response: &Response) -> Response {
    let encoded = serde_json::to_string(response).expect("a Response must serialize");
    serde_json::from_str(&encoded).unwrap_or_else(|e| {
        panic!("a Response must deserialize from its own output: {e}\n{encoded}")
    })
}

/// An `AppError` after the same hop, through its [`WireError`] mirror.
fn hop_error(error: &AppError) -> AppError {
    let encoded =
        serde_json::to_string(&WireError::from(error)).expect("a WireError must serialize");
    let decoded: WireError = serde_json::from_str(&encoded).unwrap_or_else(|e| {
        panic!("a WireError must deserialize from its own output: {e}\n{encoded}")
    });
    decoded.into()
}

fn snapshot(id: &str, title: &str) -> StorySnapshot {
    StorySnapshot {
        id: id.to_string(),
        title: title.to_string(),
        created_at: "2026-07-28T09:00:00Z".to_string(),
        updated_at: "2026-07-28T09:30:00Z".to_string(),
        state: "todo".to_string(),
        superstate: SuperState::Open,
        assignee: None,
        awaiting: None,
        comments: Vec::new(),
        referenced_by_commits: Vec::new(),
        relationships: Vec::new(),
        priority: Priority::None,
        labels: Vec::new(),
        story_type: None,
        description: None,
        closed_at: None,
        deleted: false,
        deleted_reason: None,
        hidden_at: None,
        draft: false,
    }
}

fn view(story: StorySnapshot) -> StoryView {
    StoryView {
        story,
        derived_relationships: Vec::new(),
        referenced_by: ReferencedBy::default(),
        warnings: Vec::new(),
        flagged_reasons: Vec::new(),
        stale_info: None,
        progress: None,
        display_state: None,
    }
}

/// A story with every optional field populated — the case where a dropped
/// `#[serde(skip_serializing_if)]` without a matching `#[serde(default)]`
/// would show up.
fn maximal_view() -> StoryView {
    StoryView {
        story: StorySnapshot {
            assignee: Some("ada-lovelace".to_string()),
            awaiting: Some("SH-9 to land".to_string()),
            comments: vec![
                StoryComment {
                    at: "2026-07-28T10:00:00Z".to_string(),
                    text: "Settled the shape of the trait.".to_string(),
                },
                StoryComment {
                    at: "2026-07-28T11:00:00Z".to_string(),
                    // Quotes, a backslash, a newline and non-ASCII: the four
                    // things a naive escaping bug loses.
                    text: "said \"ship it\" — path C:\\tmp\nsecond line".to_string(),
                },
            ],
            referenced_by_commits: vec![CommitReference {
                at: "2026-07-28T09:15:00Z".to_string(),
                sha: "abc123def456abc123def456abc123def456abc".to_string(),
                subject: "feat: land SH-1".to_string(),
            }],
            relationships: vec![
                StoryRelation {
                    relation: "blocks".to_string(),
                    other_id: "SH-2".to_string(),
                },
                StoryRelation {
                    relation: "parent-of".to_string(),
                    other_id: "SH-3".to_string(),
                },
            ],
            priority: Priority::Critical,
            labels: vec!["backend".to_string(), "api".to_string()],
            story_type: Some("spike".to_string()),
            description: Some("Multi\nline\tdescription with ünïcödé".to_string()),
            closed_at: Some("2026-07-28T12:00:00Z".to_string()),
            deleted: true,
            deleted_reason: Some("superseded".to_string()),
            hidden_at: Some("2026-07-28T13:00:00Z".to_string()),
            superstate: SuperState::Closed,
            state: "done".to_string(),
            ..snapshot("SH-1", "Everything, everywhere")
        },
        derived_relationships: vec![StoryRelation {
            relation: "sibling-of".to_string(),
            other_id: "SH-4".to_string(),
        }],
        referenced_by: ReferencedBy {
            commits: vec![CommitReference {
                at: "2026-07-28T09:15:00Z".to_string(),
                sha: "abc123def456abc123def456abc123def456abc".to_string(),
                subject: "feat: land SH-1".to_string(),
            }],
            prs: vec![PrLink {
                owner: "mikeydotio".to_string(),
                repo: "storyhook".to_string(),
                number: 42,
                url: "https://github.com/mikeydotio/storyhook/pull/42".to_string(),
                close_on_merge: true,
                status: "merged".to_string(),
                linked_at: "2026-07-28T09:20:00Z".to_string(),
                last_checked_at: Some("2026-07-28T10:00:00Z".to_string()),
            }],
        },
        warnings: vec!["a warning".to_string()],
        flagged_reasons: vec!["stale for 30 days".to_string(), "no assignee".to_string()],
        stale_info: Some(StaleInfo {
            last_activity_at: "2026-06-01T00:00:00Z".to_string(),
            last_activity_type: "comment".to_string(),
            days_stale: 57,
        }),
        progress: Some(ProgressRollup {
            children_done: 2,
            children_total: 5,
        }),
        display_state: Some("in-progress".to_string()),
    }
}

fn summary() -> SummaryView {
    SummaryView {
        total_open: 9,
        total_closed: 4,
        by_state: vec![("todo".to_string(), 5), ("done".to_string(), 4)],
        by_priority: vec![("critical".to_string(), 1), ("none".to_string(), 8)],
        by_type: vec![("story".to_string(), 12), ("bug".to_string(), 1)],
        blocked_count: 3,
        flagged_count: 2,
        ready_count: 4,
        ready_stories: vec![maximal_view(), view(snapshot("SH-2", "Second"))],
    }
}

// ---------------------------------------------------------------------------
// Response
// ---------------------------------------------------------------------------

/// Every `Response` variant, in both its empty and its populated shape where
/// the renderers treat those differently (`Stories`, `Issues` and
/// `PhaseList` all have dedicated "nothing here" branches).
fn response_corpus() -> Vec<(&'static str, Response)> {
    vec![
        (
            "message",
            Response::Message("initialized story project".to_string()),
        ),
        (
            "message_multiline_unicode",
            Response::Message("line one\nline two\ttabbed — ünïcödé \"quoted\"".to_string()),
        ),
        ("message_empty", Response::Message(String::new())),
        (
            "message_with_warnings",
            Response::MessageWithWarnings(
                "imported project with 3 stories".to_string(),
                vec![
                    "`git@github.com:acme/widgets.git` is already registered to project \
                     `widgets`; not re-registered by this restore"
                        .to_string(),
                ],
            ),
        ),
        (
            "message_with_warnings_empty",
            Response::MessageWithWarnings(
                "imported project with 3 stories".to_string(),
                Vec::new(),
            ),
        ),
        (
            "project_full",
            Response::Project(Box::new(storyhook::output::ProjectView {
                slug: "storyhook".to_string(),
                name: "storyhook".to_string(),
                prefix: "SH".to_string(),
                checkout: Some(std::path::PathBuf::from(
                    "/Volumes/Code/mikeyward/storyhook",
                )),
                origins: vec!["git@github.com:mikeydotio/storyhook.git".to_string()],
            })),
        ),
        // The shape six of the fourteen projects in the author's own store are
        // in, and the one a `skip_serializing_if` on `checkout` would silently
        // break: the field must travel as `null`, never vanish, or a script
        // cannot tell "no checkout here" from "the field was renamed".
        (
            "project_no_checkout",
            Response::Project(Box::new(storyhook::output::ProjectView {
                slug: "blink".to_string(),
                name: "blink".to_string(),
                prefix: "BLK".to_string(),
                checkout: None,
                origins: Vec::new(),
            })),
        ),
        (
            "story_minimal",
            Response::Story(Box::new(view(snapshot("SH-1", "A story")))),
        ),
        ("story_maximal", Response::Story(Box::new(maximal_view()))),
        ("stories_empty", Response::Stories(Vec::new(), None)),
        (
            "stories_with_message",
            Response::Stories(
                vec![maximal_view(), view(snapshot("SH-2", "Second"))],
                Some("2 ready stories".to_string()),
            ),
        ),
        (
            "stories_without_message",
            Response::Stories(vec![view(snapshot("SH-3", "Third"))], None),
        ),
        ("summary", Response::Summary(Box::new(summary()))),
        (
            "summary_empty",
            Response::Summary(Box::new(SummaryView {
                total_open: 0,
                total_closed: 0,
                by_state: Vec::new(),
                by_priority: Vec::new(),
                by_type: Vec::new(),
                blocked_count: 0,
                flagged_count: 0,
                ready_count: 0,
                ready_stories: Vec::new(),
            })),
        ),
        (
            "graph_overview",
            Response::Graph(Box::new(GraphView {
                critical_path: None,
                blocked_chain: None,
                parallel_groups: None,
                overview: Some(GraphOverview {
                    total_open: 9,
                    total_edges: 4,
                    roots: vec!["SH-1".to_string()],
                    leaves: vec!["SH-8".to_string(), "SH-9".to_string()],
                }),
            })),
        ),
        (
            "graph_all_modes",
            Response::Graph(Box::new(GraphView {
                critical_path: Some(vec!["SH-1".to_string(), "SH-2".to_string()]),
                blocked_chain: Some(BlockedChainView {
                    source: "SH-1".to_string(),
                    blocked: vec!["SH-2".to_string(), "SH-3".to_string()],
                }),
                parallel_groups: Some(vec![
                    vec!["SH-1".to_string()],
                    vec!["SH-2".to_string(), "SH-3".to_string()],
                ]),
                overview: Some(GraphOverview {
                    total_open: 3,
                    total_edges: 2,
                    roots: Vec::new(),
                    leaves: Vec::new(),
                }),
            })),
        ),
        (
            "graph_empty_modes",
            Response::Graph(Box::new(GraphView {
                critical_path: Some(Vec::new()),
                blocked_chain: Some(BlockedChainView {
                    source: "SH-1".to_string(),
                    blocked: Vec::new(),
                }),
                parallel_groups: Some(Vec::new()),
                overview: None,
            })),
        ),
        ("issues_empty", Response::Issues(Vec::new())),
        (
            "issues",
            Response::Issues(vec![
                "SH-1: dangling relation to SH-99".to_string(),
                "SH-2: unknown state `wip`".to_string(),
            ]),
        ),
        ("phase_list_empty", Response::PhaseList(Vec::new())),
        (
            "phase_list",
            Response::PhaseList(vec![
                PhaseView {
                    phase: "1".to_string(),
                    title: Some("Foundations".to_string()),
                    total: 4,
                    done: 2,
                    in_progress: 1,
                    todo: 1,
                    blocked: 0,
                    story_ids: vec!["SH-1".to_string(), "SH-2".to_string()],
                },
                PhaseView {
                    phase: "2".to_string(),
                    title: None,
                    total: 0,
                    done: 0,
                    in_progress: 0,
                    todo: 0,
                    blocked: 0,
                    story_ids: Vec::new(),
                },
            ]),
        ),
        (
            "project_settings_empty",
            Response::ProjectSettings(Vec::new()),
        ),
        // One of each `SettingSource`, and both sides of `settable` — the four
        // combinations the renderer branches on.
        (
            "project_settings",
            Response::ProjectSettings(vec![
                SettingView {
                    key: "sync.auto_transition".to_string(),
                    kind: SettingKind::Boolean,
                    value: Some("true".to_string()),
                    source: SettingSource::Default,
                    default: Some("true".to_string()),
                    settable: true,
                    managed_by: None,
                    description: "Whether commit-sync moves a mentioned story.".to_string(),
                    note: None,
                },
                SettingView {
                    key: "doctor.stale_threshold".to_string(),
                    kind: SettingKind::Duration,
                    value: None,
                    source: SettingSource::Unset,
                    default: None,
                    settable: true,
                    managed_by: None,
                    description: "How long before a story counts as stale.".to_string(),
                    note: Some("no command reads this yet".to_string()),
                },
                SettingView {
                    key: "github.sync".to_string(),
                    kind: SettingKind::Document,
                    value: Some("configured".to_string()),
                    source: SettingSource::Set,
                    default: None,
                    settable: false,
                    managed_by: Some("story github-sync".to_string()),
                    description: "The github-sync document — ünïcödé and \"quotes\".".to_string(),
                    note: None,
                },
            ]),
        ),
        (
            "raw_json",
            Response::RawJson("{\n  \"schema\": 1,\n  \"stories\": []\n}".to_string()),
        ),
        (
            "project_snapshot_empty",
            Response::ProjectSnapshot(Box::new(ProjectSnapshotView {
                slug: "storyhook".to_string(),
                prefix: "SH".to_string(),
                states: Vec::new(),
                members: Vec::new(),
                stories: Vec::new(),
                drafts: Vec::new(),
            })),
        ),
        (
            "project_snapshot",
            Response::ProjectSnapshot(Box::new(ProjectSnapshotView {
                slug: "api".to_string(),
                prefix: "API".to_string(),
                states: vec![
                    StateDef {
                        slug: "todo".to_string(),
                        super_state: SuperState::Open,
                        role: None,
                        description: Some("not started".to_string()),
                    },
                    StateDef {
                        slug: "done".to_string(),
                        super_state: SuperState::Closed,
                        role: None,
                        description: None,
                    },
                ],
                members: vec![Member {
                    id: "ada".to_string(),
                    display_name: "Ada Lovelace".to_string(),
                    email: Some("ada@example.com".to_string()),
                    github: Some("ada-gh".to_string()),
                    created_at: "2026-01-01T00:00:00Z".to_string(),
                }],
                stories: vec![maximal_view().story],
                drafts: vec![StorySnapshot {
                    draft: true,
                    ..snapshot("SH-2", "A draft, not yet live")
                }],
            })),
        ),
        ("story_history_empty", Response::StoryHistory(Vec::new())),
        (
            "story_history",
            Response::StoryHistory(vec![
                StoryEvent::StoryCreated {
                    at: "2026-01-01T00:00:00Z".to_string(),
                    title: "A story — with ünïcödé".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryCommentAdded {
                    at: "2026-01-01T00:01:00Z".to_string(),
                    text: "multi\nline".to_string(),
                },
            ]),
        ),
        (
            "confirmation_required",
            Response::ConfirmationRequired(Box::new(ConfirmationPlan::Delete(DeletePlan {
                slug: "storyhook".to_string(),
                name: "storyhook — the tracker".to_string(),
                prefix: "SH".to_string(),
                stories: 47,
                events: 312,
                checkouts: vec![
                    "/Volumes/Code/storyhook".to_string(),
                    "/Volumes/Code/storyhook/.claude/worktrees/sh-117".to_string(),
                ],
            }))),
        ),
        (
            "confirmation_required_empty_project",
            Response::ConfirmationRequired(Box::new(ConfirmationPlan::Delete(DeletePlan {
                slug: "empty".to_string(),
                name: "empty".to_string(),
                prefix: "EM".to_string(),
                stories: 0,
                events: 0,
                checkouts: Vec::new(),
            }))),
        ),
        (
            "confirmation_required_purge",
            Response::ConfirmationRequired(Box::new(ConfirmationPlan::Purge(PurgePlan {
                id: "SH-20".to_string(),
                title: "A story created in error".to_string(),
                deleted_reason: Some("created in error".to_string()),
                events: 14,
                retracted: vec![
                    ("SH-5".to_string(), "blocks".to_string()),
                    ("SH-9".to_string(), "child-of".to_string()),
                ],
            }))),
        ),
        (
            "confirmation_required_purge_unclaimed",
            Response::ConfirmationRequired(Box::new(ConfirmationPlan::Purge(PurgePlan {
                id: "SH-20".to_string(),
                title: "A story created in error".to_string(),
                deleted_reason: None,
                events: 1,
                retracted: Vec::new(),
            }))),
        ),
        (
            "confirmation_required_set_prefix",
            Response::ConfirmationRequired(Box::new(ConfirmationPlan::SetPrefix(SetPrefixPlan {
                slug: "storyhook".to_string(),
                name: "storyhook — the tracker".to_string(),
                old_prefix: "SH".to_string(),
                new_prefix: "AGE".to_string(),
                stories: 47,
                relationships: 12,
                github_bases: 3,
            }))),
        ),
        (
            "confirmation_required_undelete",
            Response::ConfirmationRequired(Box::new(ConfirmationPlan::Undelete(UndeletePlan {
                id: "SH-20".to_string(),
                title: "A story created in error".to_string(),
                deleted_reason: Some("created in error".to_string()),
            }))),
        ),
        (
            "confirmation_required_undelete_no_reason",
            Response::ConfirmationRequired(Box::new(ConfirmationPlan::Undelete(UndeletePlan {
                id: "SH-20".to_string(),
                title: "A story created in error".to_string(),
                deleted_reason: None,
            }))),
        ),
        (
            "setup_required",
            Response::SetupRequired(Box::new(SetupPlan {
                owner: "acme".to_string(),
                repo: "widgets".to_string(),
                local_story_count: 12,
                open_issue_count: 30,
                exact_match_count: 4,
            })),
        ),
    ]
}

/// The headline property: a hop through JSON changes nothing a caller can
/// see, in any of the four rendering modes.
#[test]
fn every_response_renders_identically_after_a_wire_hop() {
    for (label, response) in response_corpus() {
        let hopped = hop(&response);
        for (json, quiet) in RENDER_MODES {
            assert_eq!(
                render_response(&response, json, quiet),
                render_response(&hopped, json, quiet),
                "`{label}` rendered differently after a wire hop (json={json}, quiet={quiet})"
            );
        }
    }
}

/// A second hop must not drift either: a first pass that quietly normalizes
/// something (dropping an empty vec, reordering a map) can still be stable
/// afterwards, and would pass the test above while having lost data.
#[test]
fn a_second_wire_hop_is_a_fixed_point() {
    for (label, response) in response_corpus() {
        let once = serde_json::to_string(&hop(&response)).expect("serializing");
        let twice = serde_json::to_string(&hop(&hop(&response))).expect("serializing");
        assert_eq!(once, twice, "`{label}` was not stable across two wire hops");
    }
}

/// A delete plan's fields stay directly under `plan`, alongside the `confirm`
/// discriminant rather than nested beneath it.
///
/// The dashboard reads `err.body.plan.slug`, `.stories`, `.events` and
/// `.checkouts` out of the 409 it gets from `DELETE /api/repos/{id}` and draws
/// its own modal from them (`src/web_dashboard.html`). `ConfirmationPlan` is an
/// enum now, and the *externally* tagged default would have moved every one of
/// those a level down — a browser-only breakage, invisible to a Rust round-trip
/// test, which is exactly why this asserts on the JSON rather than on a value.
#[test]
fn a_delete_confirmation_keeps_the_flat_shape_the_dashboard_reads() {
    let response = Response::ConfirmationRequired(Box::new(ConfirmationPlan::Delete(DeletePlan {
        slug: "storyhook".to_string(),
        name: "storyhook — the tracker".to_string(),
        prefix: "SH".to_string(),
        stories: 47,
        events: 312,
        checkouts: vec!["/repo".to_string()],
    })));

    let rendered = render_response(&response, true, false);
    let document: serde_json::Value = serde_json::from_str(&rendered).expect("valid JSON");
    let plan = &document["plan"];

    assert_eq!(document["result"], "confirmation-required");
    assert_eq!(
        plan["confirm"], "delete",
        "the kind is stated, not inferred"
    );
    assert_eq!(plan["slug"], "storyhook");
    assert_eq!(plan["name"], "storyhook — the tracker");
    assert_eq!(plan["stories"], 47);
    assert_eq!(plan["events"], 312);
    assert_eq!(plan["checkouts"][0], "/repo");
}

/// Every `Response` variant has a corpus row.
///
/// **This guard did not exist until SH-120, and its absence is why it exists.**
/// `AppError` has `the_error_corpus_covers_every_variant` and `Invocation` has
/// `the_invocation_corpus_covers_every_variant`; `Response` had neither, and
/// `response_variants_travel_as_snake_case_keys` hand-lists its variants, so it
/// would not notice a new one. A permanent wire variant could therefore land
/// with nothing proving it survives the daemon hop — which is exactly what
/// `Response::Project` was about to do.
///
/// The match is what makes this exhaustive rather than a count that drifts: a
/// new variant fails to compile here until somebody names it, and naming it is
/// the moment they are asked for a corpus row.
#[test]
fn the_response_corpus_covers_every_variant() {
    fn variant_of(response: &Response) -> &'static str {
        match response {
            Response::Message(_) => "message",
            Response::MessageWithWarnings(..) => "message_with_warnings",
            Response::Story(_) => "story",
            Response::Stories(..) => "stories",
            Response::Summary(_) => "summary",
            Response::Graph(_) => "graph",
            Response::Issues(_) => "issues",
            Response::PhaseList(_) => "phase_list",
            Response::ProjectSettings(_) => "project_settings",
            Response::RawJson(_) => "raw_json",
            Response::ProjectSnapshot(_) => "project_snapshot",
            Response::StoryHistory(_) => "story_history",
            Response::ConfirmationRequired(_) => "confirmation_required",
            Response::Project(_) => "project",
            Response::SetupRequired(_) => "setup_required",
        }
    }

    const EVERY_VARIANT: [&str; 15] = [
        "message",
        "message_with_warnings",
        "story",
        "stories",
        "summary",
        "graph",
        "issues",
        "phase_list",
        "project_settings",
        "raw_json",
        "project_snapshot",
        "story_history",
        "confirmation_required",
        "project",
        "setup_required",
    ];

    let mut covered: Vec<&str> = response_corpus()
        .iter()
        .map(|(_, response)| variant_of(response))
        .collect();
    covered.sort_unstable();
    covered.dedup();

    let missing: Vec<&&str> = EVERY_VARIANT
        .iter()
        .filter(|name| !covered.contains(name))
        .collect();
    assert!(
        missing.is_empty(),
        "these Response variants have no row in `response_corpus`: {missing:?}\n\n\
         A permanent wire variant with no corpus row is one nothing proves survives the \
         daemon hop. Add a row — and if the variant has a nullable field, add one row for \
         each side of it."
    );
}

/// `Response`'s wire form is externally tagged, so the variant travels as the
/// single top-level key. Pinned because a receiver that has to *guess* the
/// variant from the payload's shape is the failure mode this envelope exists
/// to avoid, and because `rename_all` is easy to lose in a refactor.
#[test]
fn response_variants_travel_as_snake_case_keys() {
    let expected = [
        ("message", "message"),
        ("message_with_warnings", "message_with_warnings"),
        ("story_minimal", "story"),
        ("stories_empty", "stories"),
        ("summary", "summary"),
        ("graph_overview", "graph"),
        ("issues", "issues"),
        ("phase_list", "phase_list"),
        ("project_settings", "project_settings"),
        ("raw_json", "raw_json"),
        ("confirmation_required", "confirmation_required"),
        ("project_full", "project"),
        ("setup_required", "setup_required"),
    ];
    let corpus = response_corpus();
    for (label, key) in expected {
        let response = &corpus
            .iter()
            .find(|(name, _)| *name == label)
            .unwrap_or_else(|| panic!("corpus must contain `{label}`"))
            .1;
        let encoded: serde_json::Value =
            serde_json::to_value(response).expect("a Response must serialize");
        let keys: Vec<&String> = encoded
            .as_object()
            .unwrap_or_else(|| panic!("`{label}` must serialize to an object: {encoded}"))
            .keys()
            .collect();
        assert_eq!(
            keys,
            vec![key],
            "`{label}` must travel under exactly one key, `{key}`"
        );
    }
}

// ---------------------------------------------------------------------------
// AppError
// ---------------------------------------------------------------------------

/// Every `AppError` variant, constructed with a payload that would survive a
/// lossy hop only by accident.
fn error_corpus() -> Vec<AppError> {
    vec![
        AppError::Usage("unknown flag `--typo`".to_string()),
        AppError::Validation("invalid priority `urgent`".to_string()),
        AppError::NotFound("story `SH-99` not found".to_string()),
        AppError::LockTimeout("another process holds the project lock".to_string()),
        AppError::Integrity("SH-1: dangling relation".to_string()),
        AppError::Storage("failed to read .storyhook/stories: no such file".to_string()),
        // The prefixed variants: their `Display` is not their payload, so a
        // wire form that carried the rendered message would double the prefix.
        AppError::GithubAuth("token expired".to_string()),
        AppError::GithubApi("422 Unprocessable Entity".to_string()),
        AppError::SyncConflict("remote moved SH-1 to `done`".to_string()),
        AppError::StateConflict("todo".to_string(), "in-progress".to_string()),
        AppError::SyncErrors("SH-1: 404 not found".to_string()),
    ]
}

/// An exhaustive `match`, so an eleventh `AppError` variant stops this file
/// compiling until it has a wire form and a corpus row. The same guard
/// `tests/error_contract.rs` uses for the exit-code table.
fn variant_name(error: &AppError) -> &'static str {
    match error {
        AppError::Usage(_) => "Usage",
        AppError::Validation(_) => "Validation",
        AppError::NotFound(_) => "NotFound",
        AppError::LockTimeout(_) => "LockTimeout",
        AppError::Integrity(_) => "Integrity",
        AppError::Storage(_) => "Storage",
        AppError::GithubAuth(_) => "GithubAuth",
        AppError::GithubApi(_) => "GithubApi",
        AppError::SyncConflict(_) => "SyncConflict",
        AppError::StateConflict(..) => "StateConflict",
        AppError::SyncErrors(_) => "SyncErrors",
    }
}

/// The corpus must name every variant — the compile-time guard above only
/// forces a *name*, this forces an actual value to round-trip.
#[test]
fn the_error_corpus_covers_every_variant() {
    let mut names: Vec<&str> = error_corpus().iter().map(variant_name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(
        names.len(),
        11,
        "every AppError variant needs a row in `error_corpus`; found {names:?}"
    );
}

/// What the CLI does with an error is exactly two things — render it and exit
/// with its code — so those two are what must survive the hop. The variant
/// identity is checked as well, because two variants can share an exit code
/// (`Integrity` and `Storage` are both 5) and the daemon's HTTP status
/// mapping in `web.rs` discriminates on the variant, not the code.
#[test]
fn every_error_survives_a_wire_hop() {
    for error in error_corpus() {
        let hopped = hop_error(&error);

        assert_eq!(
            variant_name(&error),
            variant_name(&hopped),
            "`{}` came back as a different variant",
            variant_name(&error)
        );
        assert_eq!(
            error.exit_code(),
            hopped.exit_code(),
            "`{}` came back with a different exit code",
            variant_name(&error)
        );
        assert_eq!(
            error.to_string(),
            hopped.to_string(),
            "`{}` came back with a different message",
            variant_name(&error)
        );
        for json in [false, true] {
            assert_eq!(
                render_error(&error, json),
                render_error(&hopped, json),
                "`{}` rendered differently after a wire hop (json={json})",
                variant_name(&error)
            );
        }
    }
}

/// `StateConflict` is the one variant whose payload is structured rather than
/// prose: `tests/move_if_state.rs` and `story.sh`'s compare-and-swap claim
/// both read `expected`/`actual` out of the JSON envelope, and `web.rs` maps
/// it to HTTP 409. Flattening it to a string on the wire would force a
/// receiver to parse the two values back out of the rendered sentence.
#[test]
fn state_conflict_keeps_its_two_fields_apart_on_the_wire() {
    let error = AppError::StateConflict("todo".to_string(), "in-progress".to_string());
    let encoded = serde_json::to_value(WireError::from(&error)).expect("serializing");
    assert_eq!(
        encoded,
        serde_json::json!({
            "kind": "state_conflict",
            "expected": "todo",
            "actual": "in-progress",
        })
    );

    // And the rendered conflict envelope — the bytes `story.sh` parses — is
    // reproduced exactly from the wire form alone.
    let rendered = render_error(&error, true);
    let value: serde_json::Value = serde_json::from_str(&rendered).expect("the envelope is JSON");
    assert_eq!(value["result"], "conflict");
    assert_eq!(value["expected"], "todo");
    assert_eq!(value["actual"], "in-progress");
    assert_eq!(render_error(&hop_error(&error), true), rendered);
}

/// The wire form names its variant in snake_case under `kind`, which is the
/// shape the daemon's error envelope is specified to carry.
#[test]
fn error_variants_travel_under_a_kind_tag() {
    let kinds: Vec<String> = error_corpus()
        .iter()
        .map(|e| {
            serde_json::to_value(WireError::from(e)).expect("serializing")["kind"]
                .as_str()
                .expect("`kind` must be a string")
                .to_string()
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            "usage",
            "validation",
            "not_found",
            "lock_timeout",
            "integrity",
            "storage",
            "github_auth",
            "github_api",
            "sync_conflict",
            "state_conflict",
            "sync_errors",
        ]
    );
}

// ---------------------------------------------------------------------------
// Invocation
// ---------------------------------------------------------------------------

/// Every `Invocation` variant, plus every variant of the six action enums
/// they nest. `Invocation` derives `PartialEq`, so unlike `Response` this one
/// can assert on values directly.
fn invocation_corpus() -> Vec<Invocation> {
    vec![
        Invocation::Help,
        // All four `NewProjectRequest`/`Attach` shapes. `Ask` is on the wire
        // deliberately: the client is the only process that can answer it, and
        // the dispatcher's refusal of it is only reachable if it survives a
        // round trip.
        Invocation::Project {
            action: ProjectAction::New(NewProjectRequest::Ask),
        },
        Invocation::Project {
            action: ProjectAction::New(NewProjectRequest::Stated(NewProjectSpec {
                attach: Attach::Cwd,
                prefix: "API".to_string(),
                name: None,
                no_agents_md: false,
            })),
        },
        Invocation::Project {
            action: ProjectAction::New(NewProjectRequest::Stated(NewProjectSpec {
                attach: Attach::Path("../elsewhere".to_string()),
                prefix: "API".to_string(),
                name: Some("Elsewhere — renamed".to_string()),
                no_agents_md: true,
            })),
        },
        Invocation::Project {
            action: ProjectAction::New(NewProjectRequest::Stated(NewProjectSpec {
                attach: Attach::Nothing,
                prefix: "OFF".to_string(),
                name: Some("off-machine".to_string()),
                no_agents_md: true,
            })),
        },
        Invocation::Project {
            action: ProjectAction::New(NewProjectRequest::Stated(NewProjectSpec {
                attach: Attach::Path("../elsewhere".to_string()),
                prefix: "API".to_string(),
                name: Some("Elsewhere — renamed".to_string()),
                no_agents_md: true,
            })),
        },
        Invocation::Project {
            action: ProjectAction::Delete { force: true },
        },
        Invocation::Project {
            action: ProjectAction::SetPrefix {
                new_prefix: "AGE".to_string(),
                force: false,
            },
        },
        Invocation::Project {
            action: ProjectAction::List,
        },
        Invocation::Project {
            action: ProjectAction::Settings(SettingsAction::List),
        },
        Invocation::Project {
            action: ProjectAction::Settings(SettingsAction::Get {
                key: "sync.auto_transition".to_string(),
            }),
        },
        Invocation::Project {
            action: ProjectAction::Settings(SettingsAction::Set {
                key: "doctor.stale_threshold".to_string(),
                value: "14d".to_string(),
            }),
        },
        Invocation::Project {
            action: ProjectAction::Settings(SettingsAction::Unset {
                key: "doctor.stale_threshold".to_string(),
            }),
        },
        Invocation::New {
            title: "A story — with ünïcödé and \"quotes\"".to_string(),
            state: Some("todo".to_string()),
            story_type: Some("spike".to_string()),
            description: Some("multi\nline".to_string()),
            priority: Some("high".to_string()),
            labels: Some(vec!["backend".to_string(), "api".to_string()]),
            assignee: Some("ada-lovelace".to_string()),
            draft: true,
        },
        Invocation::MemberAdd {
            input: MemberInput::Identity("Ada Lovelace <ada@example.com>".to_string()),
        },
        Invocation::MemberAdd {
            input: MemberInput::Github("ada".to_string()),
        },
        Invocation::State {
            action: StateAction::List,
        },
        Invocation::State {
            action: StateAction::Add {
                slug: "review".to_string(),
                superstate: "OPEN".to_string(),
                role: Some("active".to_string()),
                description: Some("Awaiting code review".to_string()),
            },
        },
        Invocation::State {
            action: StateAction::Set {
                slug: "review".to_string(),
                superstate: None,
                role: Some("none".to_string()),
                description: None,
                clear_description: true,
                move_stories_to: Some("todo".to_string()),
            },
        },
        Invocation::State {
            action: StateAction::Remove {
                slug: "review".to_string(),
                move_stories_to: None,
            },
        },
        Invocation::State {
            action: StateAction::Reorder {
                order: vec!["todo".to_string(), "done".to_string()],
            },
        },
        Invocation::List {
            state: Some("todo".to_string()),
            assignee: Some("ada".to_string()),
            flagged: true,
            priority: Some("high".to_string()),
            label: Some("api".to_string()),
            created_after: Some("7d".to_string()),
            updated_after: Some("2h".to_string()),
            blocked: true,
            ready: true,
            stale: Some("30d".to_string()),
            phase: Some("1".to_string()),
            story_type: Some("bug".to_string()),
            drafts: true,
        },
        Invocation::Search {
            query: "trait".to_string(),
        },
        Invocation::Next {
            count: 3,
            phase: Some("2".to_string()),
        },
        Invocation::Summary,
        Invocation::Report { html: true },
        Invocation::Doctor { fix: true },
        Invocation::Show {
            id: "SH-1".to_string(),
        },
        Invocation::Comment {
            id: "SH-1".to_string(),
            text: "a comment".to_string(),
        },
        Invocation::Assign {
            id: "SH-1".to_string(),
            member: "ada".to_string(),
        },
        Invocation::SetState {
            id: "SH-1".to_string(),
            state: "done".to_string(),
            comment: Some("shipped".to_string()),
            if_state: Some("in-progress".to_string()),
        },
        Invocation::SetAwaiting {
            id: "SH-1".to_string(),
            awaiting: "SH-2".to_string(),
        },
        Invocation::ClearAwaiting {
            id: "SH-1".to_string(),
        },
        Invocation::SetPriority {
            id: "SH-1".to_string(),
            priority: "critical".to_string(),
        },
        Invocation::SetLabels {
            id: "SH-1".to_string(),
            add: vec!["api".to_string()],
            remove: vec!["backend".to_string()],
        },
        Invocation::Reopen {
            id: "SH-1".to_string(),
            force: true,
        },
        Invocation::Hide {
            id: "SH-1".to_string(),
        },
        Invocation::Unhide {
            id: "SH-1".to_string(),
        },
        Invocation::Publish {
            id: "SH-1".to_string(),
        },
        Invocation::HideState {
            state: "done".to_string(),
            force: true,
        },
        Invocation::Delete {
            id: "SH-1".to_string(),
            reason: "superseded".to_string(),
        },
        Invocation::Purge {
            id: "SH-1".to_string(),
            force: true,
        },
        Invocation::BulkUpdate {
            updates: vec![
                ("SH-1".to_string(), "done".to_string()),
                ("SH-2".to_string(), "todo".to_string()),
            ],
        },
        Invocation::Import {
            file: Some("stories.yaml".to_string()),
        },
        Invocation::Decompose {
            file: None,
            stdin: true,
            dry_run: true,
        },
        Invocation::Export,
        Invocation::ImportProject {
            file: "export.json".to_string(),
            legacy_links: true,
        },
        Invocation::Context {
            format: Some("json".to_string()),
        },
        Invocation::Handoff {
            since: Some("7d".to_string()),
        },
        Invocation::Phase {
            action: PhaseAction::List,
        },
        Invocation::Phase {
            action: PhaseAction::Show {
                phase: "1".to_string(),
            },
        },
        Invocation::Phase {
            action: PhaseAction::Add {
                id: "SH-1".to_string(),
                phase: "1".to_string(),
            },
        },
        Invocation::Phase {
            action: PhaseAction::Remove {
                id: "SH-1".to_string(),
            },
        },
        Invocation::Phase {
            action: PhaseAction::Create {
                phase: "2".to_string(),
                title: Some("Foundations".to_string()),
            },
        },
        Invocation::Type {
            action: TypeAction::List,
        },
        Invocation::Type {
            action: TypeAction::Add {
                slug: "spike".to_string(),
                description: Some("A timeboxed investigation".to_string()),
                emoji: Some("🔬".to_string()),
            },
        },
        Invocation::Type {
            action: TypeAction::Set {
                slug: "spike".to_string(),
                description: Some("A timeboxed investigation".to_string()),
                clear_description: false,
                emoji: None,
                clear_emoji: true,
            },
        },
        Invocation::Type {
            action: TypeAction::Remove {
                slug: "spike".to_string(),
            },
        },
        Invocation::Epic {
            action: EpicAction::List,
        },
        Invocation::Epic {
            action: EpicAction::Show {
                id: "SH-1".to_string(),
            },
        },
        Invocation::Epic {
            action: EpicAction::Create {
                title: "An epic".to_string(),
            },
        },
        Invocation::Epic {
            action: EpicAction::Add {
                epic_id: "SH-1".to_string(),
                story_id: "SH-2".to_string(),
            },
        },
        Invocation::Graph {
            mode: GraphMode::Overview,
        },
        Invocation::Graph {
            mode: GraphMode::CriticalPath,
        },
        Invocation::Graph {
            mode: GraphMode::BlockedBy("SH-1".to_string()),
        },
        Invocation::Graph {
            mode: GraphMode::ParallelGroups,
        },
        Invocation::SetFields {
            id: "SH-1".to_string(),
            title: Some("New title".to_string()),
            state: Some("done".to_string()),
            priority: Some("low".to_string()),
            assignee: Some("none".to_string()),
            labels: Some("api,backend".to_string()),
            blocked: Some("SH-2".to_string()),
            unblocked: true,
            json: Some("{\"title\":\"x\"}".to_string()),
            story_type: Some("bug".to_string()),
            description: Some("new description".to_string()),
        },
        Invocation::Relate {
            a: "SH-1".to_string(),
            relation: "blocks".to_string(),
            b: "SH-2".to_string(),
            remove: true,
        },
        Invocation::Hooks {
            action: HooksAction::Install,
        },
        Invocation::Hooks {
            action: HooksAction::Uninstall,
        },
        Invocation::Hooks {
            action: HooksAction::List,
        },
        Invocation::Hooks {
            action: HooksAction::Test {
                event_type: "story-created".to_string(),
            },
        },
        Invocation::Scaffold {
            kind: "claude".to_string(),
        },
        Invocation::CommitSync {
            since: Some("7d".to_string()),
        },
        Invocation::GithubSync {
            id: Some("SH-1".to_string()),
            dry_run: true,
            resolve: Some(ConflictSide::Local),
            strategy: None,
            mode: None,
        },
        // Both sides of `--resolve`, and its absence — which is not a default
        // but the state that makes a conflicting sync refuse (SH-152).
        Invocation::GithubSync {
            id: Some("SH-1".to_string()),
            dry_run: false,
            resolve: Some(ConflictSide::Remote),
            strategy: None,
            mode: None,
        },
        Invocation::GithubSync {
            id: None,
            dry_run: false,
            resolve: None,
            strategy: None,
            mode: None,
        },
        // `--strategy`/`--mode`, stated together — the only shape that is not
        // a refusal (SH-153's D2).
        Invocation::GithubSync {
            id: None,
            dry_run: false,
            resolve: None,
            strategy: Some(SetupStrategy::MatchTitles),
            mode: Some(SetupMode::Off),
        },
        Invocation::LinkPr {
            id: "SH-1".to_string(),
            url: "https://github.com/acme/widgets/pull/7".to_string(),
            close_on_merge: true,
        },
        Invocation::LinkPr {
            id: "SH-1".to_string(),
            url: "https://github.com/acme/widgets/pull/7".to_string(),
            close_on_merge: false,
        },
        Invocation::UnlinkPr {
            id: "SH-1".to_string(),
            url: "https://github.com/acme/widgets/pull/7".to_string(),
        },
        Invocation::PrCheck {
            id: Some("SH-1".to_string()),
        },
        Invocation::PrCheck { id: None },
        Invocation::HelpTopic {
            topic: "states".to_string(),
        },
        Invocation::HelpCompact,
        Invocation::HelpAll,
        Invocation::Plugin {
            action: PluginAction::Install {
                target: "claude".to_string(),
            },
        },
        Invocation::Plugin {
            action: PluginAction::Uninstall {
                target: "claude".to_string(),
            },
        },
        Invocation::Web {
            action: WebAction::Start { port: 3456 },
        },
        Invocation::Web {
            action: WebAction::Stop,
        },
        Invocation::Web {
            action: WebAction::Status,
        },
        Invocation::Web {
            action: WebAction::Serve { port: 19000 },
        },
        Invocation::Web {
            action: WebAction::Open,
        },
        Invocation::Web {
            action: WebAction::Address,
        },
        Invocation::SessionStart,
        Invocation::Update {
            check: true,
            force: true,
        },
        Invocation::Version,
        Invocation::ProjectSnapshot,
        Invocation::History {
            action: HistoryAction::Read {
                id: "SH-7".to_string(),
            },
        },
        Invocation::History {
            action: HistoryAction::Restore {
                id: "SH-7".to_string(),
                events: vec![
                    StoryEvent::StoryCreated {
                        at: "2026-01-01T00:00:00Z".to_string(),
                        title: "Before the mutation".to_string(),
                        state: "todo".to_string(),
                    },
                    StoryEvent::StoryCommentAdded {
                        at: "2026-01-01T00:01:00Z".to_string(),
                        text: "a comment the undo restores".to_string(),
                    },
                ],
            },
        },
        Invocation::History {
            action: HistoryAction::Restore {
                id: "SH-7".to_string(),
                events: Vec::new(),
            },
        },
        Invocation::Migrate {
            path: Some("/checkout".to_string()),
            dry_run: false,
        },
        Invocation::Migrate {
            path: None,
            dry_run: true,
        },
        Invocation::Daemon {
            action: DaemonAction::Status,
        },
        Invocation::Daemon {
            action: DaemonAction::Stop { force: true },
        },
        Invocation::DoctorAbandoned {
            action: AbandonedAction::List,
        },
        Invocation::DoctorAbandoned {
            action: AbandonedAction::Clear {
                request_id: Some("req-1".to_string()),
            },
        },
        Invocation::DoctorAbandoned {
            action: AbandonedAction::Clear { request_id: None },
        },
        Invocation::Store {
            action: StoreAction::New {
                path: "/tmp/scratch/store.db".to_string(),
            },
        },
        Invocation::Store {
            action: StoreAction::Backup {
                label: Some("pre-migration".to_string()),
            },
        },
        Invocation::Store {
            action: StoreAction::Backup { label: None },
        },
    ]
}

/// An exhaustive `match`, so a new `Invocation` variant stops this file
/// compiling until someone has decided how it crosses the wire.
fn invocation_name(invocation: &Invocation) -> &'static str {
    match invocation {
        Invocation::Help => "Help",
        Invocation::Project { .. } => "Project",
        Invocation::New { .. } => "New",
        Invocation::MemberAdd { .. } => "MemberAdd",
        Invocation::State { .. } => "State",
        Invocation::List { .. } => "List",
        Invocation::Search { .. } => "Search",
        Invocation::Next { .. } => "Next",
        Invocation::Summary => "Summary",
        Invocation::Report { .. } => "Report",
        Invocation::Doctor { .. } => "Doctor",
        Invocation::DoctorAbandoned { .. } => "DoctorAbandoned",
        Invocation::Show { .. } => "Show",
        Invocation::Comment { .. } => "Comment",
        Invocation::Assign { .. } => "Assign",
        Invocation::SetState { .. } => "SetState",
        Invocation::SetAwaiting { .. } => "SetAwaiting",
        Invocation::ClearAwaiting { .. } => "ClearAwaiting",
        Invocation::SetPriority { .. } => "SetPriority",
        Invocation::SetLabels { .. } => "SetLabels",
        Invocation::Reopen { .. } => "Reopen",
        Invocation::Hide { .. } => "Hide",
        Invocation::Unhide { .. } => "Unhide",
        Invocation::Publish { .. } => "Publish",
        Invocation::HideState { .. } => "HideState",
        Invocation::Delete { .. } => "Delete",
        Invocation::Purge { .. } => "Purge",
        Invocation::BulkUpdate { .. } => "BulkUpdate",
        Invocation::Import { .. } => "Import",
        Invocation::Decompose { .. } => "Decompose",
        Invocation::Export => "Export",
        Invocation::ImportProject { .. } => "ImportProject",
        Invocation::Context { .. } => "Context",
        Invocation::Handoff { .. } => "Handoff",
        Invocation::Phase { .. } => "Phase",
        Invocation::Type { .. } => "Type",
        Invocation::Epic { .. } => "Epic",
        Invocation::Graph { .. } => "Graph",
        Invocation::SetFields { .. } => "SetFields",
        Invocation::Relate { .. } => "Relate",
        Invocation::Hooks { .. } => "Hooks",
        Invocation::Scaffold { .. } => "Scaffold",
        Invocation::CommitSync { .. } => "CommitSync",
        Invocation::GithubSync { .. } => "GithubSync",
        Invocation::LinkPr { .. } => "LinkPr",
        Invocation::UnlinkPr { .. } => "UnlinkPr",
        Invocation::PrCheck { .. } => "PrCheck",
        Invocation::HelpTopic { .. } => "HelpTopic",
        Invocation::HelpCompact => "HelpCompact",
        Invocation::HelpAll => "HelpAll",
        Invocation::Plugin { .. } => "Plugin",
        Invocation::Web { .. } => "Web",
        Invocation::Daemon { .. } => "Daemon",
        Invocation::Store { .. } => "Store",
        Invocation::SessionStart => "SessionStart",
        Invocation::Update { .. } => "Update",
        Invocation::Version => "Version",
        Invocation::ProjectSnapshot => "ProjectSnapshot",
        Invocation::History { .. } => "History",
        Invocation::Migrate { .. } => "Migrate",
    }
}

/// Forces a corpus row for every variant, not just a name in the `match`
/// above. The count is the number of `Invocation` variants; a new one lands
/// here as a failure with the missing name spelled out.
#[test]
fn the_invocation_corpus_covers_every_variant() {
    let mut names: Vec<&str> = invocation_corpus().iter().map(invocation_name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(
        names.len(),
        60,
        "every Invocation variant needs a row in `invocation_corpus`; found {names:?}"
    );
}

/// The request half of the envelope, round-tripped by value — `Invocation`
/// derives `Eq`, so this is an exact comparison rather than a rendering one.
#[test]
fn every_invocation_survives_a_wire_hop() {
    for invocation in invocation_corpus() {
        let encoded = serde_json::to_string(&invocation).expect("an Invocation must serialize");
        let decoded: Invocation = serde_json::from_str(&encoded).unwrap_or_else(|e| {
            panic!(
                "`{}` must deserialize from its own output: {e}\n{encoded}",
                invocation_name(&invocation)
            )
        });
        assert_eq!(
            invocation,
            decoded,
            "`{}` changed across a wire hop",
            invocation_name(&invocation)
        );
    }
}
