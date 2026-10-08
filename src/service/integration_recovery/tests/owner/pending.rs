use super::*;

fn retain(f: &OwnedFixture) {
    f.store
        .write(|tx| {
            let attribution = tx.attributions(f.candidate.project)?.pop().unwrap();
            let mut attempt = tx.gate_attempts(f.candidate.project)?.pop().unwrap();
            let previous = attempt.revision;
            let mut execution = crate::store::GateExecution::new(
                "original-native-conflict".into(),
                AT,
                "fixture-conflict.ndjson".into(),
            );
            execution.finished_at = Some(AT.into());
            execution.journal_bound = true;
            execution.verdict = Some("conflict".into());
            execution.inputs = attribution.inputs.clone();
            execution.submissions = vec![attribution.submission.clone()];
            attempt.executions.push(execution);
            attempt.revision += 1;
            assert!(tx.update_gate_attempt(&attempt, previous)?);
            crate::service::integration_recovery::retain_conflict_observation(
                tx,
                &f.candidate,
                &attribution,
                &attempt,
            )
        })
        .unwrap();
}

#[test]
fn pending_integration_restarts_with_original_custody_without_effect_authority() {
    let f = OwnedFixture::new(false);
    retain(&f);
    let before = f
        .store
        .read(|tx| tx.integration_pending(f.candidate.project))
        .unwrap();
    assert_eq!(before.len(), 1);
    assert!(
        f.store
            .read(|tx| tx.integration_recoveries(f.candidate.project))
            .unwrap()
            .is_empty()
    );
    let reopened = SqliteStore::open(f.store.path()).unwrap();
    let subjects = reopened
        .read(|tx| crate::service::integration_recovery::pending_subjects(tx, f.candidate.project))
        .unwrap();
    assert_eq!(subjects.len(), 1);
    assert_eq!(subjects[0].candidate, f.candidate);
    assert_eq!(subjects[0].attribution, "original-attribution");
    let proof = f.proof();
    let record = f.reserve(&proof);
    let ctx = f.ctx();
    let service = IntegrationOwnerService::new(&ctx);
    assert!(
        service.claim_assembly(&record.id, 0, &proof).is_err(),
        "pending observation converted live original admission into assembly authority"
    );
    assert_eq!(
        f.store
            .read(|tx| tx.integration_pending(f.candidate.project))
            .unwrap(),
        before
    );
    proof.settle().unwrap();
}

#[test]
fn pending_integration_refuses_replacement_custody_and_mutating_replay() {
    let f = OwnedFixture::new(false);
    retain(&f);
    let before = f
        .store
        .read(|tx| tx.integration_pending(f.candidate.project))
        .unwrap();
    f.store
        .write(|tx| tx.insert_integration_pending(&before[0]))
        .unwrap();
    let mut replacement = before[0].clone();
    replacement.evidence["candidate"]["checkout"] =
        serde_json::json!(f.native.root.path().join("replacement"));
    assert!(
        f.store
            .write(|tx| tx.insert_integration_pending(&replacement))
            .is_err()
    );
    f.fixture.append_cleanup_lease(&f.candidate.story_id,serde_json::from_value(serde_json::json!({
        "version":1,"project_slug":f.candidate.project_slug,"story_id":f.candidate.story_id,
        "repository_path":f.native.root.path(),"worktree_path":f.native.root.path().join("replacement"),
        "branch":"replacement-work","tmux":{"socket_path":f.native.root.path().join("fixture-socket"),"revivify":null}
    })).unwrap());
    assert!(
        f.store
            .read(|tx| crate::service::integration_recovery::pending_subjects(
                tx,
                f.candidate.project
            ))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        f.store
            .read(|tx| tx.integration_pending(f.candidate.project))
            .unwrap(),
        before
    );
}

