//! Admission and manual controls share the ownership lock. Lock order is
//! registry then store, never store then registry; no subprocess runs locked.

use super::*;
use crate::service::verification_control::{
    VerificationAcknowledgement, VerificationAction, acknowledge_in_transaction,
};
use crate::store::StoreError;

/// Operator-visible lifecycle, derived from durable permission and live ownership.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VerificationControlState {
    /// Admission is enabled; the queue may be empty.
    Running,
    /// Admission is disabled while the owned attempt finishes.
    Draining,
    /// The owned attempt is cancelling and cleaning up.
    Stopping,
    /// Admission is disabled and no attempt remains owned.
    Stopped,
}

fn state(enabled: bool, slot: Option<&VerificationSlot>) -> VerificationControlState {
    match (enabled, slot) {
        (_, Some(slot)) if slot.cancellation.is_cancelled() => VerificationControlState::Stopping,
        (true, _) => VerificationControlState::Running,
        (false, Some(_)) => VerificationControlState::Draining,
        (false, None) => VerificationControlState::Stopped,
    }
}

impl VerificationActivity {
    /// Applies one project-scoped command, committing permission before cancellation.
    pub fn control(
        &self,
        store: &impl Store,
        project: ProjectId,
        action: VerificationAction,
    ) -> Result<VerificationControlState, AppError> {
        self.control_with_receipt(store, project, action, &chrono::Utc::now().to_rfc3339())
            .map(|(state, _)| state)
    }

    /// Returns the exact receipt committed by this control, independent of later writes.
    pub(crate) fn control_with_receipt(
        &self,
        store: &impl Store,
        project: ProjectId,
        action: VerificationAction,
        now: &str,
    ) -> Result<(VerificationControlState, crate::store::VerificationRecovery), AppError> {
        let slots = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        let slot = slots.get(&project);
        let enabled = action == VerificationAction::Start;
        let recovery = store.write(|tx| {
            if enabled && slot.is_some() && !tx.verification_enabled(project)? {
                return Err(AppError::Validation(
                    "the verifier is still stopping; wait for its owned attempt to exit".into(),
                )
                .into());
            }
            tx.put_verification_enabled(project, enabled)?;
            let mut recovery = tx.verification_recovery(project)?;
            if enabled && slot.is_none() {
                recovery.schedule(now);
            } else if !enabled {
                recovery.settle_pending("stopped", "Operator disabled verifier admission");
            }
            tx.put_verification_recovery(project, &recovery)?;
            Ok(recovery)
        })?;
        if action == VerificationAction::Stop
            && let Some(slot) = slot
        {
            slot.cancellation.cancel();
        }
        let result = state(enabled, slot);
        drop(slots);
        self.publish_project(store, project)?;
        Ok((result, recovery))
    }

    /// Acknowledges an exact incident and optionally changes admission atomically.
    pub fn acknowledge(
        &self,
        ctx: &Ctx<'_, impl Store>,
        incident_id: &str,
        action: Option<VerificationAcknowledgement>,
    ) -> Result<VerificationIncident, AppError> {
        self.acknowledge_with_receipt(ctx, incident_id, action)
            .map(|(incident, _)| incident)
    }

