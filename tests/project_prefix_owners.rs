//! SH-853: a supported rename must never strand live canonical-ID owners.
use storyhook::service::engine::{EngineService, StartRequest};
use storyhook::service::story_reset::StoryResetService;
use storyhook::service::{Ctx, NewStoryInput, ProjectService, StoryService};
use storyhook::store::*;
use storyhook_test_support::{
    DispatcherStep, FIXTURE_NOW, FakeDispatcher, ServiceFixture, scratch_dir,
};

fn story(f: &ServiceFixture, state: &str) -> String {
    StoryService::new(&f.ctx().no_hooks(true))
        .create(&NewStoryInput {
            title: "Prefix owner".into(),
            state: Some(state.into()),
            ..Default::default()
        })
        .unwrap()
        .id
}

fn start(ctx: &Ctx<'_, SqliteStore>, scope: EngineScope) -> EngineRunRecord {
    EngineService::new(ctx, &FakeDispatcher::default())
        .start(StartRequest {
            scope,
            lanes: 1,
            agent: EngineAgent::Codex,
            model: None,
            effort: None,
            speed: None,
        })
        .unwrap()
}

fn projection(f: &ServiceFixture) -> serde_json::Value {
    f.store()
        .read(|tx| {
            Ok(serde_json::json!({
                "prefix":tx.project(f.project())?.unwrap().prefix,
                "stories":tx.stories(f.project(), &StoryQuery::all())?.into_iter().map(|row|
                    (row.head_seq, row.snapshot)).collect::<Vec<_>>()
            }))
        })
        .unwrap()
}

fn refused(f: &ServiceFixture, owner: &str) {
    let before = projection(f);
    let service = ProjectService::new(f.store(), f.cwd());
    let preview = service
        .set_prefix_plan(f.project(), "NW")
        .unwrap_err()
        .to_string();
    let backups = scratch_dir();
    let write = service
        .set_prefix(f.project(), "NW", backups.path())
        .unwrap_err()
        .to_string();
    assert!(preview.contains(owner), "{preview}");
    assert_eq!(
        write, preview,
        "preview and final write must identify the same owner"
    );
    assert_eq!(
        projection(f),
        before,
        "refusal must not rename or refold stories"
    );
}

#[test]
fn sh853_working_lane_refuses_rename_and_is_not_quarantined_as_missing() {
    let f = ServiceFixture::new();
    let id = story(&f, "in-progress");
    let ctx = f.ctx().no_hooks(true);
    let run = start(&ctx, EngineScope::Project);
    f.store()
        .write(|tx| {
            let mut lane = tx.engine_lanes(&run.id)?.remove(0);
            lane.state = EngineLaneState::Working;
            lane.story_id = Some(id.clone());
            lane.window_name = Some(id.clone());
            tx.put_engine_lane(&lane)
        })
        .unwrap();
    refused(&f, &run.id);
    let dispatcher = FakeDispatcher::new([DispatcherStep::WindowAlive {
        window: "=fixture:=SH-1".into(),
        alive: true,
    }]);
    let outcome = EngineService::new(&ctx, &dispatcher)
        .reconcile_after_restart(&run.id)
        .unwrap();
    assert!(outcome.quarantined.is_empty(), "{:?}", outcome.quarantined);
    let lane = f
        .store()
        .read(|tx| tx.engine_lanes(&run.id))
        .unwrap()
        .remove(0);
    assert_eq!(lane.state, EngineLaneState::Working);
    assert_eq!(lane.story_id.as_deref(), Some(id.as_str()));
    assert_ne!(lane.outcome.as_deref(), Some("story-missing"));
}

