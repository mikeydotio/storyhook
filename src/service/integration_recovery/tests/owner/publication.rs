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
fn managed_publication_rechecks_manual_control_and_original_deadline_before_intent() {
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