#[test]
fn revoked_pending_integration_does_not_hide_an_independent_original_subject() {
    let f = OwnedFixture::new(false);
    retain(&f);
    let ctx = f.ctx();
    let stories = StoryService::new(&ctx);
    let second = stories
        .create(&NewStoryInput {
            title: "second retained conflict".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    PrLinkService::new(&ctx)
        .link(&second, "https://github.com/acme/widgets/pull/8", true)
        .unwrap();
    stories
        .set_state(&second, "verifying", None, None, None)
        .unwrap();
    let candidate = f
        .store
        .read(|tx| crate::service::verification::ordered_candidates_for(tx, f.candidate.project))
        .unwrap()
        .into_iter()
        .find(|c| c.story_id == second)
        .unwrap();
    f.store
        .write(|tx| {
            let mut attribution = tx
                .attributions(f.candidate.project)?
                .into_iter()
                .find(|a| a.id == "original-attribution")
                .unwrap();
            attribution.id = "second-attribution".into();
            attribution.attempt = "second-conflict".into();
            attribution.submission.story_id = second.clone();
            attribution.submission.generation = candidate.verifying_generation;
            attribution.submission.submitted_at = candidate.verifying_since.clone();
            let mut attempt = GateAttempt::new(
                attribution.attempt.clone(),
                attribution.submission.clone(),
                AT,
            );
            attempt.control_revision = Some(tx.verification_control_revision(candidate.project)?);
            tx.insert_gate_attempt(&attempt)?;
            let mut execution = crate::store::GateExecution::new(
                "second-native-conflict".into(),
                AT,
                "second-fixture.ndjson".into(),
            );
            execution.finished_at = Some(AT.into());
            execution.verdict = Some("conflict".into());
            execution.journal_bound = true;
            execution.inputs = attribution.inputs.clone();
            execution.submissions = vec![attribution.submission.clone()];
            attempt.executions.push(execution);
            attempt.revision = 1;
            assert!(tx.update_gate_attempt(&attempt, 0)?);
            tx.insert_attribution(&attribution)?;
            crate::service::integration_recovery::retain_conflict_observation(
                tx,
                &candidate,
                &attribution,
                &attempt,
            )
        })
        .unwrap();
    assert_eq!(
        f.store
            .read(|tx| crate::service::integration_recovery::pending_subjects(
                tx,
                f.candidate.project
            ))
            .unwrap()
            .len(),
        2
    );
    stories
        .set_state(&f.candidate.story_id, "in-progress", None, None, None)
        .unwrap();
    let remaining = f
        .store
        .read(|tx| crate::service::integration_recovery::pending_subjects(tx, f.candidate.project))
        .unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].candidate, candidate);
    assert_eq!(remaining[0].attribution, "second-attribution");
    assert_eq!(
        f.store
            .read(|tx| tx.integration_pending(f.candidate.project))
            .unwrap()
            .len(),
        2,
        "revocation discarded original custody"
    );
}

#[test]
fn incomplete_physical_conflict_custody_keeps_hold_without_automatic_enrollment() {
    for missing in ["base", "head", "submission", "journal", "unfinished"] {
        let f = OwnedFixture::new(false);
        f.store
            .write(|tx| {
                let mut attribution = tx.attributions(f.candidate.project)?.pop().unwrap();
                let mut attempt = tx.gate_attempts(f.candidate.project)?.pop().unwrap();
                if missing == "base" {
                    attribution.inputs.base = None;
                }
                if missing == "head" {
                    attribution.inputs.head = None;
                }
                let mut execution = crate::store::GateExecution::new(
                    "physical-conflict".into(),
                    AT,
                    "fixture.ndjson".into(),
                );
                execution.finished_at = (missing != "unfinished").then(|| AT.into());
                execution.verdict = Some("conflict".into());
                execution.journal_bound = missing != "journal";
                execution.inputs = attribution.inputs.clone();
                if missing != "submission" {
                    execution.submissions = vec![attribution.submission.clone()];
                }
                attempt.executions.push(execution);
                crate::service::integration_recovery::retain_conflict_observation(
                    tx,
                    &f.candidate,
                    &attribution,
                    &attempt,
                )
            })
            .unwrap();
        assert!(
            f.store
                .read(|tx| tx.integration_pending(f.candidate.project))
                .unwrap()
                .is_empty(),
            "accepted missing {missing}"
        );
        assert!(
            f.store
                .read(|tx| tx.attributions(f.candidate.project))
                .unwrap()[0]
                .held
        );
    }
}

fn settled_subject(f: &OwnedFixture) -> crate::service::integration_recovery::PendingIntegration {
    retain(f);
    f.store
        .write(|tx| {
            let mut original = tx
                .gate_attempts(f.candidate.project)?
                .into_iter()
                .find(|a| a.id == "original-conflict")
                .unwrap();
            let previous = original.revision;
            original.finished_at = Some(AT.into());
            original.verdict = Some("conflict".into());
            original.revision += 1;
            assert!(tx.update_gate_attempt(&original, previous)?);
            Ok(())
        })
        .unwrap();
    f.store
        .read(|tx| crate::service::integration_recovery::pending_subjects(tx, f.candidate.project))
        .unwrap()
        .pop()
        .unwrap()
}

