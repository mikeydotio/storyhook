//! Actual private Git assembly followed by store-only remote intent boundaries.
//! No remote push, GitHub call, daemon or production policy is exercised.
use super::*;

fn assembled(f: &OwnedFixture, proof: &BoundIntegrationProposal) -> AssembledIntegration {
    let record = f.reserve(proof);
    let ctx = f.ctx();
    let service = IntegrationOwnerService::new(&ctx);
    let claim = service
        .claim_assembly(&record.id, record.revision, proof)
        .unwrap()
        .unwrap();
    let native =
        assemble_owned(&service, &claim, proof, proof.deadline, &proof.cancellation).unwrap();
    service
        .accept_assembly(claim, native, proof)
        .unwrap()
        .unwrap()
}

fn proof(f: &OwnedFixture) -> BoundIntegrationProposal {
    let mut proof = f.proof();
    proof.deadline =
        Instant::now() + storyhook_test_support::load_grace::graced_now(Duration::from_secs(90));
    proof
}

#[test]
fn managed_publication_intents_are_ordered_once_and_survive_restart() {
    let f = OwnedFixture::new(true);
    let before = f.native.snapshot();
    let proof = proof(&f);
    let ready = assembled(&f, &proof);
    let ctx = f.ctx();
    let service = IntegrationOwnerService::new(&ctx);
    let mut claim = service.claim_publication(ready, &proof).unwrap().unwrap();
    let original = service.show(claim.id()).unwrap();
    assert_eq!(original.1.phase, IntegrationPhase::Publishing);
    assert_eq!(claim.epoch(), claim.assembly().epoch + 1);
    assert_eq!(claim.assembly().plan.head, f.native.head);
    assert!(service.publication_permitted(&claim, &proof).unwrap());
    assert!(
        !service
            .claim_publication_effect(&mut claim, &proof, PublicationEffect::CreatePullRequest)
            .unwrap()
    );
    assert_eq!(service.show(claim.id()).unwrap(), original);
    assert!(
        service
            .claim_publication_effect(&mut claim, &proof, PublicationEffect::PushBranch)
            .unwrap()
    );
    let uncertain_push = service.show(claim.id()).unwrap();
    assert!(
        !service
            .claim_publication_effect(&mut claim, &proof, PublicationEffect::PushBranch)
            .unwrap()
    );
    assert_eq!(service.show(claim.id()).unwrap(), uncertain_push);
    let reopened = SqliteStore::open(f.store.path()).unwrap();
    let restarted_ctx = Ctx::new(
        &reopened,
        f.candidate.project,
        f.native.root.path(),
        ctx.env().clone(),
    )
    .no_hooks(true);
    let restarted = IntegrationOwnerService::new(&restarted_ctx);
    assert_eq!(restarted.show(claim.id()).unwrap(), uncertain_push);
    assert!(
        restarted
            .claim_assembly(claim.id(), uncertain_push.0.revision, &proof)
            .unwrap()
            .is_none()
    );
    // Only an in-process owner can mark the next possible effect. This is not
    // a branch success receipt; the native publisher must inspect it separately.
    assert!(
        service
            .claim_publication_effect(&mut claim, &proof, PublicationEffect::CreatePullRequest)
            .unwrap()
    );
    assert!(
        !service
            .claim_publication_effect(&mut claim, &proof, PublicationEffect::CreatePullRequest)
            .unwrap()
    );
    assert_eq!(
        claim.effects(),
        &[
            PublicationEffect::PushBranch,
            PublicationEffect::CreatePullRequest
        ]
    );
    assert_eq!(
        f.native.snapshot(),
        before,
        "publication intents rewrote the author checkout"
    );
    proof.settle().unwrap();
}