    /// Acknowledges atomically and returns that transaction's recovery evidence.
    pub(crate) fn acknowledge_with_receipt(
        &self,
        ctx: &Ctx<'_, impl Store>,
        incident_id: &str,
        action: Option<VerificationAcknowledgement>,
    ) -> Result<(VerificationIncident, crate::store::VerificationRecovery), AppError> {
        let slots = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        let (incident, recovery) = ctx.store().write(|tx| {
            if action == Some(VerificationAcknowledgement::Retry)
                && slots.contains_key(&ctx.project())
            {
                return Err(AppError::Validation(
                    "the verifier still owns an attempt; wait for it to exit before retrying"
                        .into(),
                )
                .into());
            }
            let incident = acknowledge_in_transaction(tx, ctx.project(), incident_id)?;
            if let Some(action) = action {
                tx.put_verification_enabled(
                    ctx.project(),
                    action == VerificationAcknowledgement::Retry,
                )?;
            }
            let enabled = tx.verification_enabled(ctx.project())?;
            let mut recovery = tx.verification_recovery(ctx.project())?;
            recovery.acknowledgement = Some(crate::store::VerificationAcknowledgementRecord {
                incident: incident.clone(),
                at: ctx.now(),
                action: match action {
                    Some(VerificationAcknowledgement::Retry) => {
                        crate::store::VerificationAcknowledgementIntent::Retry
                    }
                    Some(VerificationAcknowledgement::LeaveStopped) => {
                        crate::store::VerificationAcknowledgementIntent::LeaveStopped
                    }
                    None => crate::store::VerificationAcknowledgementIntent::PreserveAdmission,
                },
                enabled,
            });
            if enabled {
                recovery.schedule(&ctx.now());
            } else {
                recovery.settle_pending(
                    "stopped",
                    "Acknowledged with admission disabled; run story verifier start",
                );
            }
            tx.put_verification_recovery(ctx.project(), &recovery)?;
            Ok((incident, recovery))
        })?;
        drop(slots);
        self.publish_project(ctx.store(), ctx.project())?;
        let enabled = recovery
            .acknowledgement
            .as_ref()
            .expect("acknowledgement transaction")
            .enabled;
        self.notify_resumed_with_receipt(ctx, &incident, "acknowledged", enabled, &recovery)?;
        Ok((incident, recovery))
    }

    /// Reads board state while preserving the registry-before-store lock order.
    pub(crate) fn read_project<S: Store, T>(
        &self,
        store: &S,
        project: ProjectId,
        read: impl FnOnce(
            &S::ReadTx<'_>,
            Option<&ActiveVerification>,
            VerificationControlState,
        ) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let slots = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        let slot = slots.get(&project);
        store.read(|tx| {
            read(
                tx,
                slot.map(|slot| &slot.active),
                state(tx.verification_enabled(project)?, slot),
            )
        })
    }

    /// Admissions observe both stop permission and incident halt before publishing ownership.
    pub(super) fn try_acquire(
        &self,
        store: &impl Store,
        candidate: &VerificationCandidate,
        started_at: String,
    ) -> Result<Option<VerificationGuard>, AppError> {
        let mut slots = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        let attempt_id = uuid::Uuid::new_v4().to_string();
        let (allowed, request_id) = store.write(|tx| {
            let allowed = tx.verification_enabled(candidate.project)?
                && !tx
                    .verification_incident(candidate.project)?
                    .is_some_and(|incident| incident.halted);
            let mut request_id = None;
            let mut recovery = tx.verification_recovery(candidate.project)?;
            if allowed
                && let Some(request) = &mut recovery.request
                && request.outcome == crate::store::VerificationRecoveryOutcome::Scheduled
            {
                request_id = Some(request.id.clone());
                request.outcome = crate::store::VerificationRecoveryOutcome::Admitted;
                request.admission = Some(crate::store::VerificationAdmission {
                    attempt_id: attempt_id.clone(),
                    story_id: candidate.story_id.clone(),
                    generation: candidate.verifying_generation,
                    started_at: started_at.clone(),
                });
                tx.put_verification_recovery(candidate.project, &recovery)?;
            }
            Ok((allowed, request_id))
        })?;
        Ok(allowed.then(|| {
            let mut guard = self.acquire_locked(&mut slots, candidate, started_at, attempt_id);
            guard.recovery_request_id = request_id;
            guard
        }))
    }

    /// Publishes after the transaction and registry lock have been released.
    pub(crate) fn publish_project(
        &self,
        store: &impl Store,
        project: ProjectId,
    ) -> Result<(), AppError> {
        let slug = store
            .read(|tx| tx.project(project))?
            .ok_or_else(|| AppError::NotFound(format!("project {project}")))?
            .slug;
        self.bus.publish(Change::Project(slug));
        Ok(())
    }

