//! `daemon::engine::{reconcile_tick, reconcile_restart_tick}` — the glue that
//! wakes the Full Auto reconcile loop (SH-466).
//!
//! `tests/engine_reconcile.rs` and `tests/engine_restart.rs` already prove
//! `EngineService::reconcile`/`reconcile_after_restart`'s own logic
//! exhaustively. This file proves only what these two functions add over
//! that: run selection across the whole store and across every project, the
//! per-run `Ctx`/checkout resolution recipe, and a *real* `ShellDispatcher`
//! construction — the same division of labor `tests/daemon_github_poll.rs`
//! states for `github_poll::tick` versus `run_check`.
//!
//! No daemon, no thread, no sleep: both functions are plain `pub fn`s over a
//! `Store`, called directly against a `ServiceFixture`. `window_alive`'s
//! real implementation spawns `tmux display-message` against a session name
//! that does not exist, which answers `false` in well under a second
//! whether or not `tmux` itself is even installed on the machine running
//! this suite — exactly the observation both `Interrupted` and `WindowGone`
//! need, so no working tmux server is required to prove the wiring end to
//! end.
//!
//! # Why `STORYHOOK_DISPATCH_SCRIPT` is pinned here
//!
//! `context_for_run` resolves a real dispatch script the same way `stop
//! --now` does, and that resolution walks the machine's own installed
//! plugins before falling back to a dev checkout. This file's first run
//! found exactly that ambiguity: a stale Codex plugin installed on the
//! development machine shadowed this checkout's own `plugins/story/bin/
//! story.sh` and failed every test on `check_dispatch_protocol`, though
//! nothing here ever executes the resolved script — only `tmux` is
//! spawned. `with_fake_dispatch_script` pins the override every other
//! resolution source cannot outrank, so these tests are deterministic
//! regardless of what else happens to be installed on the machine running
//! them.

mod store_support;

use storyhook::api::dispatch::REQUIRED_DISPATCH_PROTOCOL;
use storyhook::daemon::engine::{reconcile_restart_tick, reconcile_tick};
use storyhook::service::engine::{EngineService, HardStopKind, StartRequest};
use storyhook::service::{Clock, Ctx, NewStoryInput, StoryService};
use storyhook::store::{
    EngineAgent, EngineLaneRecord, EngineLaneState, EngineScope, ReadOps, Store, WriteOps,
};
use storyhook_test_support::{FIXTURE_NOW, FakeDispatcher, ServiceFixture};

/// Serializes this file's tests against the shared process environment and
/// pins `STORYHOOK_DISPATCH_SCRIPT` at a valid fake `story.sh` for the
/// duration of `f`, restoring whatever the variable held before.
///
/// `std::env::set_var` is process-wide and `cargo test` runs a file's tests
/// on multiple threads by default; every test in this file wants the exact
/// same override, so the lock exists to make the get-then-restore sequence
/// atomic against another thread's own restore, not to prevent two threads
/// racing to set different values (there are none here).
fn with_fake_dispatch_script<T>(f: impl FnOnce() -> T) -> T {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let dir = storyhook_test_support::scratch_dir();
    let script = dir.path().join("story.sh");
    std::fs::write(
        &script,
        format!("#!/usr/bin/env bash\nDISPATCH_PROTOCOL={REQUIRED_DISPATCH_PROTOCOL}\n"),
    )
    .expect("writing the fake dispatch script");

    let previous = std::env::var("STORYHOOK_DISPATCH_SCRIPT").ok();
    // SAFETY: serialized by `LOCK`, held for the whole get-set-run-restore
    // sequence, so no other thread in this process reads or writes this
    // variable while it is set to the fixture's own value.
    unsafe { std::env::set_var("STORYHOOK_DISPATCH_SCRIPT", &script) };
    let result = f();
    unsafe {
        match &previous {
            Some(value) => std::env::set_var("STORYHOOK_DISPATCH_SCRIPT", value),
            None => std::env::remove_var("STORYHOOK_DISPATCH_SCRIPT"),
        }
    }
    result
}

/// Starts a run through the ordinary service door. The dispatcher this run
/// starts with is irrelevant — `start` never calls it — the fake is here
/// only because `EngineService::new` requires one.
fn started_run(fixture: &ServiceFixture, lanes: u32) -> String {
    let fake = FakeDispatcher::default();
    EngineService::new(&fixture.ctx(), &fake)
        .start(StartRequest {
            scope: EngineScope::Project,
            lanes,
            agent: EngineAgent::Codex,
        })
        .unwrap()
        .id
}