#[test]
fn managed_publication_rechecks_manual_control_and_cancellation_before_intent() {
    for cancelled in [false, true] {
        let f = OwnedFixture::new(true);
        let proof = proof(&f);
        let ready = assembled(&f, &proof);
        let ctx = f.ctx();
        let service = IntegrationOwnerService::new(&ctx);
        let mut claim = service.claim_publication(ready, &proof).unwrap().unwrap();
        let before = service.show(claim.id()).unwrap();
        if cancelled {
            proof.cancellation.cancel();
        } else {
            f.store
                .write(|tx| tx.put_verification_enabled(f.candidate.project, false))
                .unwrap();
        }
        assert!(
            service
                .claim_publication_effect(&mut claim, &proof, PublicationEffect::PushBranch)
                .is_err()
        );
        assert_eq!(service.show(claim.id()).unwrap(), before);
        assert!(claim.effects().is_empty());
        proof.settle().unwrap();
    }
}

#[test]
fn managed_publication_refuses_replaced_native_workspace_without_adopting_it() {
    let f = OwnedFixture::new(true);
    let proof = proof(&f);
    let ready = assembled(&f, &proof);
    let ctx = f.ctx();
    let service = IntegrationOwnerService::new(&ctx);
    let mut claim = service.claim_publication(ready, &proof).unwrap().unwrap();
    let before = service.show(claim.id()).unwrap();
    let original = claim.assembly().workspace.path.clone();
    let moved = original.with_extension("retained-original");
    fs::rename(&original, &moved).unwrap();
    fs::create_dir(&original).unwrap();
    fs::write(original.join("replacement"), "must survive").unwrap();
    assert!(service.publication_permitted(&claim, &proof).is_err());
    assert!(
        service
            .claim_publication_effect(&mut claim, &proof, PublicationEffect::PushBranch)
            .is_err()
    );
    assert_eq!(service.show(claim.id()).unwrap(), before);
    assert_eq!(
        fs::read_to_string(original.join("replacement")).unwrap(),
        "must survive"
    );
    assert!(moved.join("storyhook-assembly.json").exists());
    proof.settle().unwrap();
}

#[test]
fn fresh_native_inspection_cannot_renew_cancelled_publication_operation() {
    let f = OwnedFixture::new(true);
    let initial = proof(&f);
    let ready = assembled(&f, &initial);
    let ctx = f.ctx();
    let service = IntegrationOwnerService::new(&ctx);
    let mut claim = service.claim_publication(ready, &initial).unwrap().unwrap();
    let before = service.show(claim.id()).unwrap();
    initial.cancellation.cancel();
    let fresh = proof(&f);
    fresh.check_live().unwrap();
    assert_eq!(initial.plan(), fresh.plan());
    assert_eq!(initial.submission(), fresh.submission());
    assert!(service.publication_permitted(&claim, &fresh).is_err());
    assert!(
        service
            .claim_publication_effect(&mut claim, &fresh, PublicationEffect::PushBranch)
            .is_err()
    );
    assert_eq!(service.show(claim.id()).unwrap(), before);
    initial.settle().unwrap();
    fresh.settle().unwrap();
}

#[test]
fn managed_integration_status_reports_original_time_and_isolates_invalid_evidence() {
    let f = OwnedFixture::new(true);
    let proof = proof(&f);
    let record = f.reserve(&proof);
    let ctx = f.ctx();
    let statuses = f
        .store
        .read(|tx| crate::service::integration_recovery::status_snapshot(tx, f.candidate.project))
        .unwrap();
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].started_at.as_deref(), Some(AT));
    assert_eq!(
        statuses[0].original_head.as_deref(),
        Some(f.native.head.as_str())
    );
    let bad_story = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "invalid retained status fixture".into(),
            ..Default::default()
        })
        .unwrap();
    let number = f
        .store
        .read(|tx| {
            let prefix = crate::service::project_prefix(tx, f.candidate.project)?;
            crate::store::StoryNo::parse_id(&prefix, &bad_story.id)
        })
        .unwrap();
    f.store
        .write(|tx| {
            let mut invalid = record.clone();
            invalid.id = uuid::Uuid::new_v4().simple().to_string();
            invalid.story = number;
            invalid.state = serde_json::json!({"version":1,"started_at":"invalid"});
            assert!(tx.insert_integration_recovery(&invalid)?);
            Ok(())
        })
        .unwrap();
    let statuses = f
        .store
        .read(|tx| crate::service::integration_recovery::status_snapshot(tx, f.candidate.project))
        .unwrap();
    assert_eq!(statuses.len(), 2);
    assert!(statuses.iter().any(|s| s.id == record.id
        && s.phase == "reserved"
        && s.started_at.as_deref() == Some(AT)));
    let invalid = statuses.iter().find(|s| s.phase == "invalid").unwrap();
    assert_eq!(invalid.original_head, None);
    assert_eq!(invalid.elapsed_milliseconds, None);
    assert_eq!(invalid.effect_epoch, None);
    proof.settle().unwrap();
}

