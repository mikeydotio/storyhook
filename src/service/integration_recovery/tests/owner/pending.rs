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