/// Puts `story` into lane `index`, naming a tmux window that cannot exist —
/// the real `ShellDispatcher::window_alive` answers `false` for it exactly
/// as it would for a session that died with the machine.
fn occupy(fixture: &ServiceFixture, run_id: &str, index: u32, story: &str) {
    let mut lane = fixture
        .store()
        .read(|tx| tx.engine_lanes(run_id))
        .unwrap()
        .into_iter()
        .find(|lane| lane.lane_index == index)
        .unwrap();
    lane.state = EngineLaneState::Working;
    lane.story_id = Some(story.to_string());
    lane.window_name = Some(format!(
        "storyhook-test-window-that-cannot-exist-{story}-{index}"
    ));
    lane.worktree_path = Some(format!("/tmp/wt/{story}"));
    lane.dispatched_at = Some(FIXTURE_NOW.to_string());
    lane.last_observed_at = FIXTURE_NOW.to_string();
    fixture
        .store()
        .write(|tx| tx.put_engine_lane(&lane))
        .unwrap();
}

fn lane_at(fixture: &ServiceFixture, run_id: &str, index: u32) -> EngineLaneRecord {
    fixture
        .store()
        .read(|tx| tx.engine_lanes(run_id))
        .unwrap()
        .into_iter()
        .find(|lane| lane.lane_index == index)
        .unwrap()
}

fn new_story(fixture: &ServiceFixture, title: &str) -> String {
    StoryService::new(&fixture.ctx())
        .create(&NewStoryInput {
            title: title.into(),
            ..NewStoryInput::default()
        })
        .unwrap()
        .id
}

/// A second, fully independent project — real states, types, checkout and
/// prefix — with one live run already occupying its lane 0 with a window
/// that cannot exist. Everything this run needs to resolve is real: this is
/// deliberately not a fixture the store's own writers would refuse
/// (`create_engine_run` refuses a `project_slug` naming no project, and
/// `delete_project` cascades a project's own engine runs away with it — a
/// live run whose project cannot resolve is not a reachable state, so this
/// file does not construct one).
///
/// Returns the run id, for the caller to inspect after a tick.
fn second_project_run(fixture: &ServiceFixture, slug: &str, prefix: &str) -> String {
    let project = store_support::seed_project(fixture.store(), slug, prefix);
    let ctx = Ctx::new(
        fixture.store(),
        project,
        fixture.cwd(),
        fixture.env().clone(),
    )
    .clock(Clock::Fixed(FIXTURE_NOW.to_string()));
    let story =
        store_support::create_story(fixture.store(), project, "second project work", FIXTURE_NOW)
            .to_id(prefix);
    let fake = FakeDispatcher::default();
    let run_id = EngineService::new(&ctx, &fake)
        .start(StartRequest {
            scope: EngineScope::Project,
            lanes: 1,
            agent: EngineAgent::Codex,
        })
        .unwrap()
        .id;
    let mut lane = fixture
        .store()
        .read(|tx| tx.engine_lanes(&run_id))
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    lane.state = EngineLaneState::Working;
    lane.story_id = Some(story.clone());
    lane.window_name = Some(format!("storyhook-test-window-that-cannot-exist-{story}"));
    lane.worktree_path = Some(format!("/tmp/wt/{story}"));
    lane.dispatched_at = Some(FIXTURE_NOW.to_string());
    lane.last_observed_at = FIXTURE_NOW.to_string();
    fixture
        .store()
        .write(|tx| tx.put_engine_lane(&lane))
        .unwrap();
    run_id
}

