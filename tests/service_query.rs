//! Properties of the read surface that a differential run cannot reach.
//!
//! `differential_query.rs` proves the store leg answers what the legacy leg
//! answers; what it cannot do is pin the *clock* (both legs read the system
//! one) or afford the twelve-story fixture the ordering fixes below need to
//! become visible. Those two things live here.
//!
//! # Every id list sorts by story number (SH-64)
//!
//! `list` and `search` always sorted by story *number*; `graph` and `handoff`
//! used to sort **lexicographically** instead (`SH-10` before `SH-2`), because
//! both iterated a map keyed by the id string rather than sorting their output.
//! Fixed for both, and for `context`'s blocked list, found carrying the same
//! defect in passing.
//!
//! `summary` and `context`'s ready lists rank by `domain::ready_order`
//! (priority, then story number) instead of bare story number, a total order
//! the legacy `priority, created_at` comparator did not have (SH-63). The
//! lexicographic answer those two used to give was never a promise anyone
//! could rely on: it was whichever order a `BTreeMap` happened to iterate two
//! same-second, same-priority stories in.
//!
//! # The Closed section means closed (SH-280)
//!
//! `handoff` used to bucket "Closed" on `updated_at` inside the window and
//! `superstate == CLOSED`, which answers "was this touched, and is it
//! currently closed" rather than "was it closed in this window". Any append
//! to an already-closed story — `story hide`/`unhide` (SH-43), `story
//! comment` (SH-261), `commit-sync`'s commit link (SH-279) — bumps
//! `updated_at` without closing anything, so a story closed months ago was
//! reported as this session's work. Fixed by bucketing on `closed_at`
//! instead, the field the heading actually claims.

use storyhook::cli::GraphMode;
use storyhook::domain::{Priority, StorySnapshot, SuperState};
use storyhook::output::StoryView;
use storyhook::service::{
    Clock, Ctx, ListFilters, NewStoryInput, PrLinkService, QueryService, RelationService,
    StoryService,
};
use storyhook::store::{ProjectId, SqliteStore, Store};
use storyhook_test_support::{FIXTURE_NOW, ServiceFixture};

// --- helpers ---------------------------------------------------------------

/// Runs one query against the fixture, at the fixture's own pinned clock.
fn query<T>(
    fixture: &ServiceFixture,
    f: impl FnOnce(
        &QueryService<'_, <SqliteStore as Store>::ReadTx<'_>>,
    ) -> Result<T, storyhook::error::AppError>,
) -> T {
    query_at(fixture, FIXTURE_NOW, f)
}

/// [`query`], with the clock moved to `now`.
fn query_at<T>(
    fixture: &ServiceFixture,
    now: &str,
    f: impl FnOnce(
        &QueryService<'_, <SqliteStore as Store>::ReadTx<'_>>,
    ) -> Result<T, storyhook::error::AppError>,
) -> T {
    let project: ProjectId = fixture.project();
    fixture
        .store()
        .read(|tx| Ok(f(&QueryService::new(tx, project, now))))
        .expect("reading")
        .expect("querying")
}

fn new_story(ctx: &Ctx<'_, SqliteStore>, title: &str) -> String {
    StoryService::new(ctx)
        .create(&NewStoryInput {
            title: title.to_string(),
            ..NewStoryInput::default()
        })
        .expect("creating a story")
        .id
}

/// Twelve stories, so that `SH-10`, `SH-11` and `SH-12` exist to sort wrongly.
fn twelve_stories(fixture: &ServiceFixture) {
    let ctx = fixture.ctx();
    for index in 1..=12 {
        new_story(&ctx, &format!("story {index}"));
    }
}

fn ids(views: &[StoryView]) -> Vec<&str> {
    views.iter().map(|view| view.story.id.as_str()).collect()
}

// --- ordering --------------------------------------------------------------

#[test]
fn list_sorts_by_story_number() {
    let fixture = ServiceFixture::new();
    twelve_stories(&fixture);
    let views = query(&fixture, |service| service.list(&ListFilters::default()));
    assert_eq!(
        ids(&views),
        [
            "SH-1", "SH-2", "SH-3", "SH-4", "SH-5", "SH-6", "SH-7", "SH-8", "SH-9", "SH-10",
            "SH-11", "SH-12",
        ]
    );
}