    /// Publishes recovery with the actual admission permission, including leave-stopped.
    pub(crate) fn notify_resumed(
        &self,
        ctx: &Ctx<'_, impl Store>,
        incident: &VerificationIncident,
        reason: &str,
    ) -> Result<(), AppError> {
        let (enabled, recovery) = ctx.store().read(|tx| {
            Ok((
                tx.verification_enabled(ctx.project())?,
                tx.verification_recovery(ctx.project())?,
            ))
        })?;
        self.notify_resumed_with_receipt(ctx, incident, reason, enabled, &recovery)
    }

    fn notify_resumed_with_receipt(
        &self,
        ctx: &Ctx<'_, impl Store>,
        incident: &VerificationIncident,
        reason: &str,
        enabled: bool,
        recovery: &crate::store::VerificationRecovery,
    ) -> Result<(), AppError> {
        let project = ctx
            .store()
            .read(|tx| tx.project(ctx.project()))?
            .ok_or_else(|| AppError::NotFound(format!("project {}", ctx.project())))?;
        ctx.fire_hook(crate::event_hooks::HookEventType::VerificationResumed, &serde_json::json!({
            "event_type":"verification_resumed", "project":project.slug,
            "incident_id":incident.incident_id, "story_id":incident.story.to_id(&project.prefix),
            "enabled":enabled, "reason":reason, "recovery":recovery,
            "remedy": if enabled { "story verifier status" } else { "story verifier start" },
        }));
        Ok(())
    }

    /// Settles a worker request while retaining its actual attempt identity.
    pub(crate) fn settle_request(
        &self,
        store: &impl Store,
        project: ProjectId,
        request_id: Option<&str>,
        reason: &str,
        detail: &str,
    ) -> Result<(), AppError> {
        let slots = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        if request_id.is_none() || slots.contains_key(&project) {
            return Ok(());
        }
        let pending = store
            .read(|tx| tx.verification_recovery(project))?
            .request
            .is_some_and(|r| {
                Some(r.id.as_str()) == request_id
                    && matches!(
                        r.outcome,
                        crate::store::VerificationRecoveryOutcome::Scheduled
                            | crate::store::VerificationRecoveryOutcome::Admitted
                    )
            });
        if !pending {
            return Ok(());
        }
        let changed = store.write(|tx| {
            let mut recovery = tx.verification_recovery(project)?;
            if let Some(request) = &mut recovery.request
                && Some(request.id.as_str()) == request_id
                && matches!(
                    request.outcome,
                    crate::store::VerificationRecoveryOutcome::Scheduled
                        | crate::store::VerificationRecoveryOutcome::Admitted
                )
                && !slots.contains_key(&project)
            {
                request.outcome = crate::store::VerificationRecoveryOutcome::Settled {
                    reason: reason.into(),
                    detail: detail.into(),
                };
                tx.put_verification_recovery(project, &recovery)?;
                return Ok(true);
            }
            Ok(false)
        })?;
        drop(slots);
        if changed {
            self.publish_project(store, project)?;
        }
        Ok(())
    }

    pub(super) fn cancellation_for(&self, project: ProjectId) -> Cancellation {
        self.active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&project)
            .map(|slot| slot.cancellation.clone())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::NewStoryInput;
    use storyhook_test_support::ServiceFixture;

    fn candidate(
        store: &crate::store::SqliteStore,
        env: &Environment,
        project: ProjectId,
    ) -> VerificationCandidate {
        let ctx = Ctx::new(store, project, env.home().to_path_buf(), env.clone()).no_hooks(true);
        let id = StoryService::new(&ctx)
            .create(&NewStoryInput {
                title: "Reconciliation cancellation".into(),
                ..NewStoryInput::default()
            })
            .unwrap()
            .id;
        StoryService::new(&ctx)
            .set_state(&id, "verifying", None, None, None)
            .unwrap();
        VerificationQueue::new(store).next().unwrap().unwrap()
    }

