//! Real native evidence enters the production owner, decision and readmission doors.
use super::*;
use crate::service::{PrLinkService, project_recovery::*};
use crate::store::StoryNo;
mod joins;

fn fixture(mixed: bool) -> (Fixture, Evidence, SharedRecoveryEvidence) {
    let mut f = Fixture::new(false);
    // The base already fails with 41. An equivalent Rust expression preserves
    // that exact failure while staying inside the native source intervention.
    f.base = f.git(&["rev-parse", "HEAD"]);
    f.write("src/lib.rs", "pub fn answer() -> u32 { 40 + 1 }\n");
    f.git(&["add", "src/lib.rs"]);
    f.git(&["commit", "-qm", "equivalent failing candidate"]);
    let mut evidence = Evidence::new();
    evidence
        .fixture
        .github_checkout("https://github.com/acme/widgets");
    {
        let ctx = context(&evidence);
        PrLinkService::new(&ctx)
            .link(
                &evidence.candidate.story_id,
                "https://github.com/acme/widgets/pull/1",
                true,
            )
            .unwrap();
    }
    evidence.candidate = VerificationQueue::new(&evidence.store)
        .with_environment(Environment::at(evidence.fixture.cwd()).with_subprocess_patience())
        .next()
        .unwrap()
        .unwrap();
    let (settled, record) = settled(&evidence, &f, mixed);
    let proof = evidence
        .store
        .read(|tx| settled.prove_shared(tx, &evidence.candidate, &record.id, "original"))
        .unwrap();
    (f, evidence, proof)
}

fn context(evidence: &Evidence) -> Ctx<'_, SqliteStore> {
    Ctx::new(
        &evidence.store,
        evidence.candidate.project,
        evidence.fixture.cwd(),
        Environment::at(evidence.fixture.cwd()).with_subprocess_patience(),
    )
    .no_hooks(true)
}

fn decision(view: &RecoveryView, scope: RepairScope) -> DecisionInput {
    DecisionInput {
        join_recovery: None,
        version: 1,
        revision: view.record.revision,
        project: view.record.project,
        generation: view.state.assessment.generation,
        dispatch_identity: view.state.assessment.dispatch_identity.clone(),
        scope,
        context: "Both native pinned inputs reproduce the same check".into(),
        question: "Who owns the shared failure?".into(),
        decision: "Use the explicitly selected recovery scope".into(),
        rationale: "The equivalent submitted expression did not introduce the failing assertion"
            .into(),
        evidence: vec![format!("attempt:{}", view.observations[0].attempt_id)],
        repair: (scope == RepairScope::SeparateStory).then(|| RepairSpec {
            title: "Repair the shared assertion".into(),
            description: "Correct the pinned base failure without changing the retained submission"
                .into(),
            acceptance: "The original detector passes through central verification".into(),
        }),
        prerequisite: (scope == RepairScope::External)
            .then(|| "Restore the explicitly assessed external fixture".into()),
    }
}

fn satisfy(service: &ProjectRecoveryService<'_, SqliteStore>, view: &RecoveryView) -> RecoveryView {
    service
        .satisfy(
            &view.record.id,
            &PrerequisiteInput {
                version: 1,
                revision: view.record.revision,
                context: "The operator checked the external fixture".into(),
                question: "Is it restored?".into(),
                decision: "The prerequisite is restored".into(),
                rationale: "A new fixture receipt confirms restoration".into(),
                evidence: vec!["operator fixture receipt".into()],
            },
        )
        .unwrap()
}