#[test]
fn search_sorts_by_story_number() {
    let fixture = ServiceFixture::new();
    twelve_stories(&fixture);
    let views = query(&fixture, |service| service.search("story"));
    assert_eq!(ids(&views)[..3], ["SH-1", "SH-2", "SH-3"]);
    assert_eq!(ids(&views)[9..], ["SH-10", "SH-11", "SH-12"]);
}

/// `summary`'s ready preview ranks by `domain::ready_order`: all twelve
/// stories tie on priority (`None`, unset), so the story number decides —
/// `SH-1 … SH-5`, not the lexicographic `SH-1, SH-10, SH-11, SH-12, SH-2` a
/// `BTreeMap`'s own iteration order used to produce (SH-63).
#[test]
fn summary_previews_ready_stories_in_numeric_id_order() {
    let fixture = ServiceFixture::new();
    twelve_stories(&fixture);
    let summary = query(&fixture, |service| service.summary());
    assert_eq!(summary.ready_count, 12);
    assert_eq!(
        ids(&summary.ready_stories),
        ["SH-1", "SH-2", "SH-3", "SH-4", "SH-5"],
        "the same-priority tie breaks on story number now, not on id-string order"
    );
}

/// The same rule, in `context`'s Markdown body.
#[test]
fn context_lists_ready_stories_in_numeric_id_order() {
    let fixture = ServiceFixture::new();
    twelve_stories(&fixture);
    let body = query(&fixture, |service| service.context(false));
    let listed: Vec<&str> = body
        .lines()
        .skip_while(|line| !line.starts_with("## Ready to Work"))
        .filter_map(|line| line.strip_prefix("- "))
        .map(|line| line.split(' ').next().unwrap_or_default())
        .collect();
    assert_eq!(listed, ["SH-1", "SH-2", "SH-3", "SH-4", "SH-5"]);
}

/// `context`'s blocked section carried the same defect as `graph` and
/// `handoff` — found in passing while fixing those two (SH-64), fixed here
/// rather than filed, since it is the same file and the same one-line cause.
#[test]
fn context_lists_blocked_stories_in_numeric_id_order() {
    let fixture = ServiceFixture::new();
    twelve_stories(&fixture);
    let ctx = fixture.ctx();
    RelationService::new(&ctx)
        .relate("SH-1", "blocks", "SH-10", false)
        .expect("relating");
    RelationService::new(&ctx)
        .relate("SH-1", "blocks", "SH-2", false)
        .expect("relating");

    let body = query(&fixture, |service| service.context(false));
    let listed: Vec<&str> = body
        .lines()
        .skip_while(|line| !line.starts_with("## Blocked"))
        .skip(1)
        .take_while(|line| !line.starts_with("## "))
        .filter_map(|line| line.strip_prefix("- "))
        .map(|line| line.split(' ').next().unwrap_or_default())
        .collect();
    assert_eq!(listed, ["SH-2", "SH-10"]);
}

/// `story next` is asked twice with nothing changed in between — the exact
/// shape of the production defect SH-63 was filed against. Every candidate
/// ties on priority (`None`) *and* was created in the fixture's one pinned
/// second, so the old `priority, created_at` comparator had no third key and
/// answered from whichever order the read model happened to hand it. Twelve
/// stories, well past the `SH-9`/`SH-10` boundary where a lexicographic
/// fallback and a numeric one visibly disagree.
#[test]
fn next_orders_same_second_ties_by_story_number_and_agrees_with_itself() {
    let fixture = ServiceFixture::new();
    twelve_stories(&fixture);
    let first = query(&fixture, |service| service.next(12, None));
    let second = query(&fixture, |service| service.next(12, None));
    assert_eq!(
        ids(&first),
        [
            "SH-1", "SH-2", "SH-3", "SH-4", "SH-5", "SH-6", "SH-7", "SH-8", "SH-9", "SH-10",
            "SH-11", "SH-12"
        ],
    );
    assert_eq!(
        ids(&first),
        ids(&second),
        "asking twice must not change the answer"
    );
}

