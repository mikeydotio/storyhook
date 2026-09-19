use super::*;
use storyhook::service::project_recovery::{DecisionInput, RecoveryView, RepairScope, RepairSpec};

pub(super) fn ready(f: &ServiceFixture) -> RecoveryView {
    let candidate = submitted(f, "scope assessor");
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let view = service
        .observe(&candidate, &fault(), "scope-attempt")
        .unwrap()
        .unwrap();
    let claimed = service.claim_assessment(&view.record.id).unwrap().unwrap();
    service
        .settle_assessment(
            &view.record.id,
            &claimed.state.assessment.dispatch_identity,
            claimed.state.assessment.epoch,
            AssessmentDelivery::Delivered,
        )
        .unwrap()
}

pub(super) fn input(view: &RecoveryView, scope: RepairScope) -> DecisionInput {
    DecisionInput {
        version: 1, revision: view.record.revision, project: view.record.project,
        generation: view.state.assessment.generation,
        dispatch_identity: view.state.assessment.dispatch_identity.clone(), scope,
        context: "The successful project gate omitted its required certificate.".into(),
        question: "Which story owns the gate repair?".into(),
        decision: format!("Use {scope:?} recovery."),
        rationale: "Source inspection shows the gate fault is in the registered project.".into(),
        evidence: vec!["attempt:scope-attempt".into(), "scripts/gate.sh:12".into()],
        repair: (scope == RepairScope::SeparateStory).then(|| RepairSpec {
            title: "Repair gate certification".into(), description: "Restore certification after required tests.".into(),
            acceptance: "Regression fails before repair; required suite produces a valid receipt after repair.".into(),
        }),
        prerequisite: (scope == RepairScope::External).then(|| "The owner must restore access to the signing service.".into()),
    }
}

