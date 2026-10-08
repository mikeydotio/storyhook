//! Actual native clean merge plus original central admission and Store CAS.
use super::*;
use crate::daemon::{
    bus::ChangeBus,
    lifecycle::InFlight,
    verification::{TickResult, VerificationActivity, integration_worker},
};
use crate::service::integration_recovery::readmission as native_readmission;

fn clean(f: &OwnedFixture, cancellation: Cancellation) -> NativeCleanIntegration {
    let observation = SubmissionObservation {
        checkout: f.candidate.checkout.clone(),
        repository: "github.com/acme/widgets".into(),
        pull_request: "https://github.com/acme/widgets/pull/7".into(),
        base_branch: "dev".into(),
        // A newly integrated base can already contain this immutable head.
        // The ordinary gate remains mandatory even though native merge is clean.
        base: f.native.head.clone(),
        head: f.native.head.clone(),
    };
    submission::observe_clean_for_fixture(
        &f.native.head,
        Instant::now() + storyhook_test_support::load_grace::graced_now(Duration::from_secs(60)),
        cancellation,
        || Ok(observation.clone()),
    )
    .unwrap()
}

#[test]
fn clean_readmission_preserves_original_generation_and_requires_fresh_original_head_gate() {
    let f = OwnedFixture::new(false);
    let subject = super::pending::settled_subject(&f);
    let ctx = f.ctx();
    let activity = VerificationActivity::new();
    let inflight = InFlight::new(ctx.env().clone());
    let result = integration_worker::start_one_with(
        &f.store,
        ctx.env(),
        &activity,
        &inflight,
        &ChangeBus::new(),
        &subject,
        |service, guard| {
            let active = activity.active_for(f.candidate.project).unwrap();
            let cancellation = guard.cancellation_for_fixture();
            assert!(!guard.is_cancelled());
            let native = clean(&f, cancellation.clone());
            assert!(service.readmit_clean(&subject, &native, &active.attempt_id, &cancellation)?);
            Ok(TickResult::RetryLater)
        },
    )
    .unwrap();
    assert_eq!(result, TickResult::RetryLater);
    let reopened = SqliteStore::open(f.store.path()).unwrap();
    reopened
        .read(|tx| {
            let records = tx.integration_readmissions(f.candidate.project)?;
            assert_eq!(records.len(), 1);
            assert_eq!(
                Some(records[0].generation),
                f.candidate.verifying_generation
            );
            let story = records[0].story;
            assert!(crate::service::project_recovery::requires_certification(
                tx,
                f.candidate.project,
                story
            )?);
            assert_eq!(
                native_readmission::expected_head(
                    tx,
                    f.candidate.project,
                    story,
                    f.candidate.verifying_generation
                )?,
                Some(f.native.head.clone())
            );
            assert!(native_readmission::check_input(tx, &f.candidate, &f.native.head).is_ok());
            assert!(native_readmission::check_input(tx, &f.candidate, &"f".repeat(40)).is_err());
            assert!(!tx.attributions(f.candidate.project)?[0].held);
            assert!(tx.integration_recoveries(f.candidate.project)?.is_empty());
            assert!(tx.landing_intents()?.is_empty());
            assert!(crate::service::verification::submission_is_current(
                tx,
                &tx.story(f.candidate.project, story)?.unwrap(),
                &f.candidate
            )?);
            Ok(())
        })
        .unwrap();
    let refreshed_ctx = Ctx::new(
        &reopened,
        f.candidate.project,
        f.native.root.path(),
        ctx.env().clone(),
    )
    .no_hooks(true);
    StoryService::new(&refreshed_ctx)
        .set_priority(&f.candidate.story_id, "high")
        .unwrap();
    let mut refreshed = VerificationQueue::new(&reopened)
        .with_environment(ctx.env().clone())
        .next()
        .unwrap()
        .unwrap();
    assert_ne!(refreshed.priority, f.candidate.priority);
    refreshed.title = "refreshed diagnostic title".into();
    refreshed.pull_request.as_mut().unwrap().last_checked_at = Some("2099-01-01T00:00:00Z".into());
    reopened
        .read(|tx| native_readmission::check_input(tx, &refreshed, &f.native.head))
        .unwrap();
    let records = reopened
        .read(|tx| tx.integration_readmissions(f.candidate.project))
        .unwrap();
    reopened
        .write(|tx| tx.insert_integration_readmission(&records[0]))
        .unwrap();
    let mut replaced = records[0].clone();
    replaced.evidence["clean"]["submission"]["head"] = serde_json::json!("f".repeat(40));
    assert!(
        reopened
            .write(|tx| tx.insert_integration_readmission(&replaced))
            .is_err()
    );
    reopened
        .write(|tx| tx.put_verification_enabled(f.candidate.project, false))
        .unwrap();
    assert!(
        reopened
            .read(|tx| native_readmission::check_input(tx, &f.candidate, &f.native.head))
            .is_err()
    );
}