/// And in `handoff`, which additionally splits open stories from archived
/// ones (that split is not the defect; the legacy path concatenated a
/// directory listing with a SQL query, and grouping open before archived is
/// kept). Within each group, story number now decides the order, not the id
/// string: `SH-11` closed before `SH-2` here, so a lexicographic sort would
/// have listed `SH-11` first in "Closed" — the numeric one lists `SH-2` first.
#[test]
fn handoff_lists_open_stories_then_archived_ones_each_in_numeric_id_order() {
    let fixture = ServiceFixture::new();
    twelve_stories(&fixture);
    let ctx = fixture.ctx();
    for id in ["SH-11", "SH-2"] {
        StoryService::new(&ctx)
            .set_state(id, "done", None, None, None)
            .expect("closing");
    }

    let body = query(&fixture, |service| service.handoff(None));
    let created: Vec<&str> = section(&body, "## Created");
    let closed: Vec<&str> = section(&body, "## Closed");
    assert_eq!(
        created,
        [
            "SH-1", "SH-3", "SH-4", "SH-5", "SH-6", "SH-7", "SH-8", "SH-9", "SH-10", "SH-12"
        ],
        "open stories, by story number"
    );
    assert_eq!(
        closed,
        ["SH-2", "SH-11"],
        "archived stories, by story number, and after every open one"
    );
}

/// `graph`'s roots and leaves used to come straight off a lexicographic map;
/// now they sort by story number like every other id list.
#[test]
fn graph_reports_roots_and_leaves_in_numeric_id_order() {
    let fixture = ServiceFixture::new();
    twelve_stories(&fixture);
    let view = query(&fixture, |service| service.graph(&GraphMode::Overview));
    let overview = view.overview.expect("an overview");
    assert_eq!(overview.total_open, 12);
    assert_eq!(overview.total_edges, 0);
    assert_eq!(overview.roots[..3], ["SH-1", "SH-2", "SH-3"]);
    assert_eq!(overview.roots, overview.leaves);
}

/// `graph --blocked-by`'s transitive chain, same fix.
#[test]
fn graph_blocked_chain_reports_in_numeric_id_order() {
    let fixture = ServiceFixture::new();
    twelve_stories(&fixture);
    let ctx = fixture.ctx();
    let relations = RelationService::new(&ctx);
    relations
        .relate("SH-1", "blocks", "SH-10", false)
        .expect("relating");
    relations
        .relate("SH-1", "blocks", "SH-2", false)
        .expect("relating");
    relations
        .relate("SH-2", "blocks", "SH-11", false)
        .expect("relating");

    let view = query(&fixture, |service| {
        service.graph(&GraphMode::BlockedBy("SH-1".to_string()))
    });
    let chain = view.blocked_chain.expect("a blocked chain");
    assert_eq!(chain.blocked, ["SH-2", "SH-10", "SH-11"]);
}

/// `graph --parallel-groups`: members within a group and the groups
/// themselves both sort by story number now, not by `BTreeSet<String>`'s own
/// iteration order.
#[test]
fn graph_parallel_groups_sort_members_and_groups_by_story_number() {
    let fixture = ServiceFixture::new();
    twelve_stories(&fixture);
    let ctx = fixture.ctx();
    RelationService::new(&ctx)
        .relate("SH-10", "blocks", "SH-2", false)
        .expect("relating");

    let view = query(&fixture, |service| {
        service.graph(&GraphMode::ParallelGroups)
    });
    let groups = view.parallel_groups.expect("parallel groups");
    let paired = groups
        .iter()
        .find(|group| group.len() > 1)
        .expect("the related pair forms its own group");
    assert_eq!(paired, &["SH-2", "SH-10"], "members sort by story number");
    assert_eq!(
        groups[0],
        vec!["SH-1"],
        "the lowest-numbered group leads, not the lexicographically-first one"
    );
}

