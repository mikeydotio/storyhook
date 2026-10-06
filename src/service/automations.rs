//! Project-scoped automation admission and a draining manual-mode boundary.
//!
//! Admission and disabling share a short mutex. No database lock spans external
//! work. Disabling persists first, cancels verifier-owned children, then drains
//! admitted effects before acknowledging. Provider sessions are never stopped.
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};

use super::Ctx;
use crate::env::Environment;
use crate::error::AppError;
use crate::store::{ProjectId, ReadOps, Store, WriteOps};

#[derive(Default)]
struct State {
    active: usize,
    disabling: bool,
}
#[derive(Default)]
struct Boundary {
    state: Mutex<State>,
    drained: Condvar,
    control: Mutex<()>,
}
type Key = (PathBuf, ProjectId);
fn boundary(env: &Environment, project: ProjectId) -> Arc<Boundary> {
    static BOUNDARIES: OnceLock<Mutex<BTreeMap<Key, Arc<Boundary>>>> = OnceLock::new();
    BOUNDARIES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry((env.store_path().to_path_buf(), project))
        .or_default()
        .clone()
}

/// An admitted automatic operation. Hold through all callbacks and external effects.
pub(crate) struct Permit(Arc<Boundary>);
impl Drop for Permit {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.active -= 1;
        self.0.drained.notify_all();
    }
}

/// Admit only while the durable project setting permits automatic work.
pub(crate) fn enter(
    store: &impl Store,
    env: &Environment,
    project: ProjectId,
) -> Result<Option<Permit>, AppError> {
    let boundary = boundary(env, project);
    let mut state = boundary
        .state
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if state.disabling || !store.read(|tx| tx.automations_enabled(project))? {
        return Ok(None);
    }
    state.active += 1;
    drop(state);
    Ok(Some(Permit(boundary)))
}

/// True only for a new submission after the last manual-mode boundary.
pub(crate) fn permits_generation(
    tx: &impl ReadOps,
    project: ProjectId,
    generation: Option<crate::store::GlobalSeq>,
) -> Result<bool, crate::store::StoreError> {
    let settings = tx.settings(project)?;
    Ok(settings.automations_enabled.unwrap_or(true)
        && settings
            .automations_after
            .is_none_or(|after| generation.is_some_and(|g| g.get() > after)))
}

struct ControlReset(Arc<Boundary>);
impl Drop for ControlReset {
    fn drop(&mut self) {
        self.0
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .disabling = false;
    }
}