#[test]
fn managed_new_worker_owns_original_generation_and_excludes_a_second_central_owner() {
    use crate::daemon::{
        bus::ChangeBus,
        lifecycle::InFlight,
        verification::{TickResult, VerificationActivity, integration_worker},
    };
    let f = OwnedFixture::new(false);
    let subject = settled_subject(&f);
    assert_eq!(subject.retained_head, f.native.head);
    let ctx = f.ctx();
    let activity = VerificationActivity::new();
    let inflight = InFlight::new(ctx.env().clone());
    let bus = ChangeBus::new();
    let result = integration_worker::start_one_with(
        &f.store,
        ctx.env(),
        &activity,
        &inflight,
        &bus,
        &subject,
        |_service, guard| {
            let active = activity.active_for(f.candidate.project).unwrap();
            assert_eq!(active.story_id, f.candidate.story_id);
            assert_eq!(active.generation, f.candidate.verifying_generation);
            assert_eq!(active.mode, crate::domain::landing::VerificationMode::Gated);
            assert!(!guard.is_cancelled());
            let admission = f
                .store
                .read(|tx| tx.gate_attempts(f.candidate.project))
                .unwrap()
                .into_iter()
                .find(|a| a.id == active.attempt_id)
                .unwrap();
            assert!(admission.finished_at.is_none());
            assert!(
                admission.executions.is_empty(),
                "admission fabricated a physical gate"
            );
            let second = integration_worker::start_one_with(
                &f.store,
                ctx.env(),
                &activity,
                &inflight,
                &bus,
                &subject,
                |_, _| panic!("second worker acquired the same central slot"),
            )?;
            assert_eq!(second, TickResult::Stopped);
            assert_eq!(activity.active_for(f.candidate.project).unwrap(), active);
            Ok(TickResult::RetryLater)
        },
    )
    .unwrap();
    assert_eq!(result, TickResult::RetryLater);
    assert!(activity.active_for(f.candidate.project).is_none());
    assert!(
        f.store
            .read(|tx| tx.integration_recoveries(f.candidate.project))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn managed_new_worker_refuses_operator_stop_and_stale_cleanup_before_native_effects() {
    use crate::daemon::{
        bus::ChangeBus,
        lifecycle::InFlight,
        verification::{TickResult, VerificationActivity, integration_worker},
    };
    for revoked in ["operator-stop", "cleanup"] {
        let f = OwnedFixture::new(false);
        let subject = settled_subject(&f);
        let ctx = f.ctx();
        let before = f
            .store
            .read(|tx| tx.gate_attempts(f.candidate.project))
            .unwrap();
        if revoked == "operator-stop" {
            f.store
                .write(|tx| tx.put_verification_enabled(f.candidate.project, false))
                .unwrap();
        } else {
            f.fixture.append_cleanup_lease(&f.candidate.story_id, serde_json::from_value(serde_json::json!({
                "version":1,"project_slug":f.candidate.project_slug,"story_id":f.candidate.story_id,
                "repository_path":f.native.root.path(),"worktree_path":f.native.root.path().join("replacement"),
                "branch":"replacement-work","tmux":{"socket_path":f.native.root.path().join("fixture-socket"),"revivify":null}
            })).unwrap());
        }
        let activity = VerificationActivity::new();
        let inflight = InFlight::new(ctx.env().clone());
        let result = integration_worker::start_one_with(
            &f.store,
            ctx.env(),
            &activity,
            &inflight,
            &ChangeBus::new(),
            &subject,
            |_, _| panic!("revoked {revoked} reached native effects"),
        )
        .unwrap();
        assert_eq!(result, TickResult::Stopped, "{revoked}");
        assert_eq!(
            f.store
                .read(|tx| tx.gate_attempts(f.candidate.project))
                .unwrap(),
            before
        );
        assert!(activity.active_for(f.candidate.project).is_none());
    }
}

#[test]
fn managed_worker_failure_diagnostic_preserves_effect_epoch_and_prevents_replay() {
    let f = OwnedFixture::new(false);
    let _subject = settled_subject(&f);
    let proof = f.proof();
    let ctx = f.ctx();
    let service = IntegrationOwnerService::new(&ctx);
    let record = f.reserve(&proof);
    let claim = service
        .claim_assembly(&record.id, record.revision, &proof)
        .unwrap()
        .unwrap();
    let before = service.show(&record.id).unwrap();
    service
        .note_hold(
            &record.id,
            "publication request outcome unknown; original workspace retained",
        )
        .unwrap();
    let after = service.show(&record.id).unwrap();
    assert!(after.0.active);
    assert_eq!(after.1.phase, before.1.phase);
    assert_eq!(after.1.effect_epoch, before.1.effect_epoch);
    assert_eq!(after.1.workspace, before.1.workspace);
    assert!(after.1.hold.as_deref().unwrap().contains("outcome unknown"));
    assert!(!service.assembly_permitted(&claim, &proof).unwrap());
    assert!(
        service
            .claim_assembly(&record.id, after.0.revision, &proof)
            .unwrap()
            .is_none()
    );
    let reopened = SqliteStore::open(f.store.path()).unwrap();
    let ctx2 = Ctx::new(
        &reopened,
        f.candidate.project,
        f.native.root.path(),
        ctx.env().clone(),
    )
    .no_hooks(true);
    assert_eq!(
        IntegrationOwnerService::new(&ctx2)
            .show(&record.id)
            .unwrap(),
        after
    );
    assert!(
        reopened
            .read(|tx| crate::service::integration_recovery::pending_subjects(
                tx,
                f.candidate.project
            ))
            .unwrap()
            .is_empty()
    );
    proof.settle().unwrap();
}
