//! The daemon-owned Full Auto engine trigger (SH-466).
//!
//! `EngineService::reconcile`/`reconcile_after_restart` decide what one pass
//! does; nothing here duplicates that decision. This module owns only what
//! wakes a pass, and over how many runs at once — the trigger the reconcile
//! loop was designed against (`docs/spec/full-auto-engine.md`, "The
//! reconcile loop") but that nothing ever wired: SH-465's and SH-468's own
//! As-built notes each said "the daemon wiring is SH-468's," but SH-468's
//! actual approved scope was the HTTP control surface only, so
//! `EngineService::reconcile` had zero production callers before this file.
//!
//! [`reconcile_restart_tick`] runs synchronously before daemon publication;
//! then the ordinary pass runs on project/catalog changes, missed messages,
//! or on a coarse tick derived from [`crate::service::engine::STALL_CEILING_SECS`] — the shape
//! [`crate::daemon::verification::poll_verification`] already uses for its
//! own event-driven worker.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::bus::{Change, ChangeBus, Subscription};
use crate::api::dispatch::resolve_engine_dispatch_script;
use crate::env::Environment;
use crate::error::AppError;
use crate::service::Ctx;
use crate::service::engine::{EngineService, RECONCILE_TICK_SECS, ShellDispatcher};
use crate::store::{EngineRunRecord, ReadOps, Store, StoreError};

/// How often a live run is reconciled in the absence of any other wake.
///
/// Overridable so a test can shrink it — the same shape `heartbeat_interval`,
/// `change_poll_interval` and `github_poll_interval` (`daemon::serve`,
/// `daemon::github_poll`) already use. The production default,
/// [`RECONCILE_TICK_SECS`], is five real minutes; no suite can wait that out.
fn reconcile_tick_interval() -> Duration {
    std::env::var("STORYHOOK_RECONCILE_TICK_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_secs(RECONCILE_TICK_SECS))
}

/// Every currently-live engine run, machine-wide.
///
/// A read failure is reported and treated as "nothing to reconcile this
/// tick" rather than propagated: a poller has no caller to hand an error
/// back to, and the next wake tries again.
fn live_runs<S: Store>(store: &S) -> Vec<EngineRunRecord> {
    match store.read(|tx| tx.live_engine_runs()) {
        Ok(runs) => runs,
        Err(error) => {
            eprintln!("storyhook: could not list live engine runs: {error}");
            Vec::new()
        }
    }
}

/// Resolves one run's `Ctx` and dispatcher — the per-project recipe
/// `api::engine::EngineController::context` already uses (the run's
/// `project_slug` names the project; its linked checkout is the working
/// directory when one exists, `env.home()` otherwise, so a run is never
/// refused just because no checkout was ever linked), plus the real
/// [`ShellDispatcher`] `stop --now` already builds from
/// [`resolve_engine_dispatch_script`]. Never [`crate::service::engine::StoreOnlyDispatcher`]-shaped:
/// a poller that read every window as dead would quarantine every healthy
/// lane on its very first tick.
fn context_for_run<'store, S: Store>(
    store: &'store S,
    env: &Environment,
    run: &EngineRunRecord,
) -> Result<(Ctx<'store, S>, ShellDispatcher), AppError> {
    let (project, checkout) = store.read(|tx| {
        let project = tx.project_by_slug(&run.project_slug)?.ok_or_else(|| {
            StoreError::NotFound(format!("project `{}` not found", run.project_slug))
        })?;
        let checkout = tx.checkout_path(project.id)?;
        Ok((project.id, checkout))
    })?;
    let cwd = checkout.unwrap_or_else(|| env.home().to_path_buf());
    let ctx = Ctx::new(store, project, cwd, env.clone()).no_hooks(true);
    let script = resolve_engine_dispatch_script(run.agent).map_err(AppError::Storage)?;
    let dispatcher = ShellDispatcher::new(script, env.clone());
    Ok((ctx, dispatcher))
}

