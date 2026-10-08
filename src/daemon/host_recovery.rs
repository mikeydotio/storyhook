//! One bounded, read-only native proof operation per recovery tick. The pending
//! journal preserves original identity across restart; it grants no authority.
use crate::{
    env::Environment,
    error::AppError,
    process::Cancellation,
    service::{
        Ctx,
        host_recovery::{self, HostRecoveryService},
    },
    store::{ReadOps, Store},
};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

/// A rotating observation cursor prevents one unavailable native subject from
/// starving another project. Durable authority remains entirely in the store.
#[derive(Default)]
pub(super) struct Worker {
    cursor: usize,
}
impl Worker {
    pub(super) fn process_one(
        &mut self,
        store: &impl Store,
        env: &Environment,
        activity: &super::verification::VerificationActivity,
        stop: &AtomicBool,
    ) -> Result<bool, AppError> {
        if stop.load(Ordering::Acquire) {
            return Ok(false);
        }
        let projects = store.read(|tx| tx.projects())?;
        let mut pending = Vec::new();
        for project in projects {
            match store.read(|tx| {
                if !tx.automations_enabled(project.id)? || !tx.verification_enabled(project.id)? {
                    return Ok(Vec::new());
                }
                host_recovery::pending_subjects(tx, project.id)
            }) {
                Ok(subjects) => pending.extend(subjects),
                Err(error) => super::activity::context::project_error(
                    store,
                    project.id,
                    "host-recovery",
                    &error.to_string(),
                ),
            }
        }
        if pending.is_empty() {
            self.cursor = 0;
            return Ok(false);
        }
        self.cursor %= pending.len();
        let subject = &pending[self.cursor];
        self.cursor = (self.cursor + 1) % pending.len();
        let candidate = &subject.candidate;
        let Some(_automation) = crate::service::automations::enter(store, env, candidate.project)?
        else {
            return Ok(false);
        };
        if stop.load(Ordering::Acquire) || activity.active_for(candidate.project).is_some() {
            return Ok(false);
        }
        let ctx =
            Ctx::new(store, candidate.project, &candidate.checkout, env.clone()).no_hooks(true);
        let cancellation = Cancellation::default();
        let deadline = Instant::now() + env.subprocess_bound(Duration::from_secs(30));
        let service = HostRecoveryService::new(&ctx);
        let before = service.list()?;
        let fault = match host_recovery::observe_fault(
            &ctx,
            candidate,
            &subject.attribution,
            &subject.execution,
            &subject.component,
            deadline,
            cancellation.clone(),
        ) {
            Ok(proof) => proof,
            Err(error) => {
                super::activity::context::project_error(
                    store,
                    candidate.project,
                    "host-recovery",
                    &format!(
                        "{} remains held: native fault proof unavailable: {error}",
                        candidate.story_id
                    ),
                );
                return Ok(false);
            }
        };
        if stop.load(Ordering::Acquire) {
            return Ok(false);
        }
        let owner = service.enroll(&fault)?;
        let enrolled = !before.contains(&owner);
        let restored = match host_recovery::observe_restoration(
            &ctx,
            candidate,
            &subject.attribution,
            &subject.execution,
            &subject.component,
            deadline,
            cancellation,
        ) {
            Ok(proof) => proof,
            Err(error) => {
                super::activity::context::project_error(
                    store,
                    candidate.project,
                    "host-recovery",
                    &format!(
                        "{} remains held: native restoration unavailable: {error}",
                        candidate.story_id
                    ),
                );
                return Ok(enrolled);
            }
        };
        if stop.load(Ordering::Acquire) {
            return Ok(enrolled);
        }
        Ok(service
            .restore(&owner.id, owner.revision, &restored)?
            .is_some_and(|after| after != owner)
            || enrolled)
    }
}