#[test]
fn sh853_idle_epic_runs_keep_their_prefix_in_every_resumable_state() {
    for state in [
        EngineRunState::Running,
        EngineRunState::Paused,
        EngineRunState::Draining,
        EngineRunState::Halted,
    ] {
        let f = ServiceFixture::new();
        f.store()
            .write(|tx| {
                let mut types = tx.types(f.project())?;
                types.push(storyhook::domain::TypeDef {
                    slug: "epic".into(),
                    description: None,
                    emoji: None,
                });
                tx.put_types(f.project(), &types)
            })
            .unwrap();
        let ctx = f.ctx().no_hooks(true);
        let epic = StoryService::new(&ctx)
            .create(&NewStoryInput {
                title: "Retained epic scope".into(),
                story_type: Some("epic".into()),
                ..Default::default()
            })
            .unwrap();
        let run = start(&ctx, EngineScope::Epic(epic.id.clone()));
        f.store()
            .write(|tx| {
                let mut current = tx.engine_run(&run.id)?.unwrap();
                current.state = state;
                tx.update_engine_run(&current)
            })
            .unwrap();
        refused(&f, &run.id);
        f.store()
            .write(|tx| {
                let mut current = tx.engine_run(&run.id)?.unwrap();
                current.state = EngineRunState::Finished;
                tx.update_engine_run(&current)
            })
            .unwrap();
        ProjectService::new(f.store(), f.cwd())
            .set_prefix(f.project(), "NW", scratch_dir().path())
            .unwrap();
        assert_eq!(
            f.store()
                .read(|tx| tx.engine_run(&run.id))
                .unwrap()
                .unwrap()
                .scope,
            EngineScope::Epic(epic.id)
        );
    }
}

#[test]
fn sh853_final_write_rechecks_a_run_admitted_after_preview() {
    let f = ServiceFixture::new();
    story(&f, "todo");
    let service = ProjectService::new(f.store(), f.cwd());
    service.set_prefix_plan(f.project(), "NW").unwrap();
    let separate = SqliteStore::open(f.store().path()).unwrap();
    let ctx = Ctx::new(&separate, f.project(), f.cwd(), f.env().clone()).no_hooks(true);
    let run = start(&ctx, EngineScope::Project);
    let before = projection(&f);
    let error = service
        .set_prefix(f.project(), "NW", scratch_dir().path())
        .unwrap_err();
    assert!(error.to_string().contains(&run.id), "{error}");
    assert_eq!(projection(&f), before);
    assert_eq!(
        separate
            .read(|tx| tx.engine_run(&run.id))
            .unwrap()
            .unwrap()
            .state,
        EngineRunState::Running
    );
}

#[test]
fn sh853_other_projects_runs_and_finished_history_do_not_block_rename() {
    let f = ServiceFixture::new();
    story(&f, "todo");
    let other = f.add_project("other", "OT");
    let ctx = Ctx::new(f.store(), other, f.cwd(), f.env().clone()).no_hooks(true);
    let run = start(&ctx, EngineScope::Project);
    let own = start(&f.ctx().no_hooks(true), EngineScope::Project);
    f.store()
        .write(|tx| {
            let mut row = tx.engine_run(&own.id)?.unwrap();
            row.state = EngineRunState::Finished;
            tx.update_engine_run(&row)
        })
        .unwrap();
    ProjectService::new(f.store(), f.cwd())
        .set_prefix(f.project(), "NW", scratch_dir().path())
        .unwrap();
    assert_eq!(
        f.store()
            .read(|tx| tx.engine_run(&run.id))
            .unwrap()
            .unwrap()
            .state,
        EngineRunState::Running
    );
    assert_eq!(
        f.store()
            .read(|tx| tx.project(other))
            .unwrap()
            .unwrap()
            .prefix,
        "OT"
    );
}