/// `reconcile_tick` records an interrupted-looking lane as `WindowGone`
/// through the REAL dispatcher, then releases it below the breaker threshold.
/// This proves script resolution, `Ctx` construction and the continuation
/// policy are wired together, not just individually correct.
#[test]
fn reconcile_tick_quarantines_a_dead_window_through_the_real_dispatcher() {
    with_fake_dispatch_script(|| {
        let fixture = ServiceFixture::new();
        let story = new_story(&fixture, "lane work");
        let run_id = started_run(&fixture, 1);
        occupy(&fixture, &run_id, 0, &story);

        reconcile_tick(fixture.store(), fixture.env());

        let lane = lane_at(&fixture, &run_id, 0);
        assert_eq!(lane.state, EngineLaneState::Idle);
        assert_eq!(
            lane.outcome.as_deref(),
            Some(HardStopKind::WindowGone.as_str())
        );
        let run = fixture
            .store()
            .read(|tx| tx.engine_run(&run_id))
            .unwrap()
            .unwrap();
        assert_eq!(run.recent_quarantines.len(), 1);
        assert_eq!(
            run.recent_quarantines[0].story_id.as_deref(),
            Some(story.as_str())
        );
    });
}

/// The identical fixture through `reconcile_restart_tick` instead:
/// `Interrupted`, not `WindowGone` — proving the TICK reaches the restart
/// pass, not merely that `classify` can produce `Interrupted` in isolation
/// (`tests/engine_restart.rs` already proves that half).
#[test]
fn reconcile_restart_tick_quarantines_the_same_lane_as_interrupted() {
    with_fake_dispatch_script(|| {
        let fixture = ServiceFixture::new();
        let story = new_story(&fixture, "lane work");
        let run_id = started_run(&fixture, 1);
        occupy(&fixture, &run_id, 0, &story);

        reconcile_restart_tick(fixture.store(), fixture.env());

        let lane = lane_at(&fixture, &run_id, 0);
        assert_eq!(lane.state, EngineLaneState::Quarantined);
        assert_eq!(
            lane.outcome.as_deref(),
            Some(HardStopKind::Interrupted.as_str())
        );
    });
}

/// A run whose project has since had its checkout unlinked (or, as here,
/// never resolved one at reconcile time) falls back to `env.home()` rather
/// than erroring the whole run out of the tick — the same fallback
/// `api::engine::EngineController::context` already uses. `start` itself
/// still requires a checkout at creation time; this proves the fallback the
/// TICK needs for a checkout that has since gone missing.
#[test]
fn a_run_with_no_linked_checkout_still_reconciles_via_the_home_fallback() {
    with_fake_dispatch_script(|| {
        let fixture = ServiceFixture::new();
        let story = new_story(&fixture, "lane work");
        let run_id = started_run(&fixture, 1);
        occupy(&fixture, &run_id, 0, &story);
        fixture
            .store()
            .write(|tx| tx.set_checkout_path(fixture.project(), None))
            .unwrap();

        reconcile_restart_tick(fixture.store(), fixture.env());

        let lane = lane_at(&fixture, &run_id, 0);
        assert_eq!(
            lane.state,
            EngineLaneState::Quarantined,
            "the reconcile went ahead despite the missing checkout"
        );
        assert_eq!(
            lane.outcome.as_deref(),
            Some(HardStopKind::Interrupted.as_str())
        );
    });
}

/// `live_engine_runs()` is machine-wide, and a tick must reconcile every
/// live run it names, on whichever project each belongs to — not just the
/// one the caller happened to be thinking about. Two real projects, each
/// with its own live run and its own interrupted lane, both reconciled by
/// one call to `reconcile_restart_tick`, proves the tick actually iterates
/// rather than reconciling a single hard-coded run.
#[test]
fn a_tick_reconciles_every_live_run_across_every_project() {
    with_fake_dispatch_script(|| {
        let fixture = ServiceFixture::new();
        let alpha_story = new_story(&fixture, "alpha lane work");
        let alpha_run = started_run(&fixture, 1);
        occupy(&fixture, &alpha_run, 0, &alpha_story);
        let beta_run = second_project_run(&fixture, "beta", "BE");

        reconcile_restart_tick(fixture.store(), fixture.env());

        let alpha_lane = lane_at(&fixture, &alpha_run, 0);
        assert_eq!(alpha_lane.state, EngineLaneState::Quarantined);
        assert_eq!(
            alpha_lane.outcome.as_deref(),
            Some(HardStopKind::Interrupted.as_str())
        );
        let beta_lane = fixture
            .store()
            .read(|tx| tx.engine_lanes(&beta_run))
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(
            beta_lane.state,
            EngineLaneState::Quarantined,
            "the second project's own run must be reconciled in the same tick, not skipped"
        );
        assert_eq!(
            beta_lane.outcome.as_deref(),
            Some(HardStopKind::Interrupted.as_str())
        );
    });
}