fn publication_evidence(claim: &PublicationClaim) -> PublicationEvidence {
    let assembly = claim.assembly();
    PublicationEvidence {
        version: 1,
        owner: claim.id().into(),
        epoch: claim.epoch(),
        original: claim.submission().clone(),
        branch: assembly.branch.clone(),
        commit: assembly.commit.clone(),
        tree: assembly.tree.clone(),
        parents: [assembly.plan.base.clone(), assembly.plan.head.clone()],
        marker: format!(
            "<!-- storyhook-integration-owner:{}:{}:{} -->",
            claim.id(),
            claim.epoch(),
            assembly.stamp_sha256
        ),
        pull_request: "https://github.com/acme/widgets/pull/99".into(),
        number: 99,
    }
}

#[test]
fn native_publication_acceptance_retains_original_story_and_requires_exact_owned_receipt() {
    for changed in [
        "none",
        "tree",
        "parents",
        "original-pr",
        "repository",
        "epoch",
    ] {
        let f = OwnedFixture::new(true);
        let proof = proof(&f);
        let ready = assembled(&f, &proof);
        let ctx = f.ctx();
        let service = IntegrationOwnerService::new(&ctx);
        let mut claim = service.claim_publication(ready, &proof).unwrap().unwrap();
        assert!(
            service
                .claim_publication_effect(&mut claim, &proof, PublicationEffect::PushBranch)
                .unwrap()
        );
        assert!(
            service
                .claim_publication_effect(&mut claim, &proof, PublicationEffect::CreatePullRequest)
                .unwrap()
        );
        let mut evidence = publication_evidence(&claim);
        match changed {
            "tree" => evidence.tree = "f".repeat(40),
            "parents" => evidence.parents.reverse(),
            "original-pr" => {
                evidence.pull_request = evidence.original.pull_request.clone();
                evidence.number = 7;
            }
            "repository" => {
                evidence.pull_request = "https://elsewhere.invalid/acme/widgets/pull/99".into()
            }
            "epoch" => evidence.epoch += 1,
            _ => {}
        }
        let before = service.show(claim.id()).unwrap();
        let id = claim.id().to_string();
        let native =
            crate::service::integration_recovery::publication::fixture_publication(evidence);
        let result = service.accept_publication(claim, native, &proof);
        if changed == "none" {
            let accepted = result.unwrap().unwrap();
            assert_eq!(accepted.id(), id);
            accepted.validate_custody().unwrap();
            let state = service.show(&id).unwrap();
            assert_eq!(state.1.phase, IntegrationPhase::Published);
            assert_eq!(state.1.started_at, before.1.started_at);
            assert_eq!(state.1.candidate, before.1.candidate);
            assert_eq!(state.1.attribution, before.1.attribution);
            let reopened = SqliteStore::open(f.store.path()).unwrap();
            assert_eq!(
                reopened
                    .read(|tx| tx.integration_recoveries(f.candidate.project))
                    .unwrap(),
                vec![state.0]
            );
            let row = f
                .store
                .read(|tx| tx.story(f.candidate.project, before.0.story))
                .unwrap()
                .unwrap();
            assert_eq!(row.state, "verifying");
            let links = f
                .store
                .read(|tx| tx.open_pr_links_for_story(f.candidate.project, before.0.story))
                .unwrap();
            assert!(
                links
                    .iter()
                    .any(|link| link.url == "https://github.com/acme/widgets/pull/7"
                        && link.close_on_merge),
                "publication fabricated original PR closure"
            );
        } else {
            assert!(result.is_err(), "accepted substituted {changed}");
            assert_eq!(service.show(&id).unwrap(), before);
        }
        proof.settle().unwrap();
    }
}