#[test]
fn shared_recovery_readmits_original_generation_once_after_restart_and_release() {
    let (_f, evidence, proof) = fixture(false);
    let ctx = context(&evidence);
    let service = ProjectRecoveryService::new(&ctx);
    let opened = service
        .observe_shared(&evidence.candidate, &proof)
        .unwrap()
        .unwrap();
    assert_eq!(
        service.observe_shared(&evidence.candidate, &proof).unwrap(),
        Some(opened.clone())
    );
    assert_eq!(opened.state.subjects.len(), 1);
    assert!(!opened.state.subjects[0].returned);
    let before = evidence
        .store
        .read(|tx| tx.gate_attempts(evidence.candidate.project))
        .unwrap();
    assert!(
        VerificationQueue::new(&evidence.store)
            .with_environment(Environment::at(evidence.fixture.cwd()).with_subprocess_patience())
            .next()
            .unwrap()
            .is_none()
    );

    // A separate store handle reconstructs only durable state, never a live proof.
    let reopened = SqliteStore::open(evidence.store.path()).unwrap();
    let restarted_ctx = Ctx::new(
        &reopened,
        evidence.candidate.project,
        evidence.fixture.cwd(),
        Environment::at(evidence.fixture.cwd()).with_subprocess_patience(),
    )
    .no_hooks(true);
    let restarted = ProjectRecoveryService::new(&restarted_ctx);
    let claimed = restarted
        .claim_assessment(&opened.record.id)
        .unwrap()
        .unwrap();
    assert!(
        restarted
            .claim_assessment(&opened.record.id)
            .unwrap()
            .is_none(),
        "duplicate delivery was claimed"
    );
    let input = decision(&claimed, RepairScope::External);
    let decided = restarted.decide(&opened.record.id, &input).unwrap();
    assert_eq!(
        restarted.decide(&opened.record.id, &input).unwrap(),
        decided
    );
    let released = satisfy(&restarted, &decided);
    assert!(!released.record.active);
    assert!(
        released
            .state
            .shared
            .as_ref()
            .unwrap()
            .readmissions
            .is_empty()
    );

    // Raw custody survives restart: modifying retained output cannot release the hold.
    let raw = _f.directory.path().join("probe-0/run.stdout");
    let original = fs::read(&raw).unwrap();
    fs::write(&raw, "changed after enrollment").unwrap();
    assert!(!restarted.landing_release_ready(&opened.record.id).unwrap());
    assert_eq!(
        restarted.reconcile_landing(&opened.record.id).unwrap(),
        released
    );
    fs::write(&raw, original).unwrap();

    reopened
        .write(|tx| tx.put_verification_enabled(evidence.candidate.project, false))
        .unwrap();
    assert!(!restarted.landing_release_ready(&opened.record.id).unwrap());
    assert!(
        restarted
            .reconcile_landing(&opened.record.id)
            .unwrap()
            .state
            .shared
            .unwrap()
            .readmissions
            .is_empty()
    );
    reopened
        .write(|tx| tx.put_verification_enabled(evidence.candidate.project, true))
        .unwrap();
    assert!(restarted.landing_release_ready(&opened.record.id).unwrap());
    let readmitted = restarted.reconcile_landing(&opened.record.id).unwrap();
    let receipts = &readmitted.state.shared.as_ref().unwrap().readmissions;
    assert_eq!(receipts.len(), 1);
    assert_eq!(
        receipts[0].generation,
        evidence.candidate.verifying_generation.unwrap()
    );
    assert_eq!(
        restarted.reconcile_landing(&opened.record.id).unwrap(),
        readmitted
    );
    let ready = VerificationQueue::new(&reopened)
        .with_environment(Environment::at(evidence.fixture.cwd()).with_subprocess_patience())
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(ready.story_id, evidence.candidate.story_id);
    assert_eq!(
        ready.verifying_generation, evidence.candidate.verifying_generation,
        "readmission invented an author submission"
    );
    let story = StoryNo::parse_id("SH", &ready.story_id).unwrap();
    assert!(
        reopened
            .read(
                |tx| crate::service::project_recovery::requires_certification(
                    tx,
                    ready.project,
                    story
                )
            )
            .unwrap()
    );
    let original_head = proof.record().inputs.head.clone().unwrap();
    let prepared = RepairInput {
        head: original_head.clone(),
        base: "e".repeat(40),
        head_tree: "c".repeat(40),
        tree: "d".repeat(40),
    };
    assert!(
        matches!(
            restarted
                .admit_repair(&ready, "fresh-readmission", &prepared)
                .unwrap(),
            RepairAdmission::Proceed { recovery_id: None }
        ),
        "fresh base movement should be allowed"
    );
    let mut changed_head = prepared.clone();
    changed_head.head = "f".repeat(40);
    let refused = restarted
        .admit_repair(&ready, "changed-retained-head", &changed_head)
        .unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("retained submission head changed"),
        "{refused}"
    );
    reopened
        .write(|tx| tx.put_verification_enabled(ready.project, false))
        .unwrap();
    assert!(
        VerificationQueue::new(&reopened)
            .with_environment(Environment::at(evidence.fixture.cwd()).with_subprocess_patience())
            .next()
            .unwrap()
            .is_none(),
        "readmission entered skipped verification after stop"
    );
    reopened
        .write(|tx| tx.put_verification_enabled(ready.project, true))
        .unwrap();

    let row = reopened
        .read(|tx| tx.story(ready.project, story))
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "verifying");
    assert!(row.awaiting.is_none());
    assert_eq!(
        reopened.read(|tx| tx.gate_attempts(ready.project)).unwrap(),
        before,
        "release fabricated gate or certification evidence"
    );
}

