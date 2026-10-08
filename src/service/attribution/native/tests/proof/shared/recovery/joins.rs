//! Distinct native evidence coordinates only through a claimed semantic decision.
use super::*;

fn distinct_faults(inflight: bool) -> (Fixture, Evidence, RecoveryView, RecoveryView) {
    let (mut f, evidence, first) = fixture(false);
    let ctx = context(&evidence);
    let service = ProjectRecoveryService::new(&ctx);
    let stories = StoryService::new(&ctx);
    let id = stories
        .create(&NewStoryInput {
            title: "distinct shared submission".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    PrLinkService::new(&ctx)
        .link(&id, "https://github.com/acme/widgets/pull/2", true)
        .unwrap();
    stories
        .set_state(&id, "verifying", None, None, None)
        .unwrap();
    let candidate = VerificationQueue::new(&evidence.store)
        .ordered_for(ctx.project())
        .unwrap()
        .into_iter()
        .find(|c| c.story_id == id)
        .unwrap();
    let mut first = service
        .observe_shared(&evidence.candidate, &first)
        .unwrap()
        .unwrap();
    if inflight {
        first = owner(&service, &first);
        first = service
            .claim_work(&first.record.id, &first.state.work[0].id)
            .unwrap()
            .unwrap();
    }
    f.base = f.git(&["rev-parse", "HEAD"]);
    f.write("README.md", "another head on a different failing base\n");
    f.git(&["add", "README.md"]);
    f.git(&["commit", "-qm", "different shared candidate"]);
    let (settled, record) = settled_named(
        &evidence,
        &f,
        false,
        &candidate,
        "distinct-attempt",
        "distinct-attribution",
    );
    let proof = evidence
        .store
        .read(|tx| settled.prove_shared(tx, &candidate, &record.id, "original"))
        .unwrap();
    let second = service.observe_shared(&candidate, &proof).unwrap().unwrap();
    assert_ne!(first.record.locus, second.record.locus);
    (f, evidence, first, second)
}

fn owner(service: &ProjectRecoveryService<'_, SqliteStore>, view: &RecoveryView) -> RecoveryView {
    let claimed = service.claim_assessment(&view.record.id).unwrap().unwrap();
    service
        .decide(
            &view.record.id,
            &decision(&claimed, RepairScope::SeparateStory),
        )
        .unwrap()
}

fn join_input(follower: &RecoveryView, owner: &RecoveryView) -> DecisionInput {
    let mut input = decision(follower, RepairScope::SeparateStory);
    input.version = 2;
    input.repair = None;
    input.join_recovery = Some(JoinRepair {
        recovery: owner.record.id.clone(),
        revision: owner.record.revision,
    });
    input.decision =
        "The existing repair addresses both separately retained native failures".into();
    input.rationale =
        "Preserve both detector obligations under one managed repair and aggregate attempt budget"
            .into();
    input
}

fn submit(evidence: &Evidence, owner: &RecoveryView) -> VerificationCandidate {
    let ctx = context(evidence);
    let id = owner
        .state
        .decision
        .as_ref()
        .unwrap()
        .repair_story
        .unwrap()
        .to_id("SH");
    PrLinkService::new(&ctx)
        .link(&id, "https://github.com/acme/widgets/pull/3", true)
        .unwrap();
    StoryService::new(&ctx)
        .set_state(&id, "verifying", None, None, None)
        .unwrap();
    VerificationQueue::new(&evidence.store)
        .ordered_for(ctx.project())
        .unwrap()
        .into_iter()
        .find(|c| c.story_id == id)
        .unwrap()
}

fn input(index: usize) -> RepairInput {
    RepairInput {
        base: "a".repeat(40),
        head: format!("{index:040x}"),
        head_tree: format!("{:040x}", index + 10),
        tree: format!("{:040x}", index + 20),
    }
}

#[test]
fn distinct_faults_share_one_explicit_repair_and_cumulative_budget_after_restart() {
    let (_f, evidence, first, second) = distinct_faults(false);
    let ctx = context(&evidence);
    let service = ProjectRecoveryService::new(&ctx);
    let owner = owner(&service, &first);
    assert!(
        service
            .claim_work(&owner.record.id, &owner.state.work[0].id)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        service.show(&owner.record.id).unwrap(),
        owner,
        "a temporary second-fault pause minted a hold that prevents its join"
    );
    let claimed = service
        .claim_assessment(&second.record.id)
        .unwrap()
        .unwrap();
    let joined = service
        .decide(&second.record.id, &join_input(&claimed, &owner))
        .unwrap();
    assert_eq!(
        joined.state.decision.as_ref().unwrap().repair_story,
        owner.state.decision.as_ref().unwrap().repair_story
    );
    assert_ne!(joined.record.locus, owner.record.locus);
    assert!(joined.state.work.is_empty());
    assert!(
        joined
            .state
            .decision
            .as_ref()
            .unwrap()
            .delivery_identity
            .is_none()
    );
    assert_eq!(
        service
            .decide(&second.record.id, &join_input(&claimed, &owner))
            .unwrap(),
        joined
    );
    let candidate = submit(&evidence, &owner);
    for index in 1..=3 {
        let pinned = input(index);
        let attempt = format!("coordinated-{index}");
        assert!(
            matches!(service.admit_repair(&candidate, &attempt, &pinned).unwrap(), RepairAdmission::Proceed { recovery_id: Some(id) } if id == owner.record.id)
        );
        service
            .complete_repair(
                &candidate,
                &attempt,
                &RepairJudgment::TestsFailed { tree: pinned.tree },
            )
            .unwrap();
    }
    let reopened = SqliteStore::open(evidence.store.path()).unwrap();
    let reopened_ctx = Ctx::new(
        &reopened,
        ctx.project(),
        evidence.fixture.cwd(),
        Environment::at(evidence.fixture.cwd()).with_subprocess_patience(),
    )
    .no_hooks(true);
    let service = ProjectRecoveryService::new(&reopened_ctx);
    assert!(matches!(
        service
            .admit_repair(&candidate, "fourth-coordinated", &input(4))
            .unwrap(),
        RepairAdmission::Deferred {
            reason: RepairRefusal::BudgetExhausted,
            ..
        }
    ));
    let status = reopened
        .read(|tx| crate::service::project_recovery::status_snapshot(tx, ctx.project()))
        .unwrap();
    assert_eq!(status.len(), 2);
    assert!(status.iter().all(|row| row.completed_attempts == 3));
    assert_eq!(service.show(&first.record.id).unwrap().state.work.len(), 1);
    assert!(
        service
            .show(&second.record.id)
            .unwrap()
            .state
            .work
            .is_empty()
    );
}

#[test]
fn coordinated_faults_release_only_after_exact_certified_landing() {
    let (_f, evidence, first, second) = distinct_faults(false);
    let ctx = context(&evidence);
    let service = ProjectRecoveryService::new(&ctx);
    let owner = owner(&service, &first);
    let claimed = service
        .claim_assessment(&second.record.id)
        .unwrap()
        .unwrap();
    service
        .decide(&second.record.id, &join_input(&claimed, &owner))
        .unwrap();
    let candidate = submit(&evidence, &owner);
    let pinned = input(1);
    service
        .admit_repair(&candidate, "coordinated-certified", &pinned)
        .unwrap();
    service
        .complete_repair(
            &candidate,
            "coordinated-certified",
            &RepairJudgment::Certified {
                head: pinned.head.clone(),
                tree: pinned.tree.clone(),
            },
        )
        .unwrap();
    for id in [&first.record.id, &second.record.id] {
        assert!(service.show(id).unwrap().record.active);
        assert!(!service.landing_release_ready(id).unwrap());
    }
    let queue = VerificationQueue::new(&evidence.store);
    let cert = crate::service::landing::VerifiedSubmission {
        head: pinned.head,
        tree: pinned.tree,
        gate: "make test".into(),
    };
    let crate::service::landing::LandingAdmission::Admitted(intent) =
        queue.begin_landing(&ctx, &candidate, &cert).unwrap()
    else {
        panic!("expected ordinary landing admission")
    };
    assert!(
        queue
            .complete_landing(&ctx, &intent, "confirmed fixture merge")
            .unwrap()
    );
    let first_landed = service.show(&first.record.id).unwrap();
    let second_landed = service.show(&second.record.id).unwrap();
    assert_eq!(first_landed.state.landing, second_landed.state.landing);
    assert!(!first_landed.record.active && !second_landed.record.active);
    for id in [&first.record.id, &second.record.id] {
        assert!(service.landing_release_ready(id).unwrap());
        let released = service.reconcile_landing(id).unwrap();
        assert_eq!(released.state.shared.unwrap().readmissions.len(), 1);
    }
    assert_eq!(queue.ordered_for(ctx.project()).unwrap().len(), 2);
}

#[test]
fn repair_join_rejects_stale_decisions_uncertain_delivery_and_cyclic_owner() {
    let (_f, evidence, first, second) = distinct_faults(false);
    let ctx = context(&evidence);
    let service = ProjectRecoveryService::new(&ctx);
    let owner = owner(&service, &first);
    let claimed = service
        .claim_assessment(&second.record.id)
        .unwrap()
        .unwrap();
    let mut stale = join_input(&claimed, &owner);
    stale.join_recovery.as_mut().unwrap().revision -= 1;
    assert!(service.decide(&second.record.id, &stale).is_err());
    let mut self_join = join_input(&claimed, &owner);
    self_join.join_recovery = Some(JoinRepair {
        recovery: second.record.id.clone(),
        revision: claimed.record.revision,
    });
    assert!(service.decide(&second.record.id, &self_join).is_err());
    // Corrupting a pointer into a cycle must fail closed without recursive reads.
    evidence
        .store
        .write(|tx| {
            let mut record = tx
                .project_recoveries(ctx.project())?
                .into_iter()
                .find(|r| r.id == first.record.id)
                .unwrap();
            let revision = record.revision;
            record.revision += 1;
            record.state["decision"]["input"]["version"] = serde_json::json!(2);
            record.state["decision"]["input"]["repair"] = serde_json::Value::Null;
            record.state["decision"]["input"]["join_recovery"] =
                serde_json::json!({"recovery": first.record.id, "revision": revision});
            record.state["decision"]["delivery_identity"] = serde_json::Value::Null;
            assert!(tx.update_project_recovery(&record, revision)?);
            Ok(())
        })
        .unwrap();
    assert!(service.show(&first.record.id).is_err());
    assert!(
        service
            .show(&second.record.id)
            .unwrap()
            .state
            .decision
            .is_none()
    );

    let (_f, evidence, owner, second) = distinct_faults(true);
    let ctx = context(&evidence);
    let service = ProjectRecoveryService::new(&ctx);
    let claimed = service
        .claim_assessment(&second.record.id)
        .unwrap()
        .unwrap();
    assert!(
        service
            .decide(&second.record.id, &join_input(&claimed, &owner))
            .is_err()
    );
    let work = &owner.state.work[0];
    let uncertain = service
        .settle_work(
            &owner.record.id,
            &work.id,
            work.epoch,
            AssessmentDelivery::Uncertain("provider result lost; ownership unresolved".into()),
        )
        .unwrap();
    assert!(
        service
            .decide(&second.record.id, &join_input(&claimed, &uncertain))
            .is_err()
    );
    assert!(
        service
            .show(&second.record.id)
            .unwrap()
            .state
            .work
            .is_empty()
    );
    assert!(
        service
            .show(&second.record.id)
            .unwrap()
            .state
            .decision
            .is_none()
    );
}