fn published(f: &OwnedFixture, proof: &BoundIntegrationProposal) -> PublishedIntegration {
    let ready = assembled(f, proof);
    let ctx = f.ctx();
    let service = IntegrationOwnerService::new(&ctx);
    let mut claim = service.claim_publication(ready, proof).unwrap().unwrap();
    for effect in [
        PublicationEffect::PushBranch,
        PublicationEffect::CreatePullRequest,
    ] {
        assert!(
            service
                .claim_publication_effect(&mut claim, proof, effect)
                .unwrap()
        );
    }
    let native = crate::service::integration_recovery::publication::fixture_publication(
        publication_evidence(&claim),
    );
    service
        .accept_publication(claim, native, proof)
        .unwrap()
        .unwrap()
}

fn gate_admission(f: &OwnedFixture, changed: &str) -> String {
    let original = f
        .store
        .read(|tx| tx.gate_attempts(f.candidate.project))
        .unwrap()
        .into_iter()
        .find(|a| a.id == "original-conflict")
        .unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let mut attempt = GateAttempt::new(id.clone(), original.submission, AT);
    attempt.control_revision = original.control_revision;
    match changed {
        "skipped" => attempt.mode = crate::domain::landing::VerificationMode::VerificationSkipped,
        "control" => attempt.control_revision = Some(999),
        "generation" => attempt.submission.generation = None,
        _ => {}
    }
    f.store
        .write(|tx| tx.insert_gate_attempt(&attempt))
        .unwrap();
    if changed == "finished" {
        attempt.revision = 1;
        attempt.finished_at = Some(AT.into());
        attempt.verdict = Some("certified".into());
        assert!(
            f.store
                .write(|tx| tx.update_gate_attempt(&attempt, 0))
                .unwrap()
        );
    }
    id
}

#[test]
fn managed_gate_claim_requires_live_gated_original_admission_and_preserves_lineage() {
    for changed in [
        "none",
        "missing",
        "skipped",
        "control",
        "generation",
        "finished",
        "original",
    ] {
        let f = OwnedFixture::new(true);
        let proof = proof(&f);
        let ready = published(&f, &proof);
        let id = ready.id().to_string();
        let ctx = f.ctx();
        let service = IntegrationOwnerService::new(&ctx);
        let before = service.show(&id).unwrap();
        let attempt = match changed {
            "missing" => "missing".into(),
            "original" => "original-conflict".into(),
            _ => gate_admission(&f, changed),
        };
        let cancellation = Cancellation::default();
        let result = service.claim_gate(ready, &proof, &attempt, proof.deadline, &cancellation);
        if changed == "none" {
            let claim = result.unwrap().unwrap();
            assert!(service.gate_permitted(&claim).unwrap());
            assert_eq!(claim.candidate(), &f.candidate);
            assert_eq!(claim.attempt(), attempt);
            assert_eq!(claim.publication().original.head, f.native.head);
            assert_eq!(
                claim.publication().pull_request,
                "https://github.com/acme/widgets/pull/99"
            );
            let after = service.show(&id).unwrap();
            assert_eq!(after.1.phase, IntegrationPhase::Gating);
            assert_eq!(after.1.started_at, before.1.started_at);
            assert_eq!(after.1.attribution, before.1.attribution);
            assert_eq!(after.1.effect_epoch, before.1.effect_epoch + 1);
            let reopened = SqliteStore::open(f.store.path()).unwrap();
            assert_eq!(
                reopened
                    .read(|tx| tx.integration_recoveries(f.candidate.project))
                    .unwrap(),
                vec![after.0]
            );
        } else {
            assert!(result.is_err(), "accepted {changed} admission");
            assert_eq!(service.show(&id).unwrap(), before);
        }
        proof.settle().unwrap();
    }
}

