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
//! One restart sweep runs once, before any run resumes claiming (D11), then
//! the ordinary pass runs on every bus change or on a coarse tick derived
//! from [`crate::service::engine::STALL_CEILING_SECS`] — the shape
//! [`crate::daemon::verification::poll_verification`] already uses for its
//! own event-driven worker.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::bus::{Change, ChangeBus};
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
/// [`RECONCILE_TICK_SECS`], is 72 real seconds; no suite can wait that out.
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
    let result = if restart {
        service.reconcile_after_restart(&run.id)
    } else {
        service.reconcile(&run.id)
    };
    if let Err(error) = result {
        eprintln!(
            "storyhook: engine run `{}` (project `{}`) reconcile failed: {error}",
            run.id, run.project_slug
        );
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

/// Runs the restart sweep once, then the steady pass on every bus wake or
/// tick, until daemon shutdown.
///
/// # Why not `poll_verification`'s own wait idiom
///
/// [`crate::daemon::verification::poll_verification`]'s idle arm waits on
/// `subscription.recv(RECOVERY_WAKE)` in a loop that restarts its own budget
/// on every [`Change::Ping`] — fine for its bare 30-second constant, wrong
/// here: a 72-second tick riding a 20-second heartbeat would almost never
/// fire on schedule under that shape. This loop computes one deadline before
/// waiting and re-derives the remaining wait from it on every wake instead —
/// the shape `daemon::serve`'s own chopped-sleep helpers already use — so a
/// run of pings cannot push the tick back. Any non-`Ping` change (a
/// `story move`, or a control command through `api::engine::EngineController`,
/// which already publishes [`Change::Project`] on every mutation) still
/// breaks the wait immediately.
pub(crate) fn poll_engine<S: Store>(
    store: &S,
    env: &Environment,
    bus: &ChangeBus,
    stop: &AtomicBool,
) {
    let subscription = bus.subscribe();
    // Before any run resumes claiming (D11) — on this same thread, ahead of
    // the loop below, so there is no second thread that could race it over
    // the same lane rows.
    reconcile_restart_tick(store, env);
    while !stop.load(Ordering::Relaxed) {
        reconcile_tick(store, env);
        let deadline = Instant::now() + reconcile_tick_interval();
        loop {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match subscription.recv(remaining) {
                Some(Change::Ping) | None => continue,
                Some(_) => break,
            }
        }
    }
}