    #[test]
    fn cancellation_survives_reconciliation_generation_replacement() {
        let fixture = ServiceFixture::new();
        // Unit tests link a second crate instance through test-support. Reopen
        // its seeded database with this crate's types instead of duplicating the seed.
        let store = crate::store::SqliteStore::open(fixture.store().path()).unwrap();
        let project = ProjectId::new(fixture.project().get());
        let env = Environment::at(fixture.cwd());
        let mut candidate = candidate(&store, &env, project);
        let activity = VerificationActivity::new();
        let mut guard = activity.acquire(&candidate, env.now());
        activity
            .control(&store, project, VerificationAction::Stop)
            .unwrap();
        candidate.verifying_generation = Some(GlobalSeq::new(
            candidate.verifying_generation.unwrap().get() + 1,
        ));
        guard.replace(&candidate, env.now());
        assert!(guard.is_cancelled());
        assert!(activity.cancellation_for(project).is_cancelled());
        assert_eq!(
            activity.active_for(project).unwrap().generation,
            candidate.verifying_generation
        );
    }

    #[test]
    fn cancellation_releases_reconciliation_without_requiring_a_store_event() {
        let fixture = ServiceFixture::new();
        // Unit tests link a second crate instance through test-support. Reopen
        // its seeded database with this crate's types instead of duplicating the seed.
        let store = crate::store::SqliteStore::open(fixture.store().path()).unwrap();
        let project = ProjectId::new(fixture.project().get());
        let env = Environment::at(fixture.cwd());
        let candidate = candidate(&store, &env, project);
        let bus = ChangeBus::new();
        let subscription = bus.subscribe();
        let stop = AtomicBool::new(false);
        let token = Cancellation::default();
        let (ready, observed) = std::sync::mpsc::channel();
        let (finished, completion) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                ready.send(()).unwrap();
                let result = wait_for_reconciled_candidate_cancellable(
                    &store,
                    &subscription,
                    &stop,
                    &candidate,
                    &token,
                );
                finished.send(result).unwrap();
            });
            observed.recv_timeout(Duration::from_secs(5)).unwrap();
            token.cancel();
            assert!(
                completion
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .unwrap()
                    .is_none()
            );
        });
        assert_eq!(
            VerificationQueue::new(&store).next().unwrap().unwrap(),
            candidate
        );
    }

    #[test]
    fn concurrent_stop_and_admission_never_leave_an_uncancelled_owner() {
        let fixture = ServiceFixture::new();
        // Unit tests link a second crate instance through test-support. Reopen
        // its seeded database with this crate's types instead of duplicating the seed.
        let store = crate::store::SqliteStore::open(fixture.store().path()).unwrap();
        let project = ProjectId::new(fixture.project().get());
        let env = Environment::at(fixture.cwd());
        let candidate = candidate(&store, &env, project);
        let activity = VerificationActivity::new();
        for _ in 0..20 {
            activity
                .control(&store, project, VerificationAction::Start)
                .unwrap();
            let barrier = std::sync::Barrier::new(2);
            std::thread::scope(|scope| {
                let admission = scope.spawn(|| {
                    barrier.wait();
                    activity.try_acquire(&store, &candidate, env.now()).unwrap()
                });
                let stopping = scope.spawn(|| {
                    barrier.wait();
                    activity
                        .control(&store, project, VerificationAction::Stop)
                        .unwrap()
                });
                let owner = admission.join().unwrap();
                stopping.join().unwrap();
                assert!(owner.as_ref().is_none_or(VerificationGuard::is_cancelled));
                assert!(!store.read(|tx| tx.verification_enabled(project)).unwrap());
            });
        }
    }
    #[test]
    fn stale_tick_cannot_settle_new_intent_or_emit_endless_idle_wakes() {
        let fixture = ServiceFixture::new();
        let store = crate::store::SqliteStore::open(fixture.store().path()).unwrap();
        let project = ProjectId::new(fixture.project().get());
        let bus = ChangeBus::new();
        let subscription = bus.subscribe();
        let activity = VerificationActivity::new().with_bus(bus);
        activity
            .settle_request(&store, project, None, "empty-queue", "idle")
            .unwrap();
        assert_eq!(subscription.recv(Duration::ZERO), None);
        activity
            .control(&store, project, VerificationAction::Start)
            .unwrap();
        let first = store
            .read(|tx| tx.verification_recovery(project))
            .unwrap()
            .request
            .unwrap()
            .id;
        activity
            .control(&store, project, VerificationAction::Start)
            .unwrap();
        let current = store.read(|tx| tx.verification_recovery(project)).unwrap();
        activity
            .settle_request(&store, project, Some(&first), "empty-queue", "old tick")
            .unwrap();
        assert_eq!(
            store.read(|tx| tx.verification_recovery(project)).unwrap(),
            current
        );
        while subscription.recv(Duration::ZERO).is_some() {}
        activity
            .settle_request(
                &store,
                project,
                current.request.as_ref().map(|r| r.id.as_str()),
                "empty-queue",
                "current tick",
            )
            .unwrap();
        assert!(subscription.recv(Duration::ZERO).is_some());
        activity
            .settle_request(
                &store,
                project,
                current.request.as_ref().map(|r| r.id.as_str()),
                "empty-queue",
                "current tick",
            )
            .unwrap();
        assert_eq!(subscription.recv(Duration::ZERO), None);
    }
}