/// The id half of every `- SH-n title` line under `heading`.
fn section<'a>(body: &'a str, heading: &str) -> Vec<&'a str> {
    body.lines()
        .skip_while(|line| !line.starts_with(heading))
        .skip(1)
        .take_while(|line| !line.starts_with("## "))
        .filter_map(|line| line.strip_prefix("- "))
        .map(|line| line.split(' ').next().unwrap_or_default())
        .collect()
}

// --- the clock -------------------------------------------------------------

/// `--stale` is a function of the current time, so a test that cannot pin the
/// clock can only assert that it did not crash. With one pinned, the whole
/// annotation is checkable: the threshold, the day count, and the *kind* of
/// the story's last event.
#[test]
fn stale_filters_and_annotates_against_the_injected_clock() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let old = new_story(&ctx, "untouched");

    // A second story whose last event is a comment, three days later.
    let recent = {
        let ctx = fixture
            .ctx()
            .clock(Clock::Fixed("2026-01-04T00:00:00Z".into()));
        let id = new_story(&ctx, "commented on");
        StoryService::new(&ctx)
            .comment(&id, "still working")
            .expect("commenting");
        id
    };

    let filters = ListFilters {
        stale: Some("2d".into()),
        ..ListFilters::default()
    };
    let views = query_at(&fixture, "2026-01-05T00:00:00Z", |service| {
        service.list(&filters)
    });
    assert_eq!(ids(&views), [old.as_str()], "only the untouched story");

    let info = views[0].stale_info.as_ref().expect("an annotation");
    assert_eq!(info.last_activity_at, FIXTURE_NOW);
    assert_eq!(info.last_activity_type, "created");
    assert_eq!(info.days_stale, 4);

    // Widening the window past the second story's own activity picks it up,
    // and its annotation names the comment rather than the creation.
    let filters = ListFilters {
        stale: Some("0h".into()),
        ..ListFilters::default()
    };
    let views = query_at(&fixture, "2026-01-05T00:00:00Z", |service| {
        service.list(&filters)
    });
    let commented = views
        .iter()
        .find(|view| view.story.id == recent)
        .expect("the second story");
    let info = commented.stale_info.as_ref().expect("an annotation");
    assert_eq!(info.last_activity_type, "comment");
    assert_eq!(info.days_stale, 1);
}

#[test]
fn stale_rejects_a_duration_it_cannot_parse() {
    let fixture = ServiceFixture::new();
    let project = fixture.project();
    let filters = ListFilters {
        stale: Some("soon".into()),
        ..ListFilters::default()
    };
    let error = fixture
        .store()
        .read(|tx| Ok(QueryService::new(tx, project, FIXTURE_NOW).list(&filters)))
        .expect("reading")
        .expect_err("`soon` is not a duration");
    assert!(
        error.to_string().contains("invalid duration `soon`"),
        "{error}"
    );
}

#[test]
fn handoff_windows_are_measured_from_the_injected_clock() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    new_story(&ctx, "yesterday's work");

    let inside = query_at(&fixture, "2026-01-01T12:00:00Z", |service| {
        service.handoff(None)
    });
    assert!(inside.contains("## Created (1)"), "{inside}");

    let outside = query_at(&fixture, "2026-02-01T00:00:00Z", |service| {
        service.handoff(None)
    });
    assert_eq!(
        outside, "# Session Handoff\n\nNo changes in the specified period.\n",
        "a story a month old is outside the default 24-hour window"
    );

    let widened = query_at(&fixture, "2026-02-01T00:00:00Z", |service| {
        service.handoff(Some("8w"))
    });
    assert!(widened.contains("## Created (1)"), "{widened}");
}

#[test]
fn handoff_rejects_a_duration_it_cannot_parse() {
    let fixture = ServiceFixture::new();
    let project = fixture.project();
    let error = fixture
        .store()
        .read(|tx| Ok(QueryService::new(tx, project, FIXTURE_NOW).handoff(Some("whenever"))))
        .expect("reading")
        .expect_err("`whenever` is not a duration");
    assert!(
        error.to_string().contains("invalid duration `whenever`"),
        "{error}"
    );
}

// --- SH-280: "Closed" means closed, not touched -----------------------------

