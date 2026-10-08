//! Real local Git proposals and durable store boundaries; no remote effects.
use super::*;
use crate::{
    env::Environment,
    service::{
        Ctx, NewStoryInput, PrLinkService, StoryService, VerificationCandidate, VerificationQueue,
        attribution::{AttributionRecord, FailureCause, FailureComponent},
    },
    store::{
        GateAttempt, GateInputs, GateSubmission, ProjectId, ReadOps, SqliteStore, Store, WriteOps,
    },
};

const AT: &str = "2026-10-05T00:00:00Z";

struct OwnedFixture {
    native: Fixture,
    fixture: storyhook_test_support::ServiceFixture,
    store: SqliteStore,
    candidate: VerificationCandidate,
}

impl OwnedFixture {
    fn new(settled: bool) -> Self {
        let native = Fixture::new(
            SINGLE,
            "docs/guide.md",
            b"start\nbase addition\nend\n",
            b"start\nauthor addition\nend\n",
        );
        let fixture = storyhook_test_support::ServiceFixture::new();
        fixture.github_checkout_at(
            fixture.project(),
            native.root.path(),
            "https://github.com/acme/widgets.git",
        );
        let store = SqliteStore::open(fixture.store().path()).unwrap();
        let project = ProjectId::new(fixture.project().get());
        let ctx = Ctx::new(
            &store,
            project,
            native.root.path(),
            Environment::at(fixture.cwd()),
        )
        .no_hooks(true);
        let id = StoryService::new(&ctx)
            .create(&NewStoryInput {
                title: "retained integration submission".into(),
                ..Default::default()
            })
            .unwrap()
            .id;
        PrLinkService::new(&ctx)
            .link(&id, "https://github.com/acme/widgets/pull/7", true)
            .unwrap();
        StoryService::new(&ctx)
            .set_state(&id, "verifying", None, None, None)
            .unwrap();
        let candidate = VerificationQueue::new(&store).next().unwrap().unwrap();
        let submission = GateSubmission {
            project,
            story_id: id,
            generation: candidate.verifying_generation,
            submitted_at: candidate.verifying_since.clone(),
        };
        let mut attempt = GateAttempt::new("original-conflict".into(), submission.clone(), AT);
        attempt.control_revision = Some(
            store
                .read(|tx| tx.verification_control_revision(project))
                .unwrap(),
        );
        if settled {
            attempt.finished_at = Some(AT.into());
            attempt.verdict = Some("conflict".into());
        }
        let record = AttributionRecord {
            version: 1,
            id: "original-attribution".into(),
            revision: 0,
            submission,
            attempt: attempt.id.clone(),
            inputs: GateInputs {
                head: Some(native.head.clone()),
                base: Some(native.base.clone()),
                ..Default::default()
            },
            created_at: AT.into(),
            components: vec![FailureComponent {
                id: "integration".into(),
                check: "native-merge".into(),
                signature: "insertions conflict".into(),
                requirement: "both original parents must be preserved".into(),
                log: "retained native merge fixture".into(),
                observed_cause: FailureCause::Integration,
            }],
            preparation: None,
            settlement: None,
            plans: vec![],
            probes: vec![],
            assessments: vec![],
            diagnosis_ms: 0,
            held: true,
            retired: None,
        };
        store
            .write(|tx| {
                tx.insert_gate_attempt(&attempt)?;
                tx.insert_attribution(&record)
            })
            .unwrap();
        Self {
            native,
            fixture,
            store,
            candidate,
        }
    }

    fn ctx(&self) -> Ctx<'_, SqliteStore> {
        Ctx::new(
            &self.store,
            self.candidate.project,
            self.native.root.path(),
            Environment::at(self.fixture.cwd()),
        )
        .no_hooks(true)
    }

    fn proof(&self) -> BoundIntegrationProposal {
        let Inspection::Proposed(proposal) = self.native.inspect() else {
            panic!("real Git conflict was not smoothable")
        };
        // Only the native metadata adapter is substituted by this in-module fixture;
        // the resolution capability comes from real private Git inspection.
        BoundIntegrationProposal {
            proposal,
            deadline: Instant::now() + Duration::from_secs(30),
            cancellation: Cancellation::default(),
            submission: SubmissionObservation {
                checkout: self.candidate.checkout.clone(),
                repository: "github.com/acme/widgets".into(),
                pull_request: "https://github.com/acme/widgets/pull/7".into(),
                base_branch: "dev".into(),
                base: self.native.base.clone(),
                head: self.native.head.clone(),
            },
        }
    }

    fn reserve(&self, proof: &BoundIntegrationProposal) -> crate::store::IntegrationRecovery {
        IntegrationOwnerService::new(&self.ctx())
            .reserve(
                &self.candidate,
                "original-attribution",
                "integration",
                proof,
            )
            .unwrap()
    }
}