/// Reconciles one run, isolated from every other run in the same sweep: a
/// dead tmux server or a missing checkout on one project must not stop
/// another project's run from being reconciled (`github_poll::tick`'s own
/// per-project isolation discipline, applied here to per-run isolation).
fn reconcile_one<S: Store>(store: &S, env: &Environment, run: &EngineRunRecord, restart: bool) {
    let (ctx, dispatcher) = match context_for_run(store, env, run) {
        Ok(built) => built,
        Err(error) => {
            eprintln!(
                "storyhook: engine run `{}` (project `{}`) could not be reconciled: {error}",
                run.id, run.project_slug
            );
            return;
        }
    };
    let service = EngineService::new(&ctx, &dispatcher);
    let activity_context = format!("project={} run={}", run.project_slug, run.id);
    super::activity::emit(
        "INFO",
        "engine",
        "event",
        &activity_context,
        if restart {
            "restart reconciliation started"
        } else {
            "reconciliation started"
        },
    );
    let result = if restart {
        service.reconcile_after_restart(&run.id)
    } else {
        service.reconcile(&run.id)
    };
    if let Err(error) = result {
        super::activity::emit(
            "ERROR",
            "engine",
            "event",
            &activity_context,
            &format!("reconciliation failed: {error}"),
        );
        eprintln!(
            "storyhook: engine run `{}` (project `{}`) reconcile failed: {error}",
            run.id, run.project_slug
        );
    } else {
        if let Ok(report) = &result
            && let Some(census) = &report.census
            && let Some((level, message)) = census_journal_edge(census)
        {
            super::activity::emit(level, "engine", "event", &activity_context, &message);
        }
        super::activity::emit(
            "INFO",
            "engine",
            "event",
            &activity_context,
            "reconciliation completed",
        );
    }
}

/// The last unanswered window census, so the outage is journaled on its
/// EDGE and never once per pass (SH-655, the SH-626 rule one probe over).
/// Process-wide rather than a store column: the census is one fact about the
/// machine, not about a lane or a run, and `EngineService` is rebuilt for
/// every sweep — so after a daemon restart the outage is journaled once
/// more, which is the price of a marker that needs no migration.
static CENSUS_EDGE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// The journal line, if any, that this pass's census earns: an ERROR when
/// tmux stops answering or the reason changes, an INFO when it answers
/// again; nothing while the answer is steady in either direction.
fn census_journal_edge(
    census: &crate::lane_budget::WindowCensus,
) -> Option<(&'static str, String)> {
    let mut previous = CENSUS_EDGE.lock().unwrap_or_else(|e| e.into_inner());
    match census {
        crate::lane_budget::WindowCensus::Unanswered { detail } => {
            if previous.as_deref() == Some(detail.as_str()) {
                return None;
            }
            *previous = Some(detail.clone());
            Some((
                "ERROR",
                format!(
                    "window census unanswered; live session count is unknown (run capacity is unchanged): {detail}"
                ),
            ))
        }
        crate::lane_budget::WindowCensus::Counted { .. } => previous.take().map(|_| {
            (
                "INFO",
                "window census answers again; live session counts are available".to_string(),
            )
        }),
    }
}

/// One steady-state pass over every live run, machine-wide.
///
/// Public for store-backed integration tests, the same reason
/// [`crate::daemon::verification::tick_with`] is.
pub fn reconcile_tick<S: Store>(store: &S, env: &Environment) {
    for run in live_runs(store) {
        reconcile_one(store, env, &run, false);
    }
}

/// The daemon-start pass (D11): every occupied lane an outage left behind is
/// classified `Interrupted` rather than misread as `WindowGone`/`Stalled`,
/// and no lane is filled or the run finished — see
/// [`crate::service::engine::ReconcilePass::Restart`].
///
/// Public for store-backed integration tests, for the same reason
/// [`reconcile_tick`] is.
pub fn reconcile_restart_tick<S: Store>(store: &S, env: &Environment) {
    for run in live_runs(store) {
        reconcile_one(store, env, &run, true);
    }
}