/// Five months past `FIXTURE_NOW` (`2026-01-01T00:00:00Z`) — where the SH-280
/// tests below do their work, so anything written at the fixture's own
/// instant is unambiguously outside the default 24-hour handoff window.
const JUNE: &str = "2026-06-01T00:00:00Z";
const JUNE_LATER: &str = "2026-06-01T01:00:00Z";

/// A story created and closed at `FIXTURE_NOW` — long closed by the time the
/// SH-280 tests query in `JUNE`.
fn closed_long_ago(fixture: &ServiceFixture, title: &str) -> String {
    let ctx = fixture.ctx();
    let id = new_story(&ctx, title);
    StoryService::new(&ctx)
        .set_state(&id, "done", None, None, None)
        .expect("closing");
    id
}

/// A story closed in January is not something this session closed, however
/// it was touched today.
///
/// `story comment` is one of the writes SH-261 let reach a closed story, and
/// every append bumps `updated_at`. Bucketing "Closed" on `superstate` and
/// `updated_at` reported a five-month-old closure as this session's work —
/// the false statement SH-280 is about.
#[test]
fn handoff_does_not_reclose_an_old_story_that_was_only_commented_on() {
    let fixture = ServiceFixture::new();
    let id = closed_long_ago(&fixture, "closed in January");

    let june = fixture.ctx().clock(Clock::Fixed(JUNE.into()));
    StoryService::new(&june)
        .comment(&id, "still the right call")
        .expect("commenting on a closed story");

    let body = query_at(&fixture, JUNE_LATER, |service| service.handoff(None));
    assert_eq!(
        section(&body, "## Updated"),
        [id.as_str()],
        "touched, not closed, in this window:\n{body}"
    );
    assert!(
        !body.contains("## Closed"),
        "nothing was closed in this window:\n{body}"
    );
}

/// The same false claim, reached through `story hide` (SH-43) instead of a
/// comment — the other write that appends to an already-closed story.
#[test]
fn handoff_does_not_reclose_an_old_story_that_was_only_hidden() {
    let fixture = ServiceFixture::new();
    let id = closed_long_ago(&fixture, "closed in January");

    let june = fixture.ctx().clock(Clock::Fixed(JUNE.into()));
    StoryService::new(&june)
        .hide(&id)
        .expect("hiding a closed story");

    let body = query_at(&fixture, JUNE_LATER, |service| service.handoff(None));
    assert_eq!(
        section(&body, "## Updated"),
        [id.as_str()],
        "archived, not closed, in this window:\n{body}"
    );
    assert!(
        !body.contains("## Closed"),
        "nothing was closed in this window:\n{body}"
    );
}

/// The other half of the promise: a story really closed in the window is
/// still reported — including one closed for the *second* time, so the fix
/// above cannot overshoot into "never reports Closed".
///
/// `fold_story` clears `closed_at` when a story reopens into an OPEN state
/// and restamps it on the next close, so the field always names the closure
/// that is current — which is what lets the bucket be one comparison.
#[test]
fn handoff_reports_a_reclosure_in_the_window_as_closed() {
    let fixture = ServiceFixture::new();
    let id = closed_long_ago(&fixture, "closed in January, finished in June");

    let june = fixture.ctx().clock(Clock::Fixed(JUNE.into()));
    let stories = StoryService::new(&june);
    stories.reopen(&id).expect("reopening");
    stories
        .set_state(&id, "done", None, None, None)
        .expect("re-closing");

    let body = query_at(&fixture, JUNE_LATER, |service| service.handoff(None));
    assert_eq!(section(&body, "## Closed"), [id.as_str()], "{body}");
}