#[test]
fn integration_claim_survives_restart_without_replaying_or_rewriting_submission() {
    let f = OwnedFixture::new(true);
    let before = f.native.snapshot();
    let proof = f.proof();
    let record = f.reserve(&proof);
    assert_eq!(
        f.reserve(&proof),
        record,
        "reservation replay created a second owner/comment"
    );
    let reopened = SqliteStore::open(f.store.path()).unwrap();
    let ctx = Ctx::new(
        &reopened,
        f.candidate.project,
        f.native.root.path(),
        Environment::at(f.fixture.cwd()),
    )
    .no_hooks(true);
    let service = IntegrationOwnerService::new(&ctx);
    let claim = service
        .claim_assembly(&record.id, 0, &proof)
        .unwrap()
        .unwrap();
    assert!(service.assembly_permitted(&claim, &proof).unwrap());
    assert!(
        service
            .claim_assembly(&record.id, 0, &proof)
            .unwrap()
            .is_none()
    );
    assert!(
        service
            .claim_assembly(&record.id, 1, &proof)
            .unwrap()
            .is_none(),
        "restart replayed an uncertain external operation"
    );
    let (_, owner) = service.show(&record.id).unwrap();
    assert_eq!(owner.started_at, AT);
    assert_eq!(owner.effect_epoch, 1);
    assert_eq!(
        owner.candidate.verifying_generation,
        f.candidate.verifying_generation
    );
    assert_eq!(owner.phase, IntegrationPhase::Assembling);
    assert!(VerificationQueue::new(&reopened).next().unwrap().is_none());
    proof.settle().unwrap();
    assert_eq!(
        f.native.snapshot(),
        before,
        "reservation or claim changed Git source/ref/object custody"
    );
}

#[test]
fn integration_claim_refuses_unfinished_cleanup_and_manual_control_changes() {
    for mode in [
        "unfinished",
        "manual-off",
        "stopped",
        "label-episode",
        "changed-generation",
    ] {
        let f = OwnedFixture::new(mode != "unfinished");
        let proof = f.proof();
        let record = f.reserve(&proof);
        let ctx = f.ctx();
        match mode {
            "manual-off" => f
                .store
                .write(|tx| {
                    let mut settings = tx.settings(f.candidate.project)?;
                    settings.automations_enabled = Some(false);
                    tx.put_settings(f.candidate.project, &settings)
                })
                .unwrap(),
            "stopped" => f
                .store
                .write(|tx| tx.put_verification_enabled(f.candidate.project, false))
                .unwrap(),
            "label-episode" => {
                let stories = StoryService::new(&ctx);
                stories
                    .set_labels(&f.candidate.story_id, &["no-auto".into()], &[])
                    .unwrap();
                stories
                    .set_labels(&f.candidate.story_id, &[], &["no-auto".into()])
                    .unwrap();
            }
            "changed-generation" => {
                let stories = StoryService::new(&ctx);
                stories
                    .set_state(&f.candidate.story_id, "in-progress", None, None, None)
                    .unwrap();
                stories
                    .set_state(&f.candidate.story_id, "verifying", None, None, None)
                    .unwrap();
            }
            _ => {}
        }
        let service = IntegrationOwnerService::new(&ctx);
        assert!(
            service.claim_assembly(&record.id, 0, &proof).is_err(),
            "admitted {mode}"
        );
        assert_eq!(
            service.show(&record.id).unwrap().0,
            record,
            "refusal consumed or replaced owner for {mode}"
        );
        proof.settle().unwrap();
    }
}

#[test]
fn integration_claim_rechecks_exact_policy_inputs_and_revokes_cached_effect() {
    let f = OwnedFixture::new(true);
    let mut proof = f.proof();
    let record = f.reserve(&proof);
    let ctx = f.ctx();
    let service = IntegrationOwnerService::new(&ctx);
    let original = proof.submission.clone();
    proof.submission.head = "f".repeat(40);
    assert!(service.claim_assembly(&record.id, 0, &proof).is_err());
    proof.submission = original;
    let old_policy = proof.proposal.plan.policy.clone();
    proof.proposal.plan.policy = "e".repeat(64);
    assert!(service.claim_assembly(&record.id, 0, &proof).is_err());
    proof.proposal.plan.policy = old_policy;
    let claim = service
        .claim_assembly(&record.id, 0, &proof)
        .unwrap()
        .unwrap();
    StoryService::new(&ctx)
        .set_awaiting(&f.candidate.story_id, "operator retains original work")
        .unwrap();
    assert!(
        service.assembly_permitted(&claim, &proof).is_err(),
        "cached capability bypassed an operator hold"
    );
    assert_eq!(service.show(&record.id).unwrap().1.effect_epoch, 1);
    proof.settle().unwrap();
}