/// Runs the steady pass on relevant bus changes or a tick, until shutdown.
///
/// # Why not `poll_verification`'s own wait idiom
///
/// [`crate::daemon::verification::poll_verification`]'s idle arm waits on
/// `subscription.recv(RECOVERY_WAKE)` in a loop that restarts its own budget
/// on every [`Change::Ping`] — fine for its bare 30-second constant, wrong
/// here: a tick of minutes riding a 20-second heartbeat would almost never
/// fire on schedule under that shape. This loop computes one deadline before
/// waiting and re-derives the remaining wait from it on every wake instead —
/// the shape `daemon::serve`'s own chopped-sleep helpers already use — so a
/// run of ignored notices cannot push the tick back. Project and catalog
/// changes wake every live run, including one started since the last pass.
/// The watcher attributes CLI controls from run records, separately from
/// lane observations: an ordinary [`Change::Resync`] must not turn the
/// engine's own observation writes into another pass (SH-642). Overflow is
/// different: the subscriber's dropped counter proves a notification was
/// lost and earns a recovery pass without changing the shared UI feed.
pub(crate) fn poll_engine<S: Store>(
    store: &S,
    env: &Environment,
    bus: &ChangeBus,
    stop: &AtomicBool,
    draining: &AtomicBool,
) {
    let subscription = bus.subscribe();
    let mut observed_drops = 0;
    while !stop.load(Ordering::Relaxed) && !draining.load(Ordering::Relaxed) {
        reconcile_tick(store, env);
        let deadline = Instant::now() + reconcile_tick_interval();
        if !wait_for_reconcile(&subscription, &mut observed_drops, deadline, stop, draining) {
            return;
        }
    }
}

/// True when another pass is due; false when shutdown forbids another pass.
fn wait_for_reconcile(
    subscription: &Subscription,
    observed_drops: &mut u64,
    deadline: Instant,
    stop: &AtomicBool,
    draining: &AtomicBool,
) -> bool {
    loop {
        if stop.load(Ordering::Relaxed) || draining.load(Ordering::Relaxed) {
            return false;
        }
        let dropped = subscription.dropped();
        if dropped != *observed_drops {
            *observed_drops = dropped;
            return true;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return true;
        }
        match subscription.recv(remaining) {
            Some(Change::Project(_) | Change::Catalog) => return true,
            Some(Change::Ping | Change::Resync | Change::Reload) | None => continue,
        }
    }
}

#[cfg(test)]
mod wait_tests;

#[cfg(test)]
mod census_edge_tests {
    use super::census_journal_edge;
    use crate::lane_budget::WindowCensus;

    /// One test for the whole sequence, because the edge is process-wide
    /// state: an outage journals once on entry, once per change of reason,
    /// and once on recovery -- never on a steady pass in either direction.
    #[test]
    fn the_census_outage_is_journaled_on_its_edges_only() {
        let counted = WindowCensus::Counted { windows: vec![] };
        let down = |d: &str| WindowCensus::Unanswered {
            detail: d.to_string(),
        };
        assert_eq!(
            census_journal_edge(&counted),
            None,
            "answering from the start earns nothing"
        );
        let entry = census_journal_edge(&down("no server")).expect("entry is journaled");
        assert_eq!(entry.0, "ERROR");
        assert!(entry.1.contains("no server"), "{}", entry.1);
        assert_eq!(
            census_journal_edge(&down("no server")),
            None,
            "a steady outage is silent"
        );
        let changed =
            census_journal_edge(&down("timed out")).expect("a changed reason is journaled");
        assert!(changed.1.contains("timed out"), "{}", changed.1);
        let recovery = census_journal_edge(&counted).expect("recovery is journaled");
        assert_eq!(recovery.0, "INFO");
        assert_eq!(
            census_journal_edge(&counted),
            None,
            "a steady recovery is silent"
        );
    }
}