#[test]
fn managed_gate_operation_retains_cancellation_and_rechecks_operator_before_effect() {
    for changed in ["cancel", "stop", "finished"] {
        let f = OwnedFixture::new(true);
        let proof = proof(&f);
        let ready = published(&f, &proof);
        let attempt = gate_admission(&f, "none");
        let ctx = f.ctx();
        let service = IntegrationOwnerService::new(&ctx);
        let cancellation = Cancellation::default();
        let claim = service
            .claim_gate(ready, &proof, &attempt, proof.deadline, &cancellation)
            .unwrap()
            .unwrap();
        assert!(service.gate_permitted(&claim).unwrap());
        let before = service.show(claim.id()).unwrap();
        match changed {
            "cancel" => cancellation.cancel(),
            "stop" => f
                .store
                .write(|tx| tx.put_verification_enabled(f.candidate.project, false))
                .unwrap(),
            _ => {
                let mut record = f
                    .store
                    .read(|tx| tx.gate_attempts(f.candidate.project))
                    .unwrap()
                    .into_iter()
                    .find(|a| a.id == attempt)
                    .unwrap();
                record.finished_at = Some(AT.into());
                record.verdict = Some("interrupted".into());
                record.revision = 1;
                assert!(
                    f.store
                        .write(|tx| tx.update_gate_attempt(&record, 0))
                        .unwrap()
                );
            }
        }
        assert!(
            service.gate_permitted(&claim).is_err(),
            "retained {changed} authority"
        );
        assert_eq!(service.show(claim.id()).unwrap(), before);
        proof.settle().unwrap();
    }
}

#[test]
fn managed_gate_expired_operation_cannot_claim_a_published_receipt() {
    let f = OwnedFixture::new(true);
    let proof = proof(&f);
    let ready = published(&f, &proof);
    let id = ready.id().to_string();
    let attempt = gate_admission(&f, "none");
    let ctx = f.ctx();
    let service = IntegrationOwnerService::new(&ctx);
    let before = service.show(&id).unwrap();
    assert!(
        service
            .claim_gate(
                ready,
                &proof,
                &attempt,
                Instant::now(),
                &Cancellation::default()
            )
            .is_err()
    );
    assert_eq!(service.show(&id).unwrap(), before);
    proof.settle().unwrap();
}

fn gate_inputs(claim: &IntegrationGateClaim) -> IntegrationGateInputsEvidence {
    IntegrationGateInputsEvidence {
        version: 1,
        owner: claim.id().into(),
        attempt: claim.attempt().into(),
        publication: claim.publication().clone(),
        current_base: claim.assembly().plan.base.clone(),
        base_branch: claim.publication().original.base_branch.clone(),
        tree: claim.publication().tree.clone(),
        policy: claim.assembly().plan.policy.clone(),
        parents: claim.publication().parents.clone(),
    }
}