#[test]
fn integration_store_fences_duplicate_owner_stale_revision_and_identity_replacement() {
    let f = OwnedFixture::new(true);
    let proof = f.proof();
    let record = f.reserve(&proof);
    let mut duplicate = record.clone();
    duplicate.id = uuid::Uuid::new_v4().simple().to_string();
    assert!(
        !f.store
            .write(|tx| tx.insert_integration_recovery(&duplicate))
            .unwrap()
    );
    let mut next = record.clone();
    next.revision = 1;
    next.state["hold"] = serde_json::json!("opaque persistence fixture");
    assert!(
        f.store
            .write(|tx| tx.update_integration_recovery(&next, 0))
            .unwrap()
    );
    assert!(
        !f.store
            .write(|tx| tx.update_integration_recovery(&next, 0))
            .unwrap()
    );
    let mut replaced = next.clone();
    replaced.generation = crate::store::GlobalSeq::new(next.generation.get() + 1);
    replaced.revision = 2;
    assert!(
        !f.store
            .write(|tx| tx.update_integration_recovery(&replaced, 1))
            .unwrap()
    );
    next.revision = 2;
    next.active = false;
    assert!(
        f.store
            .write(|tx| tx.update_integration_recovery(&next, 1))
            .unwrap()
    );
    next.revision = 3;
    next.active = true;
    assert!(
        !f.store
            .write(|tx| tx.update_integration_recovery(&next, 2))
            .unwrap()
    );
    proof.settle().unwrap();
}

#[test]
fn integration_owner_retains_history_when_original_story_is_manually_closed() {
    let f = OwnedFixture::new(true);
    let proof = f.proof();
    let record = f.reserve(&proof);
    let ctx = f.ctx();
    let stories = StoryService::new(&ctx);
    stories
        .set_state(
            &f.candidate.story_id,
            "done",
            Some("manual closure is not native integration landing"),
            None,
            None,
        )
        .unwrap();
    assert!(
        stories
            .delete(&f.candidate.story_id)
            .unwrap_err()
            .to_string()
            .contains("integration recovery")
    );
    let service = IntegrationOwnerService::new(&ctx);
    assert_eq!(service.show(&record.id).unwrap().0, record);
    assert!(service.claim_assembly(&record.id, 0, &proof).is_err());
    proof.settle().unwrap();
}

#[test]
fn integration_owner_rejects_another_pr_with_the_same_pinned_head() {
    let f = OwnedFixture::new(true);
    let mut proof = f.proof();
    let ctx = f.ctx();
    let service = IntegrationOwnerService::new(&ctx);
    proof.submission.pull_request = "https://github.com/acme/widgets/pull/8".into();
    assert!(
        service
            .reserve(&f.candidate, "original-attribution", "integration", &proof)
            .is_err(),
        "same-head PR B capability was used to reserve PR A"
    );
    proof.submission.pull_request = "https://github.com/acme/widgets/pull/7".into();
    let record = f.reserve(&proof);
    proof.submission.repository = "other.example/acme/widgets".into();
    assert!(service.claim_assembly(&record.id, 0, &proof).is_err());
    proof.settle().unwrap();
}

#[test]
fn integration_claim_rejects_same_generation_cleanup_lease_replacement() {
    let f = OwnedFixture::new(true);
    let proof = f.proof();
    let record = f.reserve(&proof);
    let ctx = f.ctx();
    let service = IntegrationOwnerService::new(&ctx);
    let claim = service
        .claim_assembly(&record.id, 0, &proof)
        .unwrap()
        .unwrap();
    f.fixture.append_cleanup_lease(&f.candidate.story_id, serde_json::from_value(serde_json::json!({
        "version":1,"project_slug":f.candidate.project_slug,"story_id":f.candidate.story_id,
        "repository_path":f.native.root.path(),"worktree_path":f.native.root.path().join("replacement"),
        "branch":"replacement-work","tmux":{"socket_path":f.native.root.path().join("fixture-socket"),"revivify":null}
    })).unwrap());
    assert!(
        service.assembly_permitted(&claim, &proof).is_err(),
        "old claim adopted a same-generation replacement resource lease"
    );
    assert_eq!(
        service.show(&record.id).unwrap().1.candidate.cleanup_lease,
        None
    );
    proof.settle().unwrap();
}