#[test]
fn same_story_decision_replays_exactly_without_creating_a_second_story() {
    let f = fixture();
    let view = ready(&f);
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let request = input(&view, RepairScope::SameStory);
    let accepted = service.decide(&view.record.id, &request).unwrap();
    assert_eq!(accepted.state.assessment.status, AssessmentStatus::Decided);
    let receipt = accepted.state.decision.as_ref().unwrap();
    assert_eq!(receipt.repair_story, Some(StoryNo::new(1)));
    assert!(receipt.delivery_identity.is_some());
    assert!(receipt.owned_edges.is_empty());
    assert_eq!(service.decide(&view.record.id, &request).unwrap(), accepted);
    let mut conflicting = request;
    conflicting.rationale.push_str(" changed");
    assert!(service.decide(&view.record.id, &conflicting).is_err());
    assert_eq!(
        f.store()
            .read(|tx| tx.stories(f.project(), &Default::default()))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn separate_decision_creates_one_critical_bug_with_atomic_reciprocal_edges() {
    let f = fixture();
    let initial = ready(&f);
    let joined = submitted(&f, "another affected submission");
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let view = service
        .observe(&joined, &fault(), "joined-attempt")
        .unwrap()
        .unwrap();
    let stale = input(&initial, RepairScope::SeparateStory);
    assert!(service.decide(&view.record.id, &stale).is_err());
    let request = input(&view, RepairScope::SeparateStory);
    let accepted = service.decide(&view.record.id, &request).unwrap();
    let receipt = accepted.state.decision.as_ref().unwrap();
    assert_eq!(receipt.repair_story, Some(StoryNo::new(3)));
    assert_eq!(receipt.owned_edges, vec![StoryNo::new(1), StoryNo::new(2)]);
    let repair = f
        .store()
        .read(|tx| tx.story(f.project(), StoryNo::new(3)))
        .unwrap()
        .unwrap();
    assert_eq!(
        repair.snapshot.priority,
        storyhook::domain::Priority::Critical
    );
    assert_eq!(repair.snapshot.story_type.as_deref(), Some("bug"));
    assert!(
        repair
            .snapshot
            .description
            .as_ref()
            .unwrap()
            .contains("Acceptance")
    );
    for n in [1, 2] {
        let row = f
            .store()
            .read(|tx| tx.story(f.project(), StoryNo::new(n)))
            .unwrap()
            .unwrap();
        assert!(
            row.snapshot
                .relationships
                .iter()
                .any(|r| r.relation == "blocked-by" && r.other_id == repair.snapshot.id)
        );
        assert!(
            repair
                .snapshot
                .relationships
                .iter()
                .any(|r| r.relation == "blocks" && r.other_id == row.snapshot.id)
        );
        assert!(row.awaiting.is_none());
    }
    assert_eq!(service.decide(&view.record.id, &request).unwrap(), accepted);
    assert_eq!(
        f.store()
            .read(|tx| tx.stories(f.project(), &Default::default()))
            .unwrap()
            .len(),
        3
    );
}

#[test]
fn external_decision_sets_contextual_holds_without_dispatch_or_repair() {
    let f = fixture();
    let view = ready(&f);
    let ctx = f.ctx();
    let request = input(&view, RepairScope::External);
    let accepted = ProjectRecoveryService::new(&ctx)
        .decide(&view.record.id, &request)
        .unwrap();
    let receipt = accepted.state.decision.unwrap();
    assert!(receipt.repair_story.is_none());
    assert!(receipt.delivery_identity.is_none());
    let row = f
        .store()
        .read(|tx| tx.story(f.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert!(
        row.awaiting
            .unwrap()
            .contains(request.prerequisite.as_ref().unwrap())
    );
    assert!(
        row.snapshot
            .comments
            .iter()
            .any(|c| c.text.contains("Context:") && c.text.contains("Rationale:"))
    );
}

#[test]
fn malformed_or_foreign_decisions_leave_no_partial_work() {
    let f = fixture();
    let view = ready(&f);
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let request = input(&view, RepairScope::SeparateStory);
    let mut invalid = Vec::new();
    let mut x = request.clone();
    x.version = 2;
    invalid.push(x);
    let mut x = request.clone();
    x.dispatch_identity = "foreign".into();
    invalid.push(x);
    let mut x = request.clone();
    x.generation = storyhook::store::GlobalSeq::new(999999);
    invalid.push(x);
    let mut x = request.clone();
    x.project = storyhook::store::ProjectId::new(999999);
    invalid.push(x);
    let mut x = request.clone();
    x.context = " ".into();
    invalid.push(x);
    let mut x = request.clone();
    x.evidence = vec!["unrelated".into()];
    invalid.push(x);
    let mut x = request.clone();
    x.repair.as_mut().unwrap().acceptance.clear();
    invalid.push(x);
    let mut x = request.clone();
    x.prerequisite = Some("not separate work".into());
    invalid.push(x);
    for x in invalid {
        assert!(service.decide(&view.record.id, &x).is_err());
        assert_eq!(service.show(&view.record.id).unwrap(), view);
    }
    assert_eq!(
        f.store()
            .read(|tx| tx.stories(f.project(), &Default::default()))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn changed_origin_authority_refuses_even_a_well_formed_decision() {
    for hold in ["stop", "label", "state", "awaiting"] {
        let f = fixture();
        let view = ready(&f);
        let ctx = f.ctx();
        match hold {
            "stop" => {
                f.store()
                    .write(|tx| tx.put_verification_enabled(f.project(), false))
                    .unwrap();
            }
            "label" => {
                StoryService::new(&ctx)
                    .set_labels("SH-1", &["no-auto".into()], &[])
                    .unwrap();
            }
            "state" => {
                StoryService::new(&ctx)
                    .set_state("SH-1", "todo", None, None, None)
                    .unwrap();
            }
            _ => {
                StoryService::new(&ctx)
                    .set_awaiting("SH-1", "independent hold")
                    .unwrap();
            }
        }
        let service = ProjectRecoveryService::new(&ctx);
        assert!(
            service
                .decide(&view.record.id, &input(&view, RepairScope::SeparateStory))
                .is_err(),
            "{hold}"
        );
        assert_eq!(service.show(&view.record.id).unwrap(), view);
    }
}

#[test]
fn decision_during_delivery_proves_receipt_and_late_confirmation_is_idempotent() {
    let f = fixture();
    let candidate = submitted(&f, "fast assessor");
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let view = service
        .observe(&candidate, &fault(), "scope-attempt")
        .unwrap()
        .unwrap();
    assert!(
        service
            .decide(&view.record.id, &input(&view, RepairScope::SameStory))
            .is_err()
    );
    let claimed = service.claim_assessment(&view.record.id).unwrap().unwrap();
    let accepted = service
        .decide(&view.record.id, &input(&claimed, RepairScope::SameStory))
        .unwrap();
    assert_eq!(
        service
            .settle_assessment(
                &view.record.id,
                &claimed.state.assessment.dispatch_identity,
                claimed.state.assessment.epoch,
                AssessmentDelivery::Delivered
            )
            .unwrap(),
        accepted
    );
    assert!(service.claim_assessment(&view.record.id).unwrap().is_none());
}

#[test]
fn changed_joined_subject_is_preserved_without_stalling_origin_repair() {
    let f = fixture();
    let _origin = ready(&f);
    let joined = submitted(&f, "reserved joiner");
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let view = service
        .observe(&joined, &fault(), "joined-attempt")
        .unwrap()
        .unwrap();
    StoryService::new(&ctx)
        .set_awaiting(&joined.story_id, "unrelated operator hold")
        .unwrap();
    let before = f
        .store()
        .read(|tx| tx.story(f.project(), StoryNo::new(2)))
        .unwrap();
    let accepted = service
        .decide(&view.record.id, &input(&view, RepairScope::SeparateStory))
        .unwrap();
    let receipt = accepted.state.decision.unwrap();
    assert_eq!(receipt.skipped_subjects, vec![StoryNo::new(2)]);
    assert_eq!(receipt.owned_edges, vec![StoryNo::new(1)]);
    assert_eq!(
        f.store()
            .read(|tx| tx.story(f.project(), StoryNo::new(2)))
            .unwrap(),
        before
    );
}

#[test]
fn same_story_repair_blocks_joined_subject_without_a_self_dependency() {
    let f = fixture();
    let _origin = ready(&f);
    let joined = submitted(&f, "shared source fault");
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let view = service
        .observe(&joined, &fault(), "joined-attempt")
        .unwrap()
        .unwrap();
    let accepted = service
        .decide(&view.record.id, &input(&view, RepairScope::SameStory))
        .unwrap();
    let receipt = accepted.state.decision.unwrap();
    assert_eq!(receipt.owned_edges, vec![StoryNo::new(2)]);
    let owner = f
        .store()
        .read(|tx| tx.story(f.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert!(
        owner
            .snapshot
            .relationships
            .iter()
            .all(|r| r.other_id != owner.snapshot.id)
    );
}

#[test]
fn late_observation_joins_the_existing_repair_without_another_assessment() {
    let f = fixture();
    let view = ready(&f);
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let accepted = service
        .decide(&view.record.id, &input(&view, RepairScope::SeparateStory))
        .unwrap();
    let late = submitted(&f, "late affected submission");
    let joined = service
        .observe(&late, &fault(), "late-attempt")
        .unwrap()
        .unwrap();
    assert_eq!(joined.record.id, view.record.id);
    assert_eq!(joined.state.assessment, accepted.state.assessment);
    let receipt = joined.state.decision.unwrap();
    assert_eq!(receipt.owned_edges, vec![StoryNo::new(1), StoryNo::new(3)]);
    let row = f
        .store()
        .read(|tx| tx.story(f.project(), StoryNo::new(3)))
        .unwrap()
        .unwrap();
    assert!(
        row.snapshot
            .relationships
            .iter()
            .any(|r| r.relation == "blocked-by" && r.other_id == "SH-2")
    );
}

#[test]
fn cli_and_rpc_share_strict_decision_input_and_recovery_output() {
    use storyhook::{cli::parse_invocation, invoke::dispatch, output::render_response};
    let f = fixture();
    let view = ready(&f);
    let ctx = f.ctx();
    let file = f.ctx().cwd().join("decision.json");
    std::fs::write(
        &file,
        serde_json::to_vec(&input(&view, RepairScope::SameStory)).unwrap(),
    )
    .unwrap();
    let args: Vec<String> = [
        "verifier",
        "repair",
        "decide",
        &view.record.id,
        "--input",
        file.to_str().unwrap(),
    ]
    .map(str::to_owned)
    .into();
    let command = parse_invocation(&args).unwrap();
    let wire = serde_json::to_vec(&command).unwrap();
    let response = dispatch(&ctx, serde_json::from_slice(&wire).unwrap()).unwrap();
    let rendered = render_response(&response, true, false);
    assert!(rendered.contains("same-story"));
    assert!(rendered.contains("repair_story"));
    let response_wire = serde_json::to_vec(&response).unwrap();
    assert_eq!(
        render_response(
            &serde_json::from_slice(&response_wire).unwrap(),
            true,
            false
        ),
        rendered
    );
    let args: Vec<String> = ["verifier", "repair", "show", &view.record.id]
        .map(str::to_owned)
        .into();
    let shown = dispatch(&ctx, parse_invocation(&args).unwrap()).unwrap();
    assert_eq!(render_response(&shown, true, false), rendered);
    for suffix in [
        vec!["decide", "id"],
        vec!["show", "id", "extra"],
        vec!["decide", "id", "--input", "x", "extra"],
    ] {
        let args = [vec!["verifier", "repair"], suffix]
            .concat()
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert!(parse_invocation(&args).is_err());
    }
}

#[test]
fn dependency_write_failure_rolls_back_the_repair_decision_and_allocated_story() {
    let f = fixture();
    let view = ready(&f);
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let connection = rusqlite::Connection::open(f.store().path()).unwrap();
    connection.execute_batch("CREATE TRIGGER refuse_recovery_edge BEFORE INSERT ON story_relations BEGIN SELECT RAISE(ABORT, 'injected edge failure'); END;").unwrap();
    assert!(
        service
            .decide(&view.record.id, &input(&view, RepairScope::SeparateStory))
            .is_err()
    );
    assert_eq!(service.show(&view.record.id).unwrap(), view);
    assert_eq!(
        f.store()
            .read(|tx| tx.stories(f.project(), &Default::default()))
            .unwrap()
            .len(),
        1
    );
    connection
        .execute_batch("DROP TRIGGER refuse_recovery_edge;")
        .unwrap();
    let accepted = service
        .decide(&view.record.id, &input(&view, RepairScope::SeparateStory))
        .unwrap();
    assert_eq!(
        accepted.state.decision.unwrap().repair_story,
        Some(StoryNo::new(2))
    );
}

#[test]
fn concurrent_scope_decisions_have_one_winner_and_survive_restart() {
    let f = fixture();
    let view = ready(&f);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let handles = [RepairScope::SameStory, RepairScope::SeparateStory].map(|scope| {
        let path = f.store().path().to_path_buf();
        let env = f.env().clone();
        let project = f.project();
        let cwd = f.ctx().cwd().to_path_buf();
        let id = view.record.id.clone();
        let request = input(&view, scope);
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            let store = storyhook::store::SqliteStore::open(&path).unwrap();
            let ctx = storyhook::service::Ctx::new(&store, project, cwd, env).clock(
                storyhook::service::Clock::Fixed("2026-01-01T00:01:00Z".into()),
            );
            barrier.wait();
            ProjectRecoveryService::new(&ctx).decide(&id, &request)
        })
    });
    let results = handles.map(|h| h.join().unwrap());
    assert_eq!(
        results.iter().filter(|r| r.is_ok()).count(),
        1,
        "{results:?}"
    );
    let accepted = results.into_iter().find_map(Result::ok).unwrap();
    let reopened = storyhook::store::SqliteStore::open(f.store().path()).unwrap();
    let ctx = storyhook::service::Ctx::new(
        &reopened,
        f.project(),
        f.ctx().cwd().to_path_buf(),
        f.env().clone(),
    );
    let service = ProjectRecoveryService::new(&ctx);
    assert_eq!(service.show(&view.record.id).unwrap(), accepted);
    assert_eq!(
        service
            .decide(
                &view.record.id,
                &accepted.state.decision.as_ref().unwrap().input
            )
            .unwrap(),
        accepted
    );
}
