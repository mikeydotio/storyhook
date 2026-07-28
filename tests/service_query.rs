//! Properties of the read surface that a differential run cannot reach.
//!
//! `differential_query.rs` proves the store leg answers what the legacy leg
//! answers; what it cannot do is pin the *clock* (both legs read the system
//! one) or afford the twelve-story fixture the ordering defects need to become
//! visible. Those two things live here.
//!
//! # The orderings pinned below are DEFECTS, deliberately frozen
//!
//! `list` and `search` sort by story *number*; `graph`, `handoff`, `context`
//! and `summary`'s ready list sort **lexicographically**, so `SH-10` comes
//! before `SH-2`. The golden CLI corpus freezes the current bytes, so the wave
//! that ports these surfaces has to reproduce the defect rather than repair
//! it. These tests exist so that a later wave changing it does so on purpose:
//! they will fail, and their names say what the fix is.

use storyhook::cli::GraphMode;
use storyhook::domain::{Priority, StorySnapshot, SuperState};
use storyhook::output::StoryView;
use storyhook::service::{
    Clock, Ctx, ListFilters, NewStoryInput, QueryService, RelationService, StoryService,
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

/// KNOWN DEFECT, frozen by the golden corpus: `summary`'s ready preview is
/// ordered by `priority ASC, created_at ASC`, and when those tie — which they
/// do for any two stories created in the same second — the stable sort falls
/// back to the id-*string* order the story map arrived in.
#[test]
fn summary_previews_ready_stories_in_lexicographic_id_order() {
    let fixture = ServiceFixture::new();
    twelve_stories(&fixture);
    let summary = query(&fixture, |service| service.summary());
    assert_eq!(summary.ready_count, 12);
    assert_eq!(
        ids(&summary.ready_stories),
        ["SH-1", "SH-10", "SH-11", "SH-12", "SH-2"],
        "the preview is lexicographic, not numeric — a frozen defect"
    );
}

/// The same defect, in `context`'s Markdown body.
#[test]
fn context_lists_ready_stories_in_lexicographic_id_order() {
    let fixture = ServiceFixture::new();
    twelve_stories(&fixture);
    let body = query(&fixture, |service| service.context(false));
    let listed: Vec<&str> = body
        .lines()
        .skip_while(|line| !line.starts_with("## Ready to Work"))
        .filter_map(|line| line.strip_prefix("- "))
        .map(|line| line.split(' ').next().unwrap_or_default())
        .collect();
    assert_eq!(listed, ["SH-1", "SH-10", "SH-11", "SH-12", "SH-2"]);
}

/// And in `handoff`, which additionally splits open stories from archived ones
/// because the legacy path concatenated a directory listing with a SQL query.
#[test]
fn handoff_lists_open_stories_then_archived_ones_each_lexicographically() {
    let fixture = ServiceFixture::new();
    twelve_stories(&fixture);
    let ctx = fixture.ctx();
    for id in ["SH-11", "SH-2"] {
        StoryService::new(&ctx)
            .set_state(id, "done", None, None)
            .expect("closing");
    }

    let body = query(&fixture, |service| service.handoff(None));
    let created: Vec<&str> = section(&body, "## Created");
    let closed: Vec<&str> = section(&body, "## Closed");
    assert_eq!(
        created,
        [
            "SH-1", "SH-10", "SH-12", "SH-3", "SH-4", "SH-5", "SH-6", "SH-7", "SH-8", "SH-9"
        ],
        "open stories, lexicographically"
    );
    assert_eq!(
        closed,
        ["SH-11", "SH-2"],
        "archived stories, lexicographically, and after every open one"
    );
}

/// `graph`'s roots and leaves come off the same lexicographic map.
#[test]
fn graph_reports_roots_and_leaves_in_lexicographic_id_order() {
    let fixture = ServiceFixture::new();
    twelve_stories(&fixture);
    let view = query(&fixture, |service| service.graph(&GraphMode::Overview));
    let overview = view.overview.expect("an overview");
    assert_eq!(overview.total_open, 12);
    assert_eq!(overview.total_edges, 0);
    assert_eq!(overview.roots[..3], ["SH-1", "SH-10", "SH-11"]);
    assert_eq!(overview.roots, overview.leaves);
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

// --- what the surfaces include ---------------------------------------------

#[test]
fn show_finds_archived_and_deleted_stories_as_well_as_open_ones() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let open = new_story(&ctx, "open");
    let closed = new_story(&ctx, "closed");
    let deleted = new_story(&ctx, "deleted");
    StoryService::new(&ctx)
        .set_state(&closed, "done", None, None)
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
        .set_state(&closed, "done", None, None)
        .expect("closing");

    let summary = query(&fixture, |service| service.summary());
    let report = query(&fixture, |service| service.report_data());
    assert_eq!(summary.ready_count, 1);
    assert_eq!(report.summary.ready_count, summary.ready_count);
    assert_eq!(report.ready_ids, ["SH-1"]);
    assert_eq!(report.blocked_ids, Vec::<String>::new());
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
        .set_state(&closed, "done", None, None)
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