#[test]
fn integration_native_claim_cannot_outlive_proposal_deadline_or_cancellation() {
    for cancelled in [false, true] {
        let f = OwnedFixture::new(true);
        let mut proof = f.proof();
        let record = f.reserve(&proof);
        let ctx = f.ctx();
        let service = IntegrationOwnerService::new(&ctx);
        let claim = service
            .claim_assembly(&record.id, 0, &proof)
            .unwrap()
            .unwrap();
        if cancelled {
            proof.cancellation.cancel();
        } else {
            proof.deadline = Instant::now() - Duration::from_secs(1);
        }
        assert!(service.assembly_permitted(&claim, &proof).is_err());
        assert_eq!(
            service.show(&record.id).unwrap().1.effect_epoch,
            1,
            "freshness refusal reset uncertain effect custody"
        );
        proof.settle().unwrap();
    }
}

// Cancel after a real transaction begins, before handing it to the operation.
// No wall-clock sleep or thread scheduling assumption is involved.
struct CancelAtAdmission {
    inner: SqliteStore,
    cancellation: Cancellation,
}
impl Store for CancelAtAdmission {
    fn access(&self) -> crate::store::Access {
        self.inner.access()
    }
    type ReadTx<'a> = <SqliteStore as Store>::ReadTx<'a>;
    type WriteTx<'a> = <SqliteStore as Store>::WriteTx<'a>;
    fn read<T>(
        &self,
        f: impl FnOnce(&Self::ReadTx<'_>) -> Result<T, crate::store::StoreError>,
    ) -> Result<T, crate::store::StoreError> {
        self.inner.read(|tx| {
            self.cancellation.cancel();
            f(tx)
        })
    }
    fn write<T>(
        &self,
        f: impl FnOnce(&mut Self::WriteTx<'_>) -> Result<T, crate::store::StoreError>,
    ) -> Result<T, crate::store::StoreError> {
        self.inner.write(|tx| {
            self.cancellation.cancel();
            f(tx)
        })
    }
    fn try_write<T>(
        &self,
        f: impl FnOnce(&mut Self::WriteTx<'_>) -> Result<T, crate::store::StoreError>,
    ) -> Result<T, crate::store::StoreError> {
        self.inner.try_write(|tx| {
            self.cancellation.cancel();
            f(tx)
        })
    }
    fn migrate(&self) -> Result<crate::store::MigrationReport, crate::store::StoreError> {
        self.inner.migrate()
    }
    fn change_token(&self) -> Result<u64, crate::store::StoreError> {
        self.inner.change_token()
    }
    fn snapshot(
        &self,
        dir: &Path,
        label: &str,
    ) -> Result<std::path::PathBuf, crate::store::StoreError> {
        self.inner.snapshot(dir, label)
    }
    fn write_with_snapshot<T>(
        &self,
        dir: &Path,
        label: &str,
        f: impl FnOnce(&mut Self::WriteTx<'_>) -> Result<T, crate::store::StoreError>,
    ) -> Result<crate::store::WriteWithSnapshot<T>, crate::store::StoreError> {
        self.inner.write_with_snapshot(dir, label, |tx| {
            self.cancellation.cancel();
            f(tx)
        })
    }
}

#[test]
fn integration_claim_rechecks_cancellation_after_transaction_admission() {
    for effect in [false, true] {
        let f = OwnedFixture::new(true);
        let proof = f.proof();
        let record = f.reserve(&proof);
        let normal_ctx = f.ctx();
        let normal = IntegrationOwnerService::new(&normal_ctx);
        let claim = if effect {
            normal.claim_assembly(&record.id, 0, &proof).unwrap()
        } else {
            None
        };
        let before = normal.show(&record.id).unwrap().0;
        let interrupted = CancelAtAdmission {
            inner: SqliteStore::open(f.store.path()).unwrap(),
            cancellation: proof.cancellation.clone(),
        };
        let ctx = Ctx::new(
            &interrupted,
            f.candidate.project,
            f.native.root.path(),
            Environment::at(f.fixture.cwd()),
        )
        .no_hooks(true);
        let service = IntegrationOwnerService::new(&ctx);
        assert!(!proof.cancellation.is_cancelled());
        if let Some(claim) = claim {
            assert!(
                service.assembly_permitted(&claim, &proof).is_err(),
                "read admission renewed a cancelled effect capability"
            );
        } else {
            assert!(
                service.claim_assembly(&record.id, 0, &proof).is_err(),
                "write admission minted a cancelled claim"
            );
        }
        assert_eq!(normal.show(&record.id).unwrap().0, before);
        proof.settle().unwrap();
    }
}
