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