/// A closure inside the window is reported even when the story's *last*
/// event is stamped before it.
///
/// `fold_story` sets `updated_at` to the last *replayed* event's `at`, not
/// the greatest one, and nothing orders an event's `at` against its
/// predecessor's — no schema CHECK, no guard in `append_and_fold`. A
/// restored import or a system clock that stepped back can leave
/// `updated_at` behind `closed_at`; the `updated_at` pre-filter this loop
/// used to open with then dropped the closure entirely — a handoff that
/// omits the one thing the session finished (SH-280).
#[test]
fn handoff_reports_a_closure_whose_last_event_is_stamped_earlier() {
    let fixture = ServiceFixture::new();
    let june = fixture.ctx().clock(Clock::Fixed(JUNE.into()));
    let id = new_story(&june, "closed in June");
    StoryService::new(&june)
        .set_state(&id, "done", None, None, None)
        .expect("closing");

    // Appended after the closure, stamped five months before it.
    StoryService::new(&fixture.ctx())
        .comment(&id, "from a clock that stepped back")
        .expect("commenting");

    let body = query_at(&fixture, JUNE_LATER, |service| service.handoff(None));
    assert_eq!(section(&body, "## Closed"), [id.as_str()], "{body}");
}

// --- what the surfaces include ---------------------------------------------

#[test]
fn show_finds_archived_and_deleted_stories_as_well_as_open_ones() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let open = new_story(&ctx, "open");
    let closed = new_story(&ctx, "closed");
    let deleted = new_story(&ctx, "deleted");
    StoryService::new(&ctx)
        .set_state(&closed, "done", None, None, None)
        .expect("closing");
    StoryService::new(&ctx)
        .delete(&deleted, "obsolete")
        .expect("deleting");

    for id in [&open, &closed, &deleted] {
        let view = query(&fixture, |service| service.show(id));
        assert_eq!(&view.story.id, id);
    }
    let error = query_at(&fixture, FIXTURE_NOW, |service| {
        Ok(service.show("SH-99").unwrap_err())
    });
    assert!(error.to_string().contains("story `SH-99` not found"));
}

#[test]
fn search_returns_bare_views_with_no_cross_story_facts() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let parent = new_story(&ctx, "parent");
    let child = new_story(&ctx, "child");
    RelationService::new(&ctx)
        .relate(&parent, "parent-of", &child, false)
        .expect("relating");

    let hit = query(&fixture, |service| service.search("parent"));
    assert_eq!(ids(&hit), [parent.as_str()]);
    assert!(
        hit[0].derived_relationships.is_empty() && hit[0].progress.is_none(),
        "search has never carried the cross-story pass"
    );

    // `show` on the same story does carry it, which is the contrast.
    let shown = query(&fixture, |service| service.show(&parent));
    assert!(shown.progress.is_some(), "show rolls progress up");
}

/// `referenced_by.prs` is a project-wide store read, gated behind
/// `include_derived` the same way `derived_relationships` is (SH-169) — so
/// `list` must not pay for it, only `show` does.
#[test]
fn referenced_by_prs_only_arrives_on_show_not_list() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let id = new_story(&ctx, "Referenced");
    PrLinkService::new(&ctx)
        .link(&id, "https://github.com/acme/widgets/pull/7", false)
        .expect("linking a PR");

    let listed = query(&fixture, |service| service.list(&ListFilters::default()));
    let listed_view = listed.iter().find(|v| v.story.id == id).unwrap();
    assert!(
        listed_view.referenced_by.prs.is_empty(),
        "prs are a project-wide read `list` must not pay for, same as \
         derived_relationships"
    );

    let shown = query(&fixture, |service| service.show(&id));
    assert_eq!(
        shown.referenced_by.prs.len(),
        1,
        "show computes the full project-wide pr_links read"
    );
    assert_eq!(shown.referenced_by.prs[0].number, 7);
}

/// `referenced_by.comment_mentions` is a project-wide scan of every other
/// story's comment thread, gated behind `include_derived` exactly as
/// `referenced_by.prs` is (SH-220) — so `list` must not pay for it.
#[test]
fn referenced_by_comment_mentions_only_arrive_on_show_not_list() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let mentioned = new_story(&ctx, "Mentioned");
    let mentioner = new_story(&ctx, "Mentioner");
    StoryService::new(&ctx)
        .comment(&mentioner, &format!("superseded by {mentioned}"))
        .expect("commenting");

    let listed = query(&fixture, |service| service.list(&ListFilters::default()));
    let listed_view = listed.iter().find(|v| v.story.id == mentioned).unwrap();
    assert!(
        listed_view.referenced_by.comment_mentions.is_empty(),
        "the scan is cross-story work `list` must not pay for, same as prs"
    );

    let shown = query(&fixture, |service| service.show(&mentioned));
    assert_eq!(
        shown.referenced_by.comment_mentions.len(),
        1,
        "show scans the whole project's comment threads"
    );
    let mention = &shown.referenced_by.comment_mentions[0];
    assert_eq!(mention.other_id, mentioner, "named by the commenting story");
    assert_eq!(mention.snippet, format!("superseded by {mentioned}"));

    let back = query(&fixture, |service| service.show(&mentioner));
    assert!(
        back.referenced_by.comment_mentions.is_empty(),
        "the mention points one way: nothing named the commenter"
    );
}