#[test]
fn native_gate_start_consumes_exact_fresh_inputs_without_rewriting_submission() {
    for changed in [
        "none",
        "base",
        "tree",
        "owner",
        "attempt",
        "expired",
        "cancelled",
    ] {
        let f = OwnedFixture::new(true);
        let proof = proof(&f);
        let ready = published(&f, &proof);
        let attempt = gate_admission(&f, "none");
        let ctx = f.ctx();
        let service = IntegrationOwnerService::new(&ctx);
        let cancellation = Cancellation::default();
        let claim = service
            .claim_gate(ready, &proof, &attempt, proof.deadline, &cancellation)
            .unwrap()
            .unwrap();
        let id = claim.id().to_string();
        let before = service.show(&id).unwrap();
        let mut inputs = gate_inputs(&claim);
        let observation_cancel = Cancellation::default();
        let deadline = if changed == "expired" {
            Instant::now()
        } else {
            proof.deadline
        };
        match changed {
            "base" => inputs.current_base = "f".repeat(40),
            "tree" => inputs.tree = "f".repeat(40),
            "owner" => inputs.owner = uuid::Uuid::new_v4().to_string(),
            "attempt" => inputs.attempt = uuid::Uuid::new_v4().to_string(),
            "cancelled" => observation_cancel.cancel(),
            _ => {}
        }
        // Substitute only the native remote observation boundary; actual local
        // assembly, custody, original proposal and Store CAS remain real.
        let native = crate::service::integration_recovery::gate_inputs::fixture_gate_inputs(
            inputs.clone(),
            deadline,
            observation_cancel,
        );
        let result = service.start_gate(claim, native);
        if changed == "none" {
            let running = result.unwrap().unwrap();
            assert!(service.gate_permitted(&running).unwrap());
            assert!(running.owns_cancellation(&cancellation));
            assert!(!running.owns_cancellation(&Cancellation::default()));
            assert_eq!(running.inputs(), Some(&inputs));
            let after = service.show(&id).unwrap();
            assert_eq!(after.1.phase, IntegrationPhase::Running);
            assert_eq!(after.1.started_at, before.1.started_at);
            assert_eq!(after.1.effect_epoch, before.1.effect_epoch);
            assert_eq!(after.1.attribution, before.1.attribution);
            cancellation.cancel();
            assert!(service.gate_permitted(&running).is_err());
            assert_eq!(service.show(&id).unwrap(), after);
        } else {
            assert!(result.is_err(), "accepted {changed} native observation");
            assert_eq!(service.show(&id).unwrap(), before);
        }
        proof.settle().unwrap();
    }
}

#[test]
fn running_gate_keeps_central_admission_after_physical_result_until_guard_settles() {
    let f = OwnedFixture::new(true);
    let proof = proof(&f);
    let ready = published(&f, &proof);
    let attempt = gate_admission(&f, "none");
    let ctx = f.ctx();
    let service = IntegrationOwnerService::new(&ctx);
    let cancellation = Cancellation::default();
    let claim = service
        .claim_gate(ready, &proof, &attempt, proof.deadline, &cancellation)
        .unwrap()
        .unwrap();
    let native = crate::service::integration_recovery::gate_inputs::fixture_gate_inputs(
        gate_inputs(&claim),
        proof.deadline,
        Cancellation::default(),
    );
    let running = service.start_gate(claim, native).unwrap().unwrap();
    let mut admission = f
        .store
        .read(|tx| tx.gate_attempts(f.candidate.project))
        .unwrap()
        .into_iter()
        .find(|a| a.id == attempt)
        .unwrap();
    admission.verdict = Some("certified".into());
    admission.revision = 1;
    assert!(
        f.store
            .write(|tx| tx.update_gate_attempt(&admission, 0))
            .unwrap()
    );
    assert!(
        service.gate_permitted(&running).unwrap(),
        "physical verdict prematurely retired native post-gate observation"
    );
    assert!(
        service.show(running.id()).unwrap().1.gate.is_none(),
        "accounting alone minted a certificate"
    );
    admission.finished_at = Some(AT.into());
    admission.revision = 2;
    assert!(
        f.store
            .write(|tx| tx.update_gate_attempt(&admission, 1))
            .unwrap()
    );
    assert!(
        service.gate_permitted(&running).is_err(),
        "retired admission retained process authority"
    );
    proof.settle().unwrap();
}