/// Persist a toggle, serializing repeated requests and draining admitted work.
pub(crate) fn set(ctx: &Ctx<'_, impl Store>, enabled: Option<bool>) -> Result<(), AppError> {
    let boundary = boundary(ctx.env(), ctx.project());
    let _control = boundary
        .control
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let _reset = ControlReset(boundary.clone());
    let effective = enabled.unwrap_or(true);
    let was_enabled = ctx
        .store()
        .read(|tx| tx.automations_enabled(ctx.project()))?;
    {
        let mut state = boundary
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        state.disabling = true;
    }
    // Previously installed merge hooks called ordinary move commands. Upgrade
    // only our existing scripts before acknowledging the manual boundary.
    if !effective
        && let Some(checkout) = ctx.store().read(|tx| tx.checkout_path(ctx.project()))?
        && checkout.is_dir()
    {
        crate::hooks::refresh_existing(&checkout)?;
    }
    let write = ctx.store().write(|tx| {
        let mut settings = tx.settings(ctx.project())?;
        let changed = settings.automations_enabled.unwrap_or(true) != effective;
        settings.automations_enabled = enabled;
        if changed {
            let project = tx
                .project(ctx.project())?
                .ok_or_else(|| crate::store::StoreError::NotFound("project disappeared".into()))?;
            settings.automations_after = Some(project.next_global_seq - 1);
        }
        tx.put_settings(ctx.project(), &settings)
    });
    if let Err(error) = write {
        boundary
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .disabling = false;
        return Err(error.into());
    }
    if !effective || !was_enabled {
        if let Some(activity) = ctx.verification_activity() {
            activity.cancel_automations(ctx.project());
        }
        let mut state = boundary
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        while state.active != 0 {
            state = boundary
                .drained
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
        drop(state);
        // Drain before retiring pending intent: an admitted callback must not
        // resurrect a delivery after this transaction invalidates it.
        ctx.store().write(|tx| {
            for mut record in tx.continuations(ctx.project())? {
                if record.status.outstanding() {
                    let revision = record.revision;
                    record.revision += 1;
                    record.status = crate::store::ContinuationStatus::Superseded;
                    record.detail =
                        "Project automations disabled; explicit new handoff required".into();
                    record.updated_at = ctx.now();
                    tx.update_continuation(&record, revision)?;
                }
            }
            for mut delivery in tx.block_deliveries(ctx.project())? {
                let previous = delivery.status;
                if matches!(
                    previous,
                    crate::store::DeliveryStatus::Pending
                        | crate::store::DeliveryStatus::Attempting
                ) {
                    delivery.status = crate::store::DeliveryStatus::Superseded;
                    delivery.detail = "Project automations disabled".into();
                    tx.update_block_delivery(&delivery, previous)?;
                }
            }
            let project = tx
                .project(ctx.project())?
                .ok_or_else(|| crate::store::StoreError::NotFound("project disappeared".into()))?;
            for mut run in tx.engine_runs(&project.slug)? {
                if run.state.is_live() {
                    run.state = crate::store::EngineRunState::Paused;
                    run.stop_reason =
                        Some("Project automations disabled; resume explicitly".into());
                    tx.update_engine_run(&run)?;
                }
            }
            Ok(())
        })?;
    }
    boundary
        .state
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .disabling = false;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;
    use storyhook_test_support::ServiceFixture;

    #[test]
    fn disable_drains_admitted_callbacks_and_rejects_new_work() {
        let fixture = ServiceFixture::new();
        let store = crate::store::SqliteStore::open(fixture.store().path()).unwrap();
        let ctx = Ctx::new(
            &store,
            ProjectId::new(fixture.project().get()),
            fixture.cwd(),
            Environment::at(fixture.cwd()),
        )
        .no_hooks(true);
        let permit = enter(ctx.store(), ctx.env(), ctx.project())
            .unwrap()
            .unwrap();
        let (finished, done) = mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                set(&ctx, Some(false)).unwrap();
                finished.send(()).unwrap();
            });
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while ctx
                .store()
                .read(|tx| tx.automations_enabled(ctx.project()))
                .unwrap()
            {
                assert!(std::time::Instant::now() < deadline);
                std::thread::yield_now();
            }
            assert!(
                enter(ctx.store(), ctx.env(), ctx.project())
                    .unwrap()
                    .is_none()
            );
            assert!(done.try_recv().is_err());
            drop(permit);
            done.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        set(&ctx, Some(true)).unwrap();
        assert!(
            enter(ctx.store(), ctx.env(), ctx.project())
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn parallel_repeated_controls_leave_a_durable_quiescent_boundary() {
        let fixture = ServiceFixture::new();
        let store = crate::store::SqliteStore::open(fixture.store().path()).unwrap();
        let ctx = Ctx::new(
            &store,
            ProjectId::new(fixture.project().get()),
            fixture.cwd(),
            Environment::at(fixture.cwd()),
        )
        .no_hooks(true);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for _ in 0..5 {
                        set(&ctx, Some(false)).unwrap();
                        set(&ctx, Some(true)).unwrap();
                    }
                });
            }
        });
        set(&ctx, Some(false)).unwrap();
        assert!(
            enter(ctx.store(), ctx.env(), ctx.project())
                .unwrap()
                .is_none()
        );
    }
}
