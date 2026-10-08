//! Actual private Git assembly followed by store-only remote intent boundaries.
//! No remote push, GitHub call, daemon or production policy is exercised.
use super::*;

fn assembled(f: &OwnedFixture, proof: &BoundIntegrationProposal) -> AssembledIntegration {
    assembled_with_id(f, proof).1
}

fn assembled_with_id(
    f: &OwnedFixture,
    proof: &BoundIntegrationProposal,
) -> (String, AssembledIntegration) {
    let record = f.reserve(proof);
    let ctx = f.ctx();
    let service = IntegrationOwnerService::new(&ctx);
    let claim = service
        .claim_assembly(&record.id, record.revision, proof)
        .unwrap()
        .unwrap();
    let native =
        assemble_owned(&service, &claim, proof, proof.deadline, &proof.cancellation).unwrap();
    let ready = service
        .accept_assembly(claim, native, proof)
        .unwrap()
        .unwrap();
    (record.id, ready)
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

fn certified(f: &OwnedFixture, proof: &BoundIntegrationProposal) -> CertifiedIntegration {
    let ready = published(f, proof);
    let attempt = gate_admission(f, "none");
    let ctx = f.ctx();
    let service = IntegrationOwnerService::new(&ctx);
    let cancellation = Cancellation::default();
    let claim = service
        .claim_gate(ready, proof, &attempt, proof.deadline, &cancellation)
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
    execution.journal_bound = true;
    admission.executions.push(execution);
    admission.revision = 1;
    assert!(
        f.store
            .write(|tx| tx.update_gate_attempt(&admission, 0))
            .unwrap()
    );
    let execution = admission.executions.last_mut().unwrap();
    execution.finished_at = Some(AT.into());
    execution.verdict = Some("certified".into());
    admission.verdict = Some("certified".into());
    admission.revision = 2;
    assert!(
        f.store
            .write(|tx| tx.update_gate_attempt(&admission, 1))
            .unwrap()
    );
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
    service.accept_gate(running, native).unwrap().unwrap()
}

#[test]
fn managed_landing_owns_one_distinct_request_and_retains_uncertainty_after_restart() {
    let f = OwnedFixture::new(true);
    let proof = proof(&f);
    let ready = certified(&f, &proof);
    let ctx = f.ctx();
    let service = IntegrationOwnerService::new(&ctx);
    let original = service.show(ready.id()).unwrap();
    let mut claim = service.claim_landing(ready, &proof).unwrap().unwrap();
    assert!(service.landing_permitted(&claim).unwrap());
    let intent = claim.intent().clone();
    assert_eq!(
        intent.pull_request,
        "https://github.com/acme/widgets/pull/7"
    );
    assert_eq!(
        intent.landing_pull_request(),
        "https://github.com/acme/widgets/pull/99"
    );
    assert_eq!(intent.certification.head(), claim.assembly().commit);
    assert_eq!(
        intent.certification.integration().unwrap().original_head,
        f.native.head
    );
    assert_eq!(claim.epoch(), original.1.effect_epoch + 1);
    assert!(service.claim_landing_effect(&mut claim).unwrap());
    let uncertain = service.show(claim.id()).unwrap();
    assert!(!service.claim_landing_effect(&mut claim).unwrap());
    assert_eq!(service.show(claim.id()).unwrap(), uncertain);
    let reopened = SqliteStore::open(f.store.path()).unwrap();
    assert_eq!(
        reopened.read(|tx| tx.landing_intents()).unwrap(),
        [intent.clone()]
    );
    assert_eq!(uncertain.1.started_at, original.1.started_at);
    assert_eq!(uncertain.1.attribution, original.1.attribution);
    assert!(
        VerificationQueue::new(&f.store)
            .complete_landing(&ctx, &intent, "managed merge reported")
            .is_err()
    );
    assert!(
        f.store
            .read(|tx| tx.open_pr_links_for_story(intent.project, intent.story))
            .unwrap()
            .iter()
            .any(|link| link.url == intent.pull_request)
    );
    proof.settle().unwrap();
}

#[test]
fn managed_landing_stop_revokes_new_effect_but_preserves_pending_merge_fence() {
    let f = OwnedFixture::new(true);
    let proof = proof(&f);
    let ready = certified(&f, &proof);
    let ctx = f.ctx();
    let service = IntegrationOwnerService::new(&ctx);
    let mut claim = service.claim_landing(ready, &proof).unwrap().unwrap();
    let before = service.show(claim.id()).unwrap();
    f.store
        .write(|tx| tx.put_verification_enabled(f.candidate.project, false))
        .unwrap();
    assert!(service.landing_permitted(&claim).is_err());
    assert!(service.claim_landing_effect(&mut claim).is_err());
    assert_eq!(service.show(claim.id()).unwrap(), before);
    assert_eq!(
        f.store.read(|tx| tx.landing_intents()).unwrap(),
        [claim.intent().clone()]
    );
    proof.settle().unwrap();
}

#[test]
fn managed_landing_restart_mints_only_fresh_read_only_observation() {
    let f = OwnedFixture::new(true);
    let proof = proof(&f);
    let ready = certified(&f, &proof);
    let ctx = f.ctx();
    let service = IntegrationOwnerService::new(&ctx);
    let mut claim = service.claim_landing(ready, &proof).unwrap().unwrap();
    assert!(service.claim_landing_effect(&mut claim).unwrap());
    let id = claim.id().to_string();
    let before = service.show(&id).unwrap();
    proof.cancellation.cancel();
    f.store
        .write(|tx| tx.put_verification_enabled(f.candidate.project, false))
        .unwrap();
    assert!(service.landing_permitted(&claim).is_err());
    let reopened = SqliteStore::open(f.store.path()).unwrap();
    let reopened_ctx = Ctx::new(
        &reopened,
        f.candidate.project,
        f.candidate.checkout.clone(),
        ctx.env().clone(),
    )
    .no_hooks(true);
    let restarted = IntegrationOwnerService::new(&reopened_ctx);
    let fresh = Cancellation::default();
    let query = restarted
        .observe_landing(&id, proof.deadline, &fresh)
        .unwrap();
    assert!(restarted.landing_observation_permitted(&query).unwrap());
    assert_eq!(query.intent(), claim.intent());
    assert_eq!(query.assembly(), claim.assembly());
    assert_eq!(
        restarted.show(&id).unwrap(),
        before,
        "observation changed durable merge authority"
    );
    fresh.cancel();
    assert!(restarted.landing_observation_permitted(&query).is_err());
    assert_eq!(restarted.show(&id).unwrap(), before);
    proof.settle().unwrap();
}

#[test]
fn managed_native_merge_token_requires_durable_intent_and_is_single_use() {
    let f = OwnedFixture::new(true);
    let proof = proof(&f);
    let ready = certified(&f, &proof);
    let ctx = f.ctx();
    let service = IntegrationOwnerService::new(&ctx);
    let mut claim = service.claim_landing(ready, &proof).unwrap().unwrap();
    assert!(
        claim.take_request().is_err(),
        "native request bypassed durable intent"
    );
    assert!(service.claim_landing_effect(&mut claim).unwrap());
    let before = service.show(claim.id()).unwrap();
    let taken = std::thread::scope(|scope| {
        let first = scope.spawn(|| claim.take_request().unwrap());
        let second = scope.spawn(|| claim.take_request().unwrap());
        usize::from(first.join().unwrap()) + usize::from(second.join().unwrap())
    });
    assert_eq!(
        taken, 1,
        "parallel native callers obtained repeated merge authority"
    );
    assert!(!claim.take_request().unwrap());
    assert_eq!(service.show(claim.id()).unwrap(), before);
    proof.settle().unwrap();
}

#[test]
fn managed_native_landing_refuses_missing_central_guard_before_consuming_request() {
    use crate::daemon::verification::{
        LandingOutcome, ShellVerificationActuator, VerificationActivity, VerificationActuator,
    };
    let f = OwnedFixture::new(true);
    let proof = proof(&f);
    let ready = certified(&f, &proof);
    let ctx = f.ctx();
    let service = IntegrationOwnerService::new(&ctx);
    let mut claim = service.claim_landing(ready, &proof).unwrap().unwrap();
    assert!(service.claim_landing_effect(&mut claim).unwrap());
    let activity = VerificationActivity::new();
    let adapter = ShellVerificationActuator::with_paths(
        ctx.env().clone(),
        f.native.root.path().join("must-not-run-helper"),
        f.native.root.path().join("must-not-run-story"),
    )
    .with_activity(activity);
    let outcome = adapter.land_integration(&claim);
    assert!(
        matches!(outcome, LandingOutcome::Uncertain { ref detail } if detail.contains("no central guard")),
        "{outcome:?}"
    );
    assert!(
        claim.take_request().unwrap(),
        "adapter consumed a request without the actual original central slot"
    );
    proof.settle().unwrap();
}

fn landed_proof(
    f: &OwnedFixture,
    proof: &BoundIntegrationProposal,
    change: impl FnOnce(&mut IntegrationLandedEvidence),
) -> (String, NativeIntegrationLanded, Cancellation) {
    let ready = certified(f, proof);
    let ctx = f.ctx();
    let service = IntegrationOwnerService::new(&ctx);
    let mut claim = service.claim_landing(ready, proof).unwrap().unwrap();
    assert!(service.claim_landing_effect(&mut claim).unwrap());
    let cancellation = Cancellation::default();
    let query = service
        .observe_landing(claim.id(), proof.deadline, &cancellation)
        .unwrap();
    let publication = query.publication();
    let mut evidence = IntegrationLandedEvidence {
        version: 1,
        owner: query.id().into(),
        intent_id: query.intent().id.clone(),
        repository: publication.original.repository.clone(),
        original_pr: publication.original.pull_request.clone(),
        original_head: publication.original.head.clone(),
        managed_pr: publication.pull_request.clone(),
        managed_head: publication.commit.clone(),
        merge_commit: "a".repeat(40),
        merge_tree: publication.tree.clone(),
        base_branch: publication.original.base_branch.clone(),
        observed_base: "b".repeat(40),
        observed_base_tree: "c".repeat(40),
    };
    change(&mut evidence);
    let id = claim.id().to_string();
    let native = crate::service::integration_recovery::landed_observation::fixture_landed(
        query,
        evidence,
        proof.deadline,
        cancellation.clone(),
    )
    .unwrap();
    (id, native, cancellation)
}

#[test]
fn managed_native_completion_preserves_original_pr_status_after_operator_stop() {
    let f = OwnedFixture::new(true);
    let proof = proof(&f);
    let (id, native, _) = landed_proof(&f, &proof, |_| {});
    let ctx = f.ctx();
    let service = IntegrationOwnerService::new(&ctx);
    let intent = native.query().intent().clone();
    let original = service.show(&id).unwrap();
    let links = f
        .store
        .read(|tx| tx.open_pr_links_for_story(intent.project, intent.story))
        .unwrap();
    f.store
        .write(|tx| tx.put_verification_enabled(intent.project, false))
        .unwrap();
    proof.cancellation.cancel();
    assert!(service.complete_landing(&native).unwrap());
    assert!(
        !service.complete_landing(&native).unwrap(),
        "native result replay changed a completed owner"
    );
    let complete = service.show(&id).unwrap();
    assert!(!complete.0.active);
    assert_eq!(complete.1.phase, IntegrationPhase::Landed);
    assert_eq!(complete.1.started_at, original.1.started_at);
    assert_eq!(complete.1.candidate, original.1.candidate);
    assert_eq!(complete.1.landed.as_ref(), Some(native.evidence()));
    assert!(f.store.read(|tx| tx.landing_intents()).unwrap().is_empty());
    assert_eq!(
        f.store
            .read(|tx| tx.open_pr_links_for_story(intent.project, intent.story))
            .unwrap(),
        links
    );
    let events = f
        .store
        .read(|tx| tx.events_for(intent.project, intent.story))
        .unwrap();
    assert!(events.iter().any(|e|matches!(e.known(),Some(crate::domain::StoryEvent::StoryStateChanged{state,..}) if state=="done")));
    assert!(
        !events.iter().any(|e| matches!(
            e.known(),
            Some(crate::domain::StoryEvent::StoryPrMerged { .. })
        )),
        "ancestry invented original PR merge status"
    );
    let attribution = f.store.read(|tx| tx.attributions(intent.project)).unwrap();
    assert!(
        attribution
            .iter()
            .any(|a| a.id == original.1.attribution.id && !a.held && a.retired.is_some())
    );
    let reopened = SqliteStore::open(f.store.path()).unwrap();
    assert_eq!(
        reopened
            .read(|tx| tx.integration_recoveries(intent.project))
            .unwrap(),
        [complete.0]
    );
    let path = native.observation_path().to_path_buf();
    native.settle().unwrap();
    assert!(!path.exists());
    proof.settle().unwrap();
}

#[test]
fn managed_native_completion_rejects_cross_owner_head_tree_and_repository_receipts() {
    for variant in [
        "owner",
        "intent",
        "repository",
        "original-head",
        "managed-pr",
        "tree",
    ] {
        let f = OwnedFixture::new(true);
        let proof = proof(&f);
        let (id, native, _) = landed_proof(&f, &proof, |e| match variant {
            "owner" => e.owner = uuid::Uuid::new_v4().to_string(),
            "intent" => e.intent_id = uuid::Uuid::new_v4().to_string(),
            "repository" => e.repository = "elsewhere.example/acme/widgets".into(),
            "original-head" => e.original_head = "d".repeat(40),
            "managed-pr" => e.managed_pr = e.original_pr.clone(),
            "tree" => e.merge_tree = "e".repeat(40),
            _ => unreachable!(),
        });
        let ctx = f.ctx();
        let service = IntegrationOwnerService::new(&ctx);
        let before = service.show(&id).unwrap();
        let events = f
            .store
            .read(|tx| tx.events_for(before.0.project, before.0.story))
            .unwrap();
        assert!(
            service.complete_landing(&native).is_err(),
            "accepted {variant}"
        );
        assert_eq!(service.show(&id).unwrap(), before);
        assert_eq!(
            f.store
                .read(|tx| tx.events_for(before.0.project, before.0.story))
                .unwrap(),
            events
        );
        native.settle().unwrap();
        proof.settle().unwrap();
    }
}

#[test]
fn managed_native_completion_holds_human_reservation_and_cancelled_observation() {
    for variant in ["human", "cancelled"] {
        let f = OwnedFixture::new(true);
        let proof = proof(&f);
        let (id, native, cancellation) = landed_proof(&f, &proof, |_| {});
        let ctx = f.ctx();
        let service = IntegrationOwnerService::new(&ctx);
        if variant == "human" {
            StoryService::new(&ctx)
                .set_labels(&f.candidate.story_id, &["human-only".into()], &[])
                .unwrap();
        } else {
            cancellation.cancel();
        }
        let before = service.show(&id).unwrap();
        let result = service.complete_landing(&native);
        assert!(!matches!(result, Ok(true)), "revoked {variant} completed");
        assert_eq!(service.show(&id).unwrap(), before);
        assert_eq!(
            f.store.read(|tx| tx.landing_intents()).unwrap(),
            [native.query().intent().clone()]
        );
        native.settle().unwrap();
        proof.settle().unwrap();
    }
}

#[test]
fn managed_restart_worker_owns_central_admission_and_settles_fresh_proof() {
    let f = OwnedFixture::new(true);
    let proof = proof(&f);
    let (id, earlier, _) = landed_proof(&f, &proof, |_| {});
    let expected = earlier.evidence().clone();
    earlier.settle().unwrap();
    // The former central process has ended before this restart admission.
    for mut admission in f
        .store
        .read(|tx| tx.gate_attempts(f.candidate.project))
        .unwrap()
    {
        if admission.finished_at.is_none() {
            let revision = admission.revision;
            admission.finished_at = Some(AT.into());
            admission.revision += 1;
            assert!(
                f.store
                    .write(|tx| tx.update_gate_attempt(&admission, revision))
                    .unwrap()
            );
        }
    }
    let ctx = f.ctx();
    f.store
        .write(|tx| tx.put_verification_enabled(f.candidate.project, false))
        .unwrap();
    let activity = crate::daemon::verification::VerificationActivity::new();
    let inflight = crate::daemon::lifecycle::InFlight::new(ctx.env().clone());
    let mut path = None;
    let result = crate::daemon::verification::integration_worker::reconcile_one_with(
        &f.store,
        ctx.env(),
        &activity,
        &inflight,
        f.candidate.project,
        &id,
        |_service, query, deadline, cancellation| {
            let active = activity
                .active_for(f.candidate.project)
                .expect("no central observation owner");
            assert_eq!(active.story_id, f.candidate.story_id);
            assert_eq!(active.generation, f.candidate.verifying_generation);
            assert_eq!(active.mode, crate::domain::landing::VerificationMode::Gated);
            let attempts = f
                .store
                .read(|tx| tx.gate_attempts(f.candidate.project))
                .unwrap();
            assert!(
                attempts.iter().any(|a| a.id == active.attempt_id
                    && a.executions.is_empty()
                    && a.finished_at.is_none()),
                "readonly observation invented a physical gate"
            );
            let native = crate::service::integration_recovery::landed_observation::fixture_landed(
                query,
                expected,
                deadline,
                cancellation.clone(),
            )?;
            path = Some(native.observation_path().to_path_buf());
            Ok(native)
        },
    )
    .unwrap();
    assert_eq!(result, crate::daemon::verification::TickResult::Completed);
    assert!(
        activity.active_for(f.candidate.project).is_none(),
        "central observation owner leaked"
    );
    assert!(
        !path.unwrap().exists(),
        "fresh proof resource was not settled"
    );
    assert!(
        !IntegrationOwnerService::new(&ctx)
            .show(&id)
            .unwrap()
            .0
            .active
    );
    proof.settle().unwrap();
}

#[test]
fn managed_restart_worker_cancellation_keeps_intent_and_settles_new_proof() {
    let f = OwnedFixture::new(true);
    let proof = proof(&f);
    let (id, earlier, _) = landed_proof(&f, &proof, |_| {});
    let expected = earlier.evidence().clone();
    earlier.settle().unwrap();
    // The former central process has ended before this restart admission.
    for mut admission in f
        .store
        .read(|tx| tx.gate_attempts(f.candidate.project))
        .unwrap()
    {
        if admission.finished_at.is_none() {
            let revision = admission.revision;
            admission.finished_at = Some(AT.into());
            admission.revision += 1;
            assert!(
                f.store
                    .write(|tx| tx.update_gate_attempt(&admission, revision))
                    .unwrap()
            );
        }
    }
    let ctx = f.ctx();
    let service = IntegrationOwnerService::new(&ctx);
    let before = service.show(&id).unwrap();
    let activity = crate::daemon::verification::VerificationActivity::new();
    let inflight = crate::daemon::lifecycle::InFlight::new(ctx.env().clone());
    let mut path = None;
    let result = crate::daemon::verification::integration_worker::reconcile_one_with(
        &f.store,
        ctx.env(),
        &activity,
        &inflight,
        f.candidate.project,
        &id,
        |_service, query, deadline, cancellation| {
            let native = crate::service::integration_recovery::landed_observation::fixture_landed(
                query,
                expected,
                deadline,
                cancellation.clone(),
            )?;
            path = Some(native.observation_path().to_path_buf());
            cancellation.cancel();
            Ok(native)
        },
    );
    assert!(result.is_err());
    assert_eq!(service.show(&id).unwrap(), before);
    assert_eq!(
        f.store.read(|tx| tx.landing_intents()).unwrap(),
        [before.1.landing.unwrap()]
    );
    assert!(activity.active_for(f.candidate.project).is_none());
    assert!(
        !path.unwrap().exists(),
        "revoked fresh proof was silently dropped"
    );
    proof.settle().unwrap();
}

#[test]
fn managed_restart_worker_reports_refused_completion_and_exact_cleanup_residue() {
    let f = OwnedFixture::new(true);
    let proof = proof(&f);
    let (id, earlier, _) = landed_proof(&f, &proof, |_| {});
    let expected = earlier.evidence().clone();
    earlier.settle().unwrap();
    // The former central process has ended before this restart admission.
    for mut admission in f
        .store
        .read(|tx| tx.gate_attempts(f.candidate.project))
        .unwrap()
    {
        if admission.finished_at.is_none() {
            let revision = admission.revision;
            admission.finished_at = Some(AT.into());
            admission.revision += 1;
            assert!(
                f.store
                    .write(|tx| tx.update_gate_attempt(&admission, revision))
                    .unwrap()
            );
        }
    }
    let ctx = f.ctx();
    let service = IntegrationOwnerService::new(&ctx);
    let before = service.show(&id).unwrap();
    let activity = crate::daemon::verification::VerificationActivity::new();
    let inflight = crate::daemon::lifecycle::InFlight::new(ctx.env().clone());
    let mut path = None;
    let result = crate::daemon::verification::integration_worker::reconcile_one_with(
        &f.store,
        ctx.env(),
        &activity,
        &inflight,
        f.candidate.project,
        &id,
        |_service, query, deadline, cancellation| {
            let native = crate::service::integration_recovery::landed_observation::fixture_landed(
                query,
                expected,
                deadline,
                cancellation.clone(),
            )?;
            path = Some(native.observation_path().to_path_buf());
            std::fs::create_dir(native.observation_path().join(".git")).unwrap();
            cancellation.cancel();
            Ok(native)
        },
    );
    let error = result.unwrap_err().to_string();
    let path = path.expect("native observation was not created");
    assert!(
        error.contains("managed completion refused")
            && error.contains("cleanup failed")
            && error.contains(path.to_str().unwrap()),
        "{error}"
    );
    assert_eq!(service.show(&id).unwrap(), before);
    assert_eq!(
        f.store.read(|tx| tx.landing_intents()).unwrap(),
        [before.1.landing.unwrap()]
    );
    assert!(activity.active_for(f.candidate.project).is_none());
    assert!(
        path.join(".git").exists(),
        "uncertain cleanup removed unexpected custody"
    );
    // Only this fixture created every path here; production keeps the residue.
    std::fs::remove_dir_all(&path).unwrap();
    proof.settle().unwrap();
}

#[test]
fn managed_completed_owner_retains_cleanup_failure_without_reactivating_effects() {
    let f = OwnedFixture::new(true);
    let proof = proof(&f);
    let (id, native, _) = landed_proof(&f, &proof, |_| {});
    let ctx = f.ctx();
    let service = IntegrationOwnerService::new(&ctx);
    assert!(service.complete_landing(&native).unwrap());
    native.settle().unwrap();
    let before = service.show(&id).unwrap();
    let residue = before.1.workspace.display().to_string();
    let prior_status = f
        .store
        .read(|tx| crate::service::integration_recovery::status_snapshot(tx, f.candidate.project))
        .unwrap();
    let later_env = ctx
        .env()
        .clone()
        .clock(crate::service::Clock::Fixed("2099-01-01T00:00:00Z".into()));
    assert_ne!(later_env.now(), before.1.updated_at);
    let later_ctx = Ctx::new(
        &f.store,
        f.candidate.project,
        f.native.root.path(),
        later_env,
    )
    .no_hooks(true);
    IntegrationOwnerService::new(&later_ctx)
        .note_hold(
            &id,
            &format!("assembly cleanup incomplete; residue retained at {residue}"),
        )
        .unwrap();
    let after = service.show(&id).unwrap();
    assert!(!after.0.active);
    assert_eq!(after.1.phase, IntegrationPhase::Landed);
    assert_eq!(after.1.effect_epoch, before.1.effect_epoch);
    assert_eq!(after.1.landed, before.1.landed);
    assert_eq!(after.1.updated_at, before.1.updated_at);
    let later_status = f
        .store
        .read(|tx| crate::service::integration_recovery::status_snapshot(tx, f.candidate.project))
        .unwrap();
    assert_eq!(
        later_status[0].elapsed_milliseconds,
        prior_status[0].elapsed_milliseconds
    );
    assert!(after.1.hold.as_deref().unwrap().contains(&residue));
    assert!(f.store.read(|tx| tx.landing_intents()).unwrap().is_empty());
    assert!(
        service
            .claim_assembly(&id, after.0.revision, &proof)
            .unwrap()
            .is_none()
    );
    proof.settle().unwrap();
}

#[test]
fn retained_branch_diagnostic_cannot_replace_owner_custody_or_claim_landing() {
    let f = OwnedFixture::new(true);
    let proof = proof(&f);
    let (id, _ready) = assembled_with_id(&f, &proof);
    let ctx = f.ctx();
    let service = IntegrationOwnerService::new(&ctx);
    let before = service.show(&id).unwrap();
    let assembly = before.1.assembly.as_ref().unwrap();
    let observed = RetainedBranchObservation {
        version: 1,
        owner: assembly.owner.clone(),
        assembly_epoch: assembly.epoch,
        repository: assembly.submission.repository.clone(),
        reference: format!("refs/heads/{}", assembly.branch),
        expected_head: assembly.commit.clone(),
        observed_at: AT.into(),
        outcome: RetainedBranchOutcome::Absent,
    };
    for changed in ["owner", "epoch", "origin", "ref", "tip"] {
        let mut other = observed.clone();
        match changed {
            "owner" => other.owner = uuid::Uuid::new_v4().to_string(),
            "epoch" => other.assembly_epoch += 1,
            "origin" => other.repository = "other.example/acme/widgets".into(),
            "ref" => other.reference = "refs/heads/author".into(),
            "tip" => other.expected_head = "f".repeat(40),
            _ => unreachable!(),
        }
        assert!(
            service
                .record_branch_observation(&id, before.0.revision, &other)
                .is_err(),
            "{changed}"
        );
        assert_eq!(service.show(&id).unwrap(), before);
    }
    assert!(
        service
            .record_branch_observation(&id, before.0.revision, &observed)
            .unwrap()
    );
    let after = service.show(&id).unwrap();
    assert!(after.0.active);
    assert_eq!(after.1.phase, before.1.phase);
    assert_eq!(after.1.effect_epoch, before.1.effect_epoch);
    assert_eq!(after.1.updated_at, before.1.updated_at);
    assert!(after.1.landed.is_none());
    assert!(
        !service
            .record_branch_observation(&id, before.0.revision, &observed)
            .unwrap()
    );
    let status = f
        .store
        .read(|tx| crate::service::integration_recovery::status_snapshot(tx, f.candidate.project))
        .unwrap();
    assert_eq!(status[0].retained_branch, Some(observed));
    proof.settle().unwrap();
}