#[test]
fn clean_readmission_refuses_foreign_token_and_manual_stop_without_retiring_diagnostic() {
    for revoked in ["foreign-token", "stop"] {
        let f = OwnedFixture::new(false);
        let subject = super::pending::settled_subject(&f);
        let ctx = f.ctx();
        let activity = VerificationActivity::new();
        let inflight = InFlight::new(ctx.env().clone());
        let before = f
            .store
            .read(|tx| tx.attributions(f.candidate.project))
            .unwrap();
        integration_worker::start_one_with(
            &f.store,
            ctx.env(),
            &activity,
            &inflight,
            &ChangeBus::new(),
            &subject,
            |service, guard| {
                let active = activity.active_for(f.candidate.project).unwrap();
                let cancellation = guard.cancellation_for_fixture();
                let proof_token = if revoked == "foreign-token" {
                    Cancellation::default()
                } else {
                    cancellation.clone()
                };
                let native = clean(&f, proof_token);
                if revoked == "stop" {
                    f.store
                        .write(|tx| tx.put_verification_enabled(f.candidate.project, false))?;
                }
                assert!(
                    service
                        .readmit_clean(&subject, &native, &active.attempt_id, &cancellation)
                        .is_err()
                );
                Ok(TickResult::RetryLater)
            },
        )
        .unwrap();
        assert_eq!(
            f.store
                .read(|tx| tx.attributions(f.candidate.project))
                .unwrap(),
            before
        );
        assert!(
            f.store
                .read(|tx| tx.integration_readmissions(f.candidate.project))
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn clean_readmission_preserves_independent_failure_components_and_missing_pending_custody() {
    for held in ["other-attribution", "missing-pending"] {
        let f = OwnedFixture::new(false);
        let subject = super::pending::settled_subject(&f);
        let ctx = f.ctx();
        if held == "other-attribution" {
            f.store
                .write(|tx| {
                    let mut other = tx.attributions(f.candidate.project)?.pop().unwrap();
                    other.id = "other-held-diagnostic".into();
                    other.attempt = "independent-failed-gate".into();
                    let mut gate =
                        GateAttempt::new(other.attempt.clone(), other.submission.clone(), AT);
                    gate.control_revision =
                        Some(tx.verification_control_revision(f.candidate.project)?);
                    tx.insert_gate_attempt(&gate)?;
                    gate.finished_at = Some(AT.into());
                    gate.verdict = Some("failed".into());
                    gate.revision = 1;
                    assert!(tx.update_gate_attempt(&gate, 0)?);
                    other.components[0].observed_cause = FailureCause::Unknown;
                    tx.insert_attribution(&other)
                })
                .unwrap();
        } else {
            rusqlite::Connection::open(f.store.path())
                .unwrap()
                .execute("DELETE FROM integration_pending", [])
                .unwrap();
        }
        let before = f
            .store
            .read(|tx| tx.attributions(f.candidate.project))
            .unwrap();
        let activity = VerificationActivity::new();
        let inflight = InFlight::new(ctx.env().clone());
        integration_worker::start_one_with(
            &f.store,
            ctx.env(),
            &activity,
            &inflight,
            &ChangeBus::new(),
            &subject,
            |service, guard| {
                let active = activity.active_for(f.candidate.project).unwrap();
                let cancellation = guard.cancellation_for_fixture();
                let native = clean(&f, cancellation.clone());
                assert!(
                    service
                        .readmit_clean(&subject, &native, &active.attempt_id, &cancellation)
                        .is_err(),
                    "{held}"
                );
                Ok(TickResult::RetryLater)
            },
        )
        .unwrap();
        assert_eq!(
            f.store
                .read(|tx| tx.attributions(f.candidate.project))
                .unwrap(),
            before
        );
        assert!(
            f.store
                .read(|tx| tx.integration_readmissions(f.candidate.project))
                .unwrap()
                .is_empty()
        );
    }
}