#[test]
fn sh853_native_and_legacy_pending_resets_block_but_completed_receipts_do_not() {
    for legacy in [false, true] {
        let f = ServiceFixture::new();
        let id = story(&f, "in-progress");
        let ctx = f.ctx().no_hooks(true);
        if legacy {
            f.store()
                .write(|tx| {
                    tx.put_legacy_story_reset(
                        f.project(),
                        StoryNo::new(1),
                        Some(r#"{"operation":"legacy-owner"}"#),
                    )
                })
                .unwrap();
            refused(&f, "legacy-owner");
        } else {
            let reset = StoryResetService::new(&ctx).reserve(&id, &id).unwrap();
            refused(&f, &reset.token);
            let mut completed = reset.clone();
            completed.completed = true;
            f.store()
                .write(|tx| tx.put_story_reset(&completed))
                .unwrap();
            ProjectService::new(f.store(), f.cwd())
                .set_prefix(f.project(), "NW", scratch_dir().path())
                .unwrap();
            let saved = f
                .store()
                .read(|tx| tx.story_reset(f.project(), StoryNo::new(1)))
                .unwrap()
                .unwrap();
            assert_eq!(
                serde_json::to_value(saved).unwrap(),
                serde_json::to_value(completed).unwrap()
            );
        }
    }
}

#[test]
fn sh853_closure_cleanup_blocks_until_its_exact_receipt_completes() {
    let f = ServiceFixture::new();
    let id = story(&f, "todo");
    StoryService::new(&f.ctx().no_hooks(true))
        .set_state(
            &id,
            "done",
            Some("Fixture completion for closure ownership"),
            None,
            None,
        )
        .unwrap();
    let mut cleanup = f
        .store()
        .read(|tx| tx.closure_cleanup(f.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    refused(&f, &cleanup.token);
    cleanup.completed = true;
    f.store()
        .write(|tx| tx.update_closure_cleanup(&cleanup))
        .unwrap();
    ProjectService::new(f.store(), f.cwd())
        .set_prefix(f.project(), "NW", scratch_dir().path())
        .unwrap();
    assert_eq!(
        f.store()
            .read(|tx| tx.closure_cleanup(f.project(), StoryNo::new(1)))
            .unwrap(),
        Some(cleanup)
    );
}

fn continuation(f: &ServiceFixture, status: ContinuationStatus) -> Continuation {
    Continuation {
        id: "pending-handoff".into(),
        project_id: f.project(),
        story_no: StoryNo::new(1),
        story_id: "SH-1".into(),
        handoff: serde_json::json!({}),
        generation: serde_json::json!({}),
        capture: serde_json::json!({}),
        status,
        phase: ContinuationPhase::Observe,
        revision: 0,
        attempts: 0,
        created_at: FIXTURE_NOW.into(),
        updated_at: FIXTURE_NOW.into(),
        detail: String::new(),
        reviewed_seq: None,
        reviewed_head: None,
    }
}

#[test]
fn sh853_outstanding_continuations_block_while_terminal_history_survives_rename() {
    for status in [
        ContinuationStatus::Pending,
        ContinuationStatus::Attempting,
        ContinuationStatus::AwaitingAck,
        ContinuationStatus::NeedsAttention,
        ContinuationStatus::Acknowledged,
        ContinuationStatus::Superseded,
    ] {
        let f = ServiceFixture::new();
        story(&f, "in-progress");
        let record = continuation(&f, status);
        f.store()
            .write(|tx| tx.insert_continuation(&record))
            .unwrap();
        if status.outstanding() {
            refused(&f, &record.id);
        } else {
            ProjectService::new(f.store(), f.cwd())
                .set_prefix(f.project(), "NW", scratch_dir().path())
                .unwrap();
            assert_eq!(
                serde_json::to_value(f.store().read(|tx| tx.continuations(f.project())).unwrap())
                    .unwrap(),
                serde_json::to_value(vec![record]).unwrap()
            );
        }
    }
}

fn batch(f: &ServiceFixture) -> VerificationBatch {
    let id = BatchId::generate();
    VerificationBatch {
        branch: id.branch(),
        id,
        project: f.project(),
        project_slug: "fixture".into(),
        head: "SH-1".into(),
        base_branch: "main".into(),
        base_commit: "a".repeat(40),
        tip: "b".repeat(40),
        pull_request: None,
        phase: BatchPhase::Assembled,
        members: (0..2)
            .map(|index| BatchMember {
                story: StoryNo::new(index + 1),
                story_id: format!("SH-{}", index + 1),
                generation: GlobalSeq::new(index + 1),
                head_commit: "c".repeat(40),
                pull_request: format!("https://github.com/example/repo/pull/{}", index + 1),
                position: index as u32,
                branch: None,
                merge_commit: None,
                merge_tree: None,
                resolution: None,
            })
            .collect(),
        excluded: vec![],
        withdrawn: vec![],
        gate: None,
        detail: None,
        bisects: None,
        bisection: None,
        retired: false,
        revision: 0,
        created_at: FIXTURE_NOW.into(),
        updated_at: FIXTURE_NOW.into(),
    }
}

#[test]
fn sh853_live_batches_and_unfinished_released_bisections_block_rename() {
    let f = ServiceFixture::new();
    story(&f, "verifying");
    story(&f, "verifying");
    let mut record = batch(&f);
    f.store()
        .write(|tx| tx.insert_verification_batch(&record))
        .unwrap();
    refused(&f, record.id.as_str());
    record.phase = BatchPhase::Released;
    record.bisection = Some(BatchBisection::default());
    record.revision += 1;
    assert!(
        f.store()
            .write(|tx| tx.update_verification_batch(&record, 0))
            .unwrap()
    );
    refused(&f, record.id.as_str());
    record.bisection.as_mut().unwrap().outcome = Some(BisectionOutcome::Interrupted {
        detail: "operator stopped search".into(),
    });
    record.revision += 1;
    assert!(
        f.store()
            .write(|tx| tx.update_verification_batch(&record, 1))
            .unwrap()
    );
    ProjectService::new(f.store(), f.cwd())
        .set_prefix(f.project(), "NW", scratch_dir().path())
        .unwrap();
    assert_eq!(
        f.store()
            .read(|tx| tx.verification_batches(f.project()))
            .unwrap(),
        vec![record]
    );
}

#[test]
fn sh853_dropped_cleanup_blocks_until_released_and_keeps_its_minted_lease() {
    let f = ServiceFixture::new();
    story(&f, "in-progress");
    let mut cleanup = DroppedCleanup {
        project: f.project(),
        story: StoryNo::new(1),
        token: "dropped-owner".into(),
        generation: GlobalSeq::new(1),
        lease: storyhook::domain::StoryCleanupLease {
            version: storyhook::domain::CLEANUP_LEASE_VERSION,
            project_slug: "fixture".into(),
            story_id: "SH-1".into(),
            repository_path: f.cwd().into(),
            worktree_path: f.cwd().join("lane"),
            branch: "worktree-SH-1".into(),
            tmux: storyhook::domain::TmuxCleanupTarget {
                revivify: None,
                socket_path: f.cwd().join("absent-socket"),
            },
        },
        resources: storyhook::service::resources::ResourceReport {
            location_only: false,
            project: "fixture".into(),
            story_id: "SH-1".into(),
            status: "absent".into(),
            repository: Some(f.cwd().into()),
            worktree: None,
            branch: Some("worktree-SH-1".into()),
            window_name: "SH-1".into(),
            socket_path: None,
            pane: None,
            provider: None,
            candidates: vec![],
            observations: vec![],
            diagnostics: vec![],
        },
        paths: vec![],
        process_start: None,
        phase: DroppedCleanupPhase::Prepared,
        released: false,
        failure: None,
    };
    f.store()
        .write(|tx| tx.put_dropped_cleanup(&cleanup))
        .unwrap();
    refused(&f, &cleanup.token);
    // A released refusal is retained evidence, not an active cleanup owner.
    cleanup.released = true;
    cleanup.failure = Some("No destructive step began".into());
    f.store()
        .write(|tx| tx.put_dropped_cleanup(&cleanup))
        .unwrap();
    ProjectService::new(f.store(), f.cwd())
        .set_prefix(f.project(), "NW", scratch_dir().path())
        .unwrap();
    let saved = f
        .store()
        .read(|tx| tx.dropped_cleanup(f.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(saved).unwrap(),
        serde_json::to_value(cleanup).unwrap()
    );
}

#[test]
fn sh853_pending_landing_names_the_same_owner_in_preview_and_write() {
    use storyhook::service::landing::{LandingAdmission, VerifiedSubmission};
    use storyhook::service::{PrLinkService, VerificationQueue};
    let f = ServiceFixture::new();
    f.github_checkout("https://github.com/acme/widgets");
    let id = story(&f, "todo");
    let ctx = f.ctx().no_hooks(true);
    PrLinkService::new(&ctx)
        .link(&id, "https://github.com/acme/widgets/pull/1", true)
        .unwrap();
    StoryService::new(&ctx)
        .set_state(&id, "verifying", None, None, None)
        .unwrap();
    let queue = VerificationQueue::new(f.store());
    let candidate = queue.next().unwrap().unwrap();
    let LandingAdmission::Admitted(intent) = queue
        .begin_landing(
            &ctx,
            &candidate,
            &VerifiedSubmission {
                head: "a".repeat(40),
                tree: "b".repeat(40),
                gate: "make test".into(),
            },
        )
        .unwrap()
    else {
        panic!("expected fixture landing admission")
    };
    refused(&f, &intent.id);
    assert_eq!(
        f.store().read(|tx| tx.landing_intents()).unwrap(),
        vec![intent]
    );
}

#[test]
fn sh853_attempting_delivery_blocks_but_pending_numeric_delivery_allows_rename() {
    for attempting in [false, true] {
        let f = ServiceFixture::new();
        story(&f, "in-progress");
        f.store()
            .write(|tx| {
                tx.enqueue_block_delivery(f.project(), StoryNo::new(1), BlockAction::Interrupt)
            })
            .unwrap();
        let mut delivery = f
            .store()
            .read(|tx| tx.block_deliveries(f.project()))
            .unwrap()
            .remove(0);
        if attempting {
            delivery.status = DeliveryStatus::Attempting;
            assert!(
                f.store()
                    .write(|tx| tx.update_block_delivery(&delivery, DeliveryStatus::Pending))
                    .unwrap()
            );
            refused(&f, &format!("block delivery `{}`", delivery.id));
        } else {
            ProjectService::new(f.store(), f.cwd())
                .set_prefix(f.project(), "NW", scratch_dir().path())
                .unwrap();
        }
        assert_eq!(
            f.store()
                .read(|tx| tx.block_deliveries(f.project()))
                .unwrap(),
            vec![delivery]
        );
    }
}

fn leased_submission(f: &ServiceFixture) -> (String, storyhook::domain::StoryCleanupLease) {
    let id = story(f, "todo");
    StoryService::new(&f.ctx().no_hooks(true))
        .set_state(&id, "verifying", None, None, None)
        .unwrap();
    let worktree = f.cwd().join("leased-workspace");
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::write(worktree.join("preserved.txt"), "unpublished work").unwrap();
    let lease = storyhook::domain::StoryCleanupLease {
        version: storyhook::domain::CLEANUP_LEASE_VERSION,
        project_slug: "fixture".into(),
        story_id: id.clone(),
        repository_path: f.cwd().into(),
        worktree_path: worktree,
        branch: format!("worktree-{id}"),
        tmux: storyhook::domain::TmuxCleanupTarget {
            revivify: None,
            socket_path: f.cwd().join("leased-socket"),
        },
    };
    f.append_cleanup_lease(&id, lease.clone());
    (id, lease)
}

#[test]
fn sh853_standalone_leased_handoff_blocks_rename_even_when_held_or_stopped() {
    for mode in ["queued", "held", "stopped", "returned-repair"] {
        let f = ServiceFixture::new();
        let (id, lease) = leased_submission(&f);
        let ctx = f.ctx().no_hooks(true);
        let service = StoryService::new(&ctx);
        match mode {
            "held" => {
                service.set_awaiting(&id, "operator review").unwrap();
            }
            "stopped" => {
                f.store()
                    .write(|tx| tx.put_verification_enabled(f.project(), false))
                    .unwrap();
            }
            "returned-repair" => {
                service
                    .set_state(&id, "in-progress", None, None, None)
                    .unwrap();
            }
            _ => {}
        }
        let events = f
            .store()
            .read(|tx| tx.events_for(f.project(), StoryNo::new(1)))
            .unwrap();
        assert!(
            f.store()
                .read(|tx| tx.engine_runs("fixture"))
                .unwrap()
                .is_empty()
        );
        assert!(
            f.store()
                .read(|tx| tx.landing_intents())
                .unwrap()
                .is_empty()
        );
        assert!(
            f.store()
                .read(|tx| tx.verification_batches(f.project()))
                .unwrap()
                .is_empty()
        );
        refused(&f, "retains cleanup lease for `SH-1`");
        let after = f
            .store()
            .read(|tx| tx.events_for(f.project(), StoryNo::new(1)))
            .unwrap();
        assert_eq!(after, events);
        assert_eq!(
            storyhook::service::latest_generation(&after).unwrap().lease,
            Some(lease.clone())
        );
        assert_eq!(
            std::fs::read_to_string(lease.worktree_path.join("preserved.txt")).unwrap(),
            "unpublished work"
        );
    }
}

#[test]
fn sh853_reaped_generation_history_allows_rename_without_rewriting_its_lease() {
    let f = ServiceFixture::new();
    let (id, lease) = leased_submission(&f);
    let ctx = f.ctx().no_hooks(true);
    let service = StoryService::new(&ctx);
    service
        .set_state(&id, "in-progress", None, None, None)
        .unwrap();
    std::fs::remove_dir_all(&lease.worktree_path).unwrap();
    // This is the same durable generation-scoped retirement proof consumed by
    // the cleanup queue; an idle/stopped queue alone is not that proof.
    service
        .comment(
            &id,
            &format!(
                "{} verified absent.",
                storyhook::service::VERIFICATION_CLEANUP_COMPLETE_PREFIX
            ),
        )
        .unwrap();
    let before = f
        .store()
        .read(|tx| tx.events_for(f.project(), StoryNo::new(1)))
        .unwrap();
    let old = storyhook::service::latest_generation(&before).unwrap();
    assert_eq!(
        old.reap_marker,
        Some(storyhook::service::ReapMarker::Complete)
    );
    ProjectService::new(f.store(), f.cwd())
        .set_prefix(f.project(), "NW", scratch_dir().path())
        .unwrap();
    let after = f
        .store()
        .read(|tx| tx.events_for(f.project(), StoryNo::new(1)))
        .unwrap();
    assert_eq!(storyhook::service::latest_generation(&after), Some(old));
    assert_eq!(lease.story_id, "SH-1");
    assert!(!lease.worktree_path.exists());
}

#[test]
fn sh853_completed_exact_closure_retires_lease_but_unreaped_closure_does_not() {
    let f = ServiceFixture::new();
    let (id, lease) = leased_submission(&f);
    StoryService::new(&f.ctx().no_hooks(true))
        .set_state(
            &id,
            "done",
            Some("Fixture override to exercise the exact closure receipt"),
            None,
            None,
        )
        .unwrap();
    let mut closure = f
        .store()
        .read(|tx| tx.closure_cleanup(f.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    // The cleanup worker pins the lease before completing this exact closure.
    closure.lease = Some(lease.clone());
    f.store()
        .write(|tx| tx.update_closure_cleanup(&closure))
        .unwrap();
    refused(&f, &closure.token);
    closure.completed = true;
    f.store()
        .write(|tx| tx.update_closure_cleanup(&closure))
        .unwrap();
    ProjectService::new(f.store(), f.cwd())
        .set_prefix(f.project(), "NW", scratch_dir().path())
        .unwrap();
    assert_eq!(
        f.store()
            .read(|tx| tx.closure_cleanup(f.project(), StoryNo::new(1)))
            .unwrap(),
        Some(closure)
    );
    let events = f
        .store()
        .read(|tx| tx.events_for(f.project(), StoryNo::new(1)))
        .unwrap();
    assert_eq!(
        storyhook::service::latest_generation(&events)
            .unwrap()
            .lease,
        Some(lease)
    );
}

#[test]
fn sh853_old_reap_proof_cannot_retire_a_new_leased_generation() {
    let f = ServiceFixture::new();
    let (id, lease) = leased_submission(&f);
    let ctx = f.ctx().no_hooks(true);
    let service = StoryService::new(&ctx);
    service
        .set_state(&id, "in-progress", None, None, None)
        .unwrap();
    service
        .comment(
            &id,
            &format!(
                "{} verified absent.",
                storyhook::service::VERIFICATION_CLEANUP_COMPLETE_PREFIX
            ),
        )
        .unwrap();
    service
        .set_state(&id, "verifying", None, None, None)
        .unwrap();
    f.append_cleanup_lease(&id, lease);
    let events = f
        .store()
        .read(|tx| tx.events_for(f.project(), StoryNo::new(1)))
        .unwrap();
    assert_eq!(
        storyhook::service::latest_generation(&events)
            .unwrap()
            .reap_marker,
        None
    );
    refused(&f, "retains cleanup lease for `SH-1`");
}