#[test]
fn managed_certificate_requires_exact_settled_original_physical_gate() {
    for changed in [
        "none",
        "missing",
        "unfinished",
        "estimated",
        "unbound",
        "wrong-tree",
        "wrong-submission",
    ] {
        let f = OwnedFixture::new(true);
        let proof = proof(&f);
        let ready = published(&f, &proof);
        let attempt = gate_admission(&f, "none");
        let ctx = f.ctx();
        let service = IntegrationOwnerService::new(&ctx);
        let cancellation = Cancellation::default();
        let claim = service
            .claim_gate(ready, &proof, &attempt, proof.deadline, &cancellation)
            .unwrap()
            .unwrap();
        let inputs = gate_inputs(&claim);
        let native = crate::service::integration_recovery::gate_inputs::fixture_gate_inputs(
            inputs.clone(),
            proof.deadline,
            cancellation.clone(),
        );
        let running = service.start_gate(claim, native).unwrap().unwrap();
        let mut admission = f
            .store
            .read(|tx| tx.gate_attempts(f.candidate.project))
            .unwrap()
            .into_iter()
            .find(|a| a.id == attempt)
            .unwrap();
        let execution_id = uuid::Uuid::new_v4().to_string();
        let mut execution = crate::store::GateExecution::new(
            execution_id.clone(),
            AT,
            "scratch-managed-gate.ndjson".into(),
        );
        execution.inputs = GateInputs {
            head: Some(inputs.publication.commit.clone()),
            base: Some(inputs.current_base.clone()),
            tree: Some(inputs.tree.clone()),
            ..Default::default()
        };
        execution.submissions = vec![admission.submission.clone()];
        execution.journal_bound = changed != "unbound";
        if changed == "wrong-tree" {
            execution.inputs.tree = Some("f".repeat(40));
        }
        if changed == "wrong-submission" {
            execution.submissions[0].generation = None;
        }
        // The fixture goes through real live execution insertion, then completion
        // CAS; no production accounting invariant is disabled for this test.
        if changed != "missing" {
            admission.executions.push(execution);
            admission.revision += 1;
            assert!(
                f.store
                    .write(|tx| tx.update_gate_attempt(&admission, 0))
                    .unwrap()
            );
            let execution = admission.executions.last_mut().unwrap();
            if changed != "unfinished" {
                execution.finished_at = Some(AT.into());
            }
            execution.estimated = changed == "estimated";
            execution.verdict = Some("certified".into());
            admission.verdict = Some("certified".into());
            admission.revision += 1;
            assert!(
                f.store
                    .write(|tx| tx.update_gate_attempt(&admission, 1))
                    .unwrap()
            );
        }
        let before = service.show(running.id()).unwrap();
        let certification = crate::domain::landing::VerifiedSubmission {
            head: inputs.publication.commit.clone(),
            tree: inputs.tree.clone(),
            gate: "make test".into(),
        };
        let fresh = crate::service::integration_recovery::gate_inputs::fixture_gate_inputs(
            inputs,
            proof.deadline,
            cancellation,
        );
        let native = crate::daemon::verification::integration_gate::fixture_certification(
            execution_id,
            certification,
            fresh,
        );
        let id = running.id().to_string();
        let result = service.accept_gate(running, native);
        if changed == "none" {
            let certified = result.unwrap().unwrap();
            certified.validate_custody().unwrap();
            let after = service.show(&id).unwrap();
            assert_eq!(after.1.phase, IntegrationPhase::Certified);
            assert_eq!(after.1.candidate, before.1.candidate);
            assert_eq!(after.1.attribution, before.1.attribution);
            assert_eq!(after.1.started_at, before.1.started_at);
            assert!(
                f.store.read(|tx| tx.landing_intents()).unwrap().is_empty(),
                "certificate silently admitted a merge"
            );
        } else {
            assert!(result.is_err(), "certified {changed} physical evidence");
            assert_eq!(service.show(&id).unwrap(), before);
        }
        proof.settle().unwrap();
    }
}