#[test]
fn an_epic_in_todo_with_an_active_child_shows_a_promoted_display_state() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let epic = new_story(&ctx, "epic");
    let child = new_story(&ctx, "child");
    RelationService::new(&ctx)
        .relate(&epic, "parent-of", &child, false)
        .expect("relating");
    StoryService::new(&ctx)
        .set_state(&child, "in-progress", None, None, None)
        .expect("moving the child");

    let shown = query(&fixture, |service| service.show(&epic));
    assert_eq!(
        shown.story.state, "todo",
        "the epic's own state is untouched"
    );
    assert_eq!(shown.display_state.as_deref(), Some("in-progress"));
}

#[test]
fn a_blocked_epic_keeps_its_display_state_even_with_an_active_child() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let epic = new_story(&ctx, "epic");
    let child = new_story(&ctx, "child");
    RelationService::new(&ctx)
        .relate(&epic, "parent-of", &child, false)
        .expect("relating");
    StoryService::new(&ctx)
        .set_state(&epic, "blocked", None, None, None)
        .expect("blocking the epic");
    StoryService::new(&ctx)
        .set_state(&child, "in-progress", None, None, None)
        .expect("moving the child");

    let shown = query(&fixture, |service| service.show(&epic));
    assert_eq!(
        shown.display_state, None,
        "blocked is a deliberate signal (SH-126); an active child must not override it"
    );
}

#[test]
fn next_offers_leaves_only_and_honours_count_and_phase() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let parent = new_story(&ctx, "epic");
    let child = new_story(&ctx, "leaf");
    let other = new_story(&ctx, "unrelated");
    RelationService::new(&ctx)
        .relate(&parent, "parent-of", &child, false)
        .expect("relating");
    StoryService::new(&ctx)
        .set_labels(&other, &["phase:3".into()], &[])
        .expect("labelling");

    let all = query(&fixture, |service| service.next(10, None));
    assert_eq!(ids(&all), [child.as_str(), other.as_str()]);

    let one = query(&fixture, |service| service.next(1, None));
    assert_eq!(one.len(), 1);

    let phased = query(&fixture, |service| service.next(10, Some("3")));
    assert_eq!(ids(&phased), [other.as_str()]);

    let empty = query(&fixture, |service| service.next(10, Some("9")));
    assert!(empty.is_empty());
}

/// `summary` counts every `is_ready` story; `report` counts only the *open*
/// ones. Those are different expressions that must agree, because `is_ready`
/// is false for a closed story — and a change that made them disagree would
/// move two user-visible numbers at once.
#[test]
fn summary_and_report_agree_about_the_ready_count_by_two_different_routes() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    new_story(&ctx, "open");
    let closed = new_story(&ctx, "closed");
    StoryService::new(&ctx)
        .set_state(&closed, "done", None, None, None)
        .expect("closing");

    let summary = query(&fixture, |service| service.summary());
    let report = query(&fixture, |service| service.report_data());
    assert_eq!(summary.ready_count, 1);
    assert_eq!(report.summary.ready_count, summary.ready_count);
    assert_eq!(report.ready_ids, ["SH-1"]);
    assert_eq!(report.blocked_ids, Vec::<String>::new());
}