#[cfg(test)]
mod recovery_identity_tests {
    use super::*;
    use storyhook_test_support::ServiceFixture;

    #[test]
    fn admission_captures_new_intent_even_when_workers_snapshot_is_older() {
        let fixture = ServiceFixture::new();
        let store = crate::store::SqliteStore::open(fixture.store().path()).unwrap();
        let project = ProjectId::new(fixture.project().get());
        let env = Environment::at(fixture.cwd());
        let ctx =
            Ctx::new(&store, project, fixture.cwd().to_path_buf(), env.clone()).no_hooks(true);
        let story = StoryService::new(&ctx)
            .create(&crate::service::NewStoryInput {
                title: "Admission identity".into(),
                ..Default::default()
            })
            .unwrap();
        StoryService::new(&ctx)
            .set_state(&story.id, "verifying", None, None, None)
            .unwrap();
        let candidate = VerificationQueue::new(&store).next().unwrap().unwrap();
        let activity = VerificationActivity::new();
        assert!(
            store
                .read(|tx| tx.verification_recovery(project))
                .unwrap()
                .request
                .is_none()
        );
        let (_, receipt) = activity
            .control_with_receipt(&store, project, VerificationAction::Start, &env.now())
            .unwrap();
        let expected = receipt.request.unwrap().id;
        let guard = activity
            .try_acquire(&store, &candidate, env.now())
            .unwrap()
            .unwrap();
        assert_eq!(
            guard.recovery_request_id.as_deref(),
            Some(expected.as_str())
        );
        let observed = store
            .read(|tx| tx.verification_recovery(project))
            .unwrap()
            .request
            .unwrap();
        assert_eq!(
            observed.admission.unwrap().attempt_id,
            guard.active.attempt_id
        );
        drop(guard);
        activity
            .settle_request(
                &store,
                project,
                Some(&expected),
                "completed",
                "completed owned attempt",
            )
            .unwrap();
        assert!(
            matches!(store.read(|tx| tx.verification_recovery(project)).unwrap().request.unwrap().outcome,
            crate::store::VerificationRecoveryOutcome::Settled { reason, .. } if reason == "completed")
        );
    }
}