#[test]
fn shared_recovery_keeps_one_managed_repair_and_never_releases_for_manual_done() {
    let (_f, evidence, proof) = fixture(false);
    let ctx = context(&evidence);
    let service = ProjectRecoveryService::new(&ctx);
    let opened = service
        .observe_shared(&evidence.candidate, &proof)
        .unwrap()
        .unwrap();
    let claimed = service
        .claim_assessment(&opened.record.id)
        .unwrap()
        .unwrap();
    assert!(
        service
            .decide(
                &opened.record.id,
                &decision(&claimed, RepairScope::SameStory)
            )
            .is_err()
    );
    let input = decision(&claimed, RepairScope::SeparateStory);
    let decided = service.decide(&opened.record.id, &input).unwrap();
    assert_eq!(service.decide(&opened.record.id, &input).unwrap(), decided);
    assert_eq!(decided.state.work.len(), 1);
    let repair = decided
        .state
        .decision
        .as_ref()
        .unwrap()
        .repair_story
        .unwrap()
        .to_id("SH");
    let ordinary = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "ordinary paused work".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    StoryService::new(&ctx)
        .set_state(&ordinary, "verifying", None, None, None)
        .unwrap();
    PrLinkService::new(&ctx)
        .link(&repair, "https://github.com/acme/widgets/pull/2", true)
        .unwrap();
    StoryService::new(&ctx)
        .set_state(&repair, "verifying", None, None, None)
        .unwrap();
    let queue = VerificationQueue::new(&evidence.store)
        .with_environment(Environment::at(evidence.fixture.cwd()).with_subprocess_patience())
        .ordered_for(ctx.project())
        .unwrap();
    assert_eq!(
        queue.len(),
        1,
        "project pause did not isolate managed repair"
    );
    assert_eq!(queue[0].story_id, repair);
    let pinned = RepairInput {
        base: "a".repeat(40),
        head: "b".repeat(40),
        head_tree: "c".repeat(40),
        tree: "d".repeat(40),
    };
    assert!(
        matches!(service.admit_repair(&queue[0], "shared-repair-attempt", &pinned).unwrap(), RepairAdmission::Proceed { recovery_id: Some(id) } if id == opened.record.id)
    );
    assert!(matches!(
        service
            .admit_repair(&queue[0], "shared-repair-attempt", &pinned)
            .unwrap(),
        RepairAdmission::Proceed {
            recovery_id: Some(_)
        }
    ));
    assert_eq!(
        service
            .show(&opened.record.id)
            .unwrap()
            .state
            .attempts
            .len(),
        1
    );
    service
        .complete_repair(
            &queue[0],
            "shared-repair-attempt",
            &RepairJudgment::TestsFailed {
                tree: pinned.tree.clone(),
            },
        )
        .unwrap();
    for index in 2..=3 {
        let input = RepairInput {
            head: format!("{index:040x}"),
            head_tree: format!("{:040x}", index + 10),
            tree: format!("{:040x}", index + 20),
            base: pinned.base.clone(),
        };
        let attempt = format!("changed-repair-{index}");
        assert!(matches!(
            service.admit_repair(&queue[0], &attempt, &input).unwrap(),
            RepairAdmission::Proceed { .. }
        ));
        service
            .complete_repair(
                &queue[0],
                &attempt,
                &RepairJudgment::TestsFailed { tree: input.tree },
            )
            .unwrap();
    }
    let exhausted = RepairInput {
        head: "e".repeat(40),
        head_tree: "f".repeat(40),
        ..pinned.clone()
    };
    assert!(matches!(
        service
            .admit_repair(&queue[0], "fourth-repair", &exhausted)
            .unwrap(),
        RepairAdmission::Deferred {
            reason: RepairRefusal::BudgetExhausted,
            ..
        }
    ));
    StoryService::new(&ctx)
        .set_state(
            &repair,
            "done",
            Some("operator accepts uncertified fixture closure"),
            None,
            None,
        )
        .unwrap();
    assert!(service.show(&opened.record.id).unwrap().record.active);
    assert!(
        !service.landing_release_ready(&opened.record.id).unwrap(),
        "manual Done fabricated certified landing"
    );
    assert!(
        service
            .reconcile_landing(&opened.record.id)
            .unwrap()
            .state
            .shared
            .unwrap()
            .readmissions
            .is_empty()
    );
    assert!(
        VerificationQueue::new(&evidence.store)
            .with_environment(Environment::at(evidence.fixture.cwd()).with_subprocess_patience())
            .ordered_for(ctx.project())
            .unwrap()
            .is_empty()
    );

    let other = evidence.fixture.add_project("unrelated", "OT");
    let other_ctx = Ctx::new(
        &evidence.store,
        ProjectId::new(other.get()),
        evidence.fixture.cwd(),
        Environment::at(evidence.fixture.cwd()).with_subprocess_patience(),
    )
    .no_hooks(true);
    let other_id = StoryService::new(&other_ctx)
        .create(&NewStoryInput {
            title: "unrelated safe project".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    StoryService::new(&other_ctx)
        .set_state(&other_id, "verifying", None, None, None)
        .unwrap();
    assert_eq!(
        VerificationQueue::new(&evidence.store)
            .with_environment(Environment::at(evidence.fixture.cwd()).with_subprocess_patience())
            .ordered_for(other_ctx.project())
            .unwrap()
            .len(),
        1,
        "project-local proof paused another project"
    );
}

#[test]
fn shared_release_preserves_mixed_components_and_replacement_manual_holds() {
    for mixed in [false, true] {
        let (_f, evidence, proof) = fixture(mixed);
        let ctx = context(&evidence);
        let service = ProjectRecoveryService::new(&ctx);
        let opened = service
            .observe_shared(&evidence.candidate, &proof)
            .unwrap()
            .unwrap();
        let claimed = service
            .claim_assessment(&opened.record.id)
            .unwrap()
            .unwrap();
        let decided = service
            .decide(
                &opened.record.id,
                &decision(&claimed, RepairScope::External),
            )
            .unwrap();
        let released = satisfy(&service, &decided);
        if !mixed {
            // Identical text is still a different awaiting episode and cannot be cleared.
            let text = decided.state.decision.as_ref().unwrap().dependency_holds[0]
                .awaiting
                .clone();
            StoryService::new(&ctx)
                .clear_awaiting(&evidence.candidate.story_id)
                .unwrap();
            StoryService::new(&ctx)
                .set_awaiting(&evidence.candidate.story_id, &text)
                .unwrap();
        }
        let before = evidence
            .store
            .read(|tx| tx.attributions(ctx.project()))
            .unwrap();
        assert!(!service.landing_release_ready(&released.record.id).unwrap());
        assert!(
            service
                .reconcile_landing(&released.record.id)
                .unwrap()
                .state
                .shared
                .unwrap()
                .readmissions
                .is_empty()
        );
        assert_eq!(
            evidence
                .store
                .read(|tx| tx.attributions(ctx.project()))
                .unwrap(),
            before
        );
        assert!(
            VerificationQueue::new(&evidence.store)
                .with_environment(
                    Environment::at(evidence.fixture.cwd()).with_subprocess_patience()
                )
                .next()
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn matching_shared_submissions_join_one_owner_across_different_candidate_heads() {
    let (mut f, evidence, first) = fixture(false);
    let ctx = context(&evidence);
    let service = ProjectRecoveryService::new(&ctx);
    let stories = StoryService::new(&ctx);
    let second_id = stories
        .create(&NewStoryInput {
            title: "second retained submission".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    PrLinkService::new(&ctx)
        .link(&second_id, "https://github.com/acme/widgets/pull/2", true)
        .unwrap();
    stories
        .set_state(&second_id, "verifying", None, None, None)
        .unwrap();
    let second = VerificationQueue::new(&evidence.store)
        .with_environment(Environment::at(evidence.fixture.cwd()).with_subprocess_patience())
        .ordered_for(ctx.project())
        .unwrap()
        .into_iter()
        .find(|c| c.story_id == second_id)
        .unwrap();
    let third_id = stories
        .create(&NewStoryInput {
            title: "different pinned evidence".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    PrLinkService::new(&ctx)
        .link(&third_id, "https://github.com/acme/widgets/pull/3", true)
        .unwrap();
    stories
        .set_state(&third_id, "verifying", None, None, None)
        .unwrap();
    let third = VerificationQueue::new(&evidence.store)
        .with_environment(Environment::at(evidence.fixture.cwd()).with_subprocess_patience())
        .ordered_for(ctx.project())
        .unwrap()
        .into_iter()
        .find(|c| c.story_id == third_id)
        .unwrap();
    let opened = service
        .observe_shared(&evidence.candidate, &first)
        .unwrap()
        .unwrap();
    // Same failing pinned base and detector, but another submitted commit and story.
    f.write("src/lib.rs", "pub fn answer() -> u32 { 39 + 2 }\n");
    f.git(&["add", "src/lib.rs"]);
    f.git(&["commit", "-qm", "second candidate"]);
    let (settled, record) = settled_named(
        &evidence,
        &f,
        false,
        &second,
        "second-attempt",
        "second-attribution",
    );
    let proof = evidence
        .store
        .read(|tx| settled.prove_shared(tx, &second, &record.id, "original"))
        .unwrap();
    let joined = service.observe_shared(&second, &proof).unwrap().unwrap();
    assert_eq!(joined.record.id, opened.record.id);
    assert_eq!(
        joined.state.assessment.dispatch_identity,
        opened.state.assessment.dispatch_identity
    );
    assert_eq!(joined.state.subjects.len(), 2);
    assert_ne!(
        joined.observations[0].evidence["attribution"]["inputs"]["head"],
        joined.observations[1].evidence["attribution"]["inputs"]["head"]
    );
    assert_eq!(
        service.observe_shared(&second, &proof).unwrap(),
        Some(joined.clone())
    );
    assert_eq!(
        evidence
            .store
            .read(|tx| tx.project_recoveries(ctx.project()))
            .unwrap()
            .len(),
        1
    );
    let claimed = service
        .claim_assessment(&opened.record.id)
        .unwrap()
        .unwrap();
    let decided = service
        .decide(
            &opened.record.id,
            &decision(&claimed, RepairScope::SeparateStory),
        )
        .unwrap();
    assert_eq!(decided.state.work.len(), 1);
    assert_eq!(
        decided
            .state
            .decision
            .as_ref()
            .unwrap()
            .dependency_holds
            .len(),
        2
    );
    // A different pinned failing base must not inherit that owner merely because
    // the detector name and assertion signature happen to match.
    let effect = &decided.state.work[0];
    let work = service
        .claim_work(&opened.record.id, &effect.id)
        .unwrap()
        .unwrap();
    let epoch = work.state.work[0].epoch;
    assert!(
        service
            .delivery_permitted(&opened.record.id, Some(&effect.id), epoch, false)
            .unwrap()
    );
    f.base = f.git(&["rev-parse", "HEAD"]);
    f.write("src/lib.rs", "pub fn answer() -> u32 { 38 + 3 }\n");
    f.git(&["add", "src/lib.rs"]);
    f.git(&["commit", "-qm", "third candidate"]);
    let (settled, record) = settled_named(
        &evidence,
        &f,
        false,
        &third,
        "third-attempt",
        "third-attribution",
    );
    let proof = evidence
        .store
        .read(|tx| settled.prove_shared(tx, &third, &record.id, "original"))
        .unwrap();
    let distinct = service.observe_shared(&third, &proof).unwrap().unwrap();
    assert_ne!(distinct.record.id, opened.record.id);
    assert_ne!(distinct.record.locus, opened.record.locus);
    for dispatching in [false, true] {
        assert!(
            !service
                .delivery_permitted(&opened.record.id, Some(&effect.id), epoch, dispatching)
                .unwrap(),
            "cached repair delivery bypassed a newly enrolled fault"
        );
    }
    service
        .settle_work(
            &opened.record.id,
            &effect.id,
            epoch,
            AssessmentDelivery::ProvenFailure("fixture proves no provider call occurred".into()),
        )
        .unwrap();
    assert!(
        service
            .claim_work(&opened.record.id, &effect.id)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        service.show(&opened.record.id).unwrap().state.work[0].epoch,
        epoch
    );
    let input = RepairInput {
        base: "a".repeat(40),
        head: "b".repeat(40),
        head_tree: "c".repeat(40),
        tree: "d".repeat(40),
    };
    let refused = service
        .admit_repair(&third, "cached-shared-submission", &input)
        .unwrap_err();
    assert!(
        refused.to_string().contains("another active shared fault"),
        "{refused}"
    );
    assert_eq!(
        evidence
            .store
            .read(|tx| tx.project_recoveries(ctx.project()))
            .unwrap()
            .len(),
        2
    );
    let repair = decided
        .state
        .decision
        .as_ref()
        .unwrap()
        .repair_story
        .unwrap()
        .to_id("SH");
    PrLinkService::new(&ctx)
        .link(&repair, "https://github.com/acme/widgets/pull/4", true)
        .unwrap();
    stories
        .set_state(&repair, "verifying", None, None, None)
        .unwrap();
    assert!(
        VerificationQueue::new(&evidence.store)
            .with_environment(Environment::at(evidence.fixture.cwd()).with_subprocess_patience())
            .ordered_for(ctx.project())
            .unwrap()
            .is_empty(),
        "repair for fault A bypassed unrelated active fault B"
    );
    evidence
        .store
        .write(|tx| {
            let mut record = tx
                .project_recoveries(ctx.project())?
                .into_iter()
                .find(|r| r.id == opened.record.id)
                .unwrap();
            let revision = record.revision;
            record.revision += 1;
            record.state["created_at"] = serde_json::json!("malformed timestamp");
            assert!(tx.update_project_recovery(&record, revision)?);
            Ok(())
        })
        .unwrap();
    let status = evidence
        .store
        .read(|tx| crate::service::project_recovery::status_snapshot(tx, ctx.project()))
        .unwrap();
    assert_eq!(status.len(), 2);
    assert!(
        status
            .iter()
            .any(|r| r.id == opened.record.id && r.phase == "invalid")
    );
    assert!(
        status
            .iter()
            .any(|r| r.id == distinct.record.id && r.phase != "invalid")
    );
}

#[test]
fn shared_repair_delivery_rechecks_raw_custody_after_scope_decision() {
    let (f, evidence, proof) = fixture(false);
    let ctx = context(&evidence);
    let service = ProjectRecoveryService::new(&ctx);
    let opened = service
        .observe_shared(&evidence.candidate, &proof)
        .unwrap()
        .unwrap();
    let claimed = service
        .claim_assessment(&opened.record.id)
        .unwrap()
        .unwrap();
    let decided = service
        .decide(
            &opened.record.id,
            &decision(&claimed, RepairScope::SeparateStory),
        )
        .unwrap();
    let effect = &decided.state.work[0];
    let work = service
        .claim_work(&opened.record.id, &effect.id)
        .unwrap()
        .unwrap();
    let epoch = work.state.work[0].epoch;
    assert!(
        service
            .delivery_permitted(&opened.record.id, Some(&effect.id), epoch, false)
            .unwrap()
    );
    let raw = f.directory.path().join("probe-0/run.stdout");
    let original = fs::read(&raw).unwrap();
    fs::write(&raw, "substituted after scope decision").unwrap();
    assert!(
        !service
            .delivery_permitted(&opened.record.id, Some(&effect.id), epoch, false)
            .unwrap()
    );
    assert!(
        !service
            .delivery_permitted(&opened.record.id, Some(&effect.id), epoch, true)
            .unwrap(),
        "dispatch fallback bypassed native custody"
    );
    // Settle a proven absent delivery, then verify even a new claim is held.
    service
        .settle_work(
            &opened.record.id,
            &effect.id,
            epoch,
            AssessmentDelivery::ProvenFailure("fixture proves no provider call occurred".into()),
        )
        .unwrap();
    assert!(
        service
            .claim_work(&opened.record.id, &effect.id)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        service.show(&opened.record.id).unwrap().state.work[0].epoch,
        epoch
    );
    fs::write(&raw, original).unwrap();
}