/// Regression test for SH-126 (council verdict,
/// `.council/sh126-blocked-column-membership/DECISION.md`): a story in the
/// literal `blocked` state, with no unmet `blocked-by` edge and no
/// `awaiting` reason, used to be absent from `blocked_ids` and present in
/// `ready_ids` — `report_data` computes both purely from `is_ready`, which
/// never inspected `story.state`.
#[test]
fn report_data_treats_the_blocked_state_as_not_ready() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let id = new_story(&ctx, "manually blocked");
    StoryService::new(&ctx)
        .set_state(&id, "blocked", None, None, None)
        .expect("blocking");

    let report = query(&fixture, |service| service.report_data());
    assert_eq!(report.blocked_ids, [id.as_str()]);
    assert!(!report.ready_ids.contains(&id));
}

#[test]
fn list_filters_are_conjunctive() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let wanted = new_story(&ctx, "wanted");
    let other = new_story(&ctx, "other");
    StoryService::new(&ctx)
        .set_priority(&wanted, "high")
        .expect("prioritising");
    StoryService::new(&ctx)
        .set_labels(&wanted, &["infra".into()], &[])
        .expect("labelling");
    StoryService::new(&ctx)
        .set_priority(&other, "high")
        .expect("prioritising");

    let filters = ListFilters {
        priority: Some("high".into()),
        label: Some("infra".into()),
        ..ListFilters::default()
    };
    let views = query(&fixture, |service| service.list(&filters));
    assert_eq!(ids(&views), [wanted.as_str()]);

    let filters = ListFilters {
        priority: Some("low".into()),
        label: Some("infra".into()),
        ..ListFilters::default()
    };
    assert!(
        query(&fixture, |service| service.list(&filters)).is_empty(),
        "a story must satisfy every filter, not any of them"
    );
}

#[test]
fn list_includes_archived_and_deleted_stories_unless_a_filter_excludes_them() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    new_story(&ctx, "open");
    let closed = new_story(&ctx, "closed");
    StoryService::new(&ctx)
        .set_state(&closed, "done", None, None, None)
        .expect("closing");

    let all = query(&fixture, |service| service.list(&ListFilters::default()));
    assert_eq!(ids(&all), ["SH-1", "SH-2"]);
    assert_eq!(
        all.iter()
            .filter(|view| view.story.superstate == SuperState::Closed)
            .count(),
        1
    );

    let filters = ListFilters {
        state: Some("done".into()),
        ..ListFilters::default()
    };
    assert_eq!(
        ids(&query(&fixture, |service| service.list(&filters))),
        ["SH-2"]
    );
}

#[test]
fn an_unparseable_priority_list_filters_nothing() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    new_story(&ctx, "one");
    new_story(&ctx, "two");

    let filters = ListFilters {
        priority: Some("urgent, whenever".into()),
        ..ListFilters::default()
    };
    assert_eq!(
        query(&fixture, |service| service.list(&filters)).len(),
        2,
        "an all-unparseable list has always listed the whole project"
    );

    // One parseable entry is enough to make the filter bite.
    let filters = ListFilters {
        priority: Some("urgent, none".into()),
        ..ListFilters::default()
    };
    assert_eq!(
        query(&fixture, |service| service.list(&filters))
            .iter()
            .map(|view| view.story.priority.clone())
            .collect::<Vec<_>>(),
        [Priority::None, Priority::None]
    );
}

#[test]
fn graph_reports_a_missing_story_as_not_found() {
    let fixture = ServiceFixture::new();
    let project = fixture.project();
    let error = fixture
        .store()
        .read(|tx| {
            Ok(QueryService::new(tx, project, FIXTURE_NOW)
                .graph(&GraphMode::BlockedBy("SH-9".into())))
        })
        .expect("reading")
        .expect_err("SH-9 does not exist");
    assert!(error.to_string().contains("story `SH-9` not found"));
}

#[test]
fn the_story_map_is_keyed_by_id_and_covers_every_story() {
    let fixture = ServiceFixture::new();
    twelve_stories(&fixture);
    let map = query(&fixture, |service| service.story_map());
    assert_eq!(map.len(), 12);
    let keys: Vec<&str> = map.keys().map(String::as_str).collect();
    assert_eq!(keys[..3], ["SH-1", "SH-10", "SH-11"]);
    assert!(
        map.values()
            .all(|story: &StorySnapshot| story.superstate == SuperState::Open)
    );
}
