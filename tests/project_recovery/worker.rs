//! Production transactions and subprocess transport; only the provider endpoint is replaced.
use super::*;
use std::sync::atomic::AtomicBool;
use storyhook::daemon::{
    project_recovery::process_one,
    verification::{ShellVerificationActuator, VerificationActivity},
};
use storyhook::service::project_recovery::{RepairScope, WorkStatus};

pub(super) fn helper(f: &ServiceFixture, reply: &str) -> ShellVerificationActuator {
    let checkout = f
        .store()
        .read(|tx| tx.checkout_path(f.project()))
        .unwrap()
        .unwrap();
    let path = f.cwd().join("recovery-helper.sh");
    std::fs::write(
        &path,
        format!(
            r#"#!/bin/bash
cd '{}'
python3 - "$@" <<'PY'
import fcntl, json, os, sys
args = sys.argv[1:]
path = '.git/storyhook/workspace-locks/' + args[3] + '.lock'
a, b = os.fstat(int(os.environ['STORY_WORKSPACE_LOCK_FD'])), os.stat(path)
assert (a.st_dev, a.st_ino) == (b.st_dev, b.st_ino)
with open(path, 'a') as rival:
    try: fcntl.flock(rival, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError: pass
    else: raise AssertionError('workspace exclusion lost')
with open('recovery-calls', 'a') as output: output.write(json.dumps(args) + '\n')
import time
payload = json.loads({reply:?})
time.sleep(payload.pop('delay', 0))
print(json.dumps(payload.get('dispatch_reply', {{'ok': True}}) if args[2] == 'dispatch' else payload))
PY
"#,
            checkout.display()
        ),
    )
    .unwrap();
    ShellVerificationActuator::with_paths(f.env().clone(), path, "unused-story".into())
}

#[test]
fn sh870_unproved_fault_never_delivers_even_after_workspace_release() {
    let f = fixture();
    let candidate = submitted(&f, "held fault");
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let view = service
        .observe(&candidate, &fault(), "observed")
        .unwrap()
        .unwrap();
    let locks = candidate.checkout.join(".git/storyhook/workspace-locks");
    std::fs::create_dir_all(&locks).unwrap();
    let owner = std::fs::File::create(locks.join("SH-1.lock")).unwrap();
    fs4::FileExt::lock_exclusive(&owner).unwrap();
    let actuator = helper(&f, r#"{"ok":true}"#);
    let activity = VerificationActivity::new();
    let stop = AtomicBool::new(false);
    assert!(!process_one(f.store(), f.env(), &actuator, &activity, &stop).unwrap());
    drop(owner);
    assert!(!process_one(f.store(), f.env(), &actuator, &activity, &stop).unwrap());
    assert_eq!(service.show(&view.record.id).unwrap(), view);
    assert!(!candidate.checkout.join("recovery-calls").exists());
}

#[test]
fn sh870_interrupted_legacy_assessment_is_held_without_replaying_delivery() {
    let f = fixture();
    let candidate = submitted(&f, "interrupted");
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let view = service
        .observe(&candidate, &fault(), "observed")
        .unwrap()
        .unwrap();
    let mut pending = view.clone();
    pending.state.assessment.status = AssessmentStatus::InFlight;
    pending.state.assessment.hold = None;
    pending.state.assessment.epoch = 1;
    pending.state.assessment.started_at = Some(ctx.now());
    legacy::save(&f, pending);
    let actuator = helper(&f, r#"{"ok":true}"#);
    assert!(
        process_one(
            f.store(),
            f.env(),
            &actuator,
            &VerificationActivity::new(),
            &AtomicBool::new(false)
        )
        .unwrap()
    );
    let held = service.show(&view.record.id).unwrap();
    assert_eq!(
        held.state.assessment.hold,
        Some(AssessmentHold::CauseUnproved)
    );
    assert!(!candidate.checkout.join("recovery-calls").exists());
}

#[test]
fn sh870_retained_same_story_repair_delivery_uses_accepted_scope_and_original_target() {
    let f = fixture();
    let initial = decision::ready(&f);
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let view = legacy::retain(&f, initial.clone(), RepairScope::SameStory);
    let actuator = helper(&f, r#"{"ok":true}"#);
    assert!(
        process_one(
            f.store(),
            f.env(),
            &actuator,
            &VerificationActivity::new(),
            &AtomicBool::new(false)
        )
        .unwrap()
    );
    let delivered = service.show(&view.record.id).unwrap();
    assert_eq!(delivered.state.work[0].status, WorkStatus::Delivered);
    assert_eq!(delivered.state.work[0].epoch, 1);
}

#[test]
fn sh870_retained_fresh_separate_repair_dispatch_requires_proven_absence_and_independent_identity()
{
    for (reason, expected) in [
        ("pane-unavailable", WorkStatus::Delivered),
        ("provider-unknown", WorkStatus::Held),
    ] {
        let f = fixture();
        let initial = decision::ready(&f);
        let ctx = f.ctx();
        let service = ProjectRecoveryService::new(&ctx);
        let view = legacy::retain(&f, initial.clone(), RepairScope::SeparateStory);
        let actuator = helper(
            &f,
            &format!(r#"{{"ok":false,"reason":"{reason}","display":"provider response"}}"#),
        );
        assert!(
            process_one(
                f.store(),
                f.env(),
                &actuator,
                &VerificationActivity::new(),
                &AtomicBool::new(false)
            )
            .unwrap()
        );
        let after = service.show(&view.record.id).unwrap();
        assert_eq!(
            after.state.work[0].status, expected,
            "{}",
            after.state.work[0].detail
        );
        let checkout = &initial.state.subjects[0].candidate.checkout;
        let calls = std::fs::read_to_string(checkout.join("recovery-calls")).unwrap();
        assert_eq!(
            calls.lines().count(),
            if expected == WorkStatus::Delivered {
                2
            } else {
                1
            }
        );
        for line in calls.lines() {
            let args: Vec<String> = serde_json::from_str(line).unwrap();
            assert_eq!(args[3], "SH-2");
            assert!(!args.iter().any(|a| a == "--full-auto" || a == "--resume"));
        }
    }
}

#[test]
fn sh870_retained_absent_original_agent_without_managed_resources_is_held_not_replaced() {
    let f = fixture();
    let candidate = submitted(&f, "unleased assessment");
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let view = service
        .observe(&candidate, &fault(), "observed")
        .unwrap()
        .unwrap();
    let view = legacy::retain(&f, view, RepairScope::SameStory);
    let actuator = helper(
        &f,
        r#"{"ok":false,"reason":"pane-unavailable","display":"absent"}"#,
    );
    assert!(
        process_one(
            f.store(),
            f.env(),
            &actuator,
            &VerificationActivity::new(),
            &AtomicBool::new(false)
        )
        .unwrap()
    );
    assert_eq!(
        service.show(&view.record.id).unwrap().state.work[0].hold,
        Some(AssessmentHold::OwnershipUncertain)
    );
    assert_eq!(
        std::fs::read_to_string(candidate.checkout.join("recovery-calls"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[test]
fn sh870_retained_policy_monitor_cancels_live_delivery_and_retains_transient_reservations() {
    const PROVIDER_DELAY_SECS: u64 = 30;
    for stop in [false, true] {
        let f = fixture();
        let candidate = submitted(&f, "cancelled assessment");
        let ctx = f.ctx();
        let service = ProjectRecoveryService::new(&ctx);
        let view = service
            .observe(&candidate, &fault(), "observed")
            .unwrap()
            .unwrap();
        let view = legacy::retain(&f, view, RepairScope::SameStory);
        let actuator = helper(
            &f,
            &serde_json::json!({"ok":true,"delay":PROVIDER_DELAY_SECS}).to_string(),
        );
        std::thread::scope(|scope| {
            let operation = scope.spawn(|| {
                process_one(
                    f.store(),
                    f.env(),
                    &actuator,
                    &VerificationActivity::new(),
                    &AtomicBool::new(false),
                )
            });
            let mut patience = storyhook_test_support::load_grace::Patience::new(
                std::time::Duration::from_secs(10),
            );
            while !candidate.checkout.join("recovery-calls").exists() {
                assert!(!patience.expired(), "{patience}; provider did not start");
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let deadline =
                std::time::Instant::now() + std::time::Duration::from_secs(PROVIDER_DELAY_SECS / 3);
            if stop {
                f.store()
                    .write(|tx| tx.put_verification_enabled(f.project(), false))
                    .unwrap();
            } else {
                StoryService::new(&ctx)
                    .set_labels("SH-1", &["no-auto".into()], &[])
                    .unwrap();
                StoryService::new(&ctx)
                    .set_labels("SH-1", &[], &["no-auto".into()])
                    .unwrap();
            }
            assert!(operation.join().unwrap().unwrap());
            assert!(
                std::time::Instant::now() < deadline,
                "cancellation waited for provider timeout"
            );
        });
        let held = service.show(&view.record.id).unwrap();
        assert_eq!(
            held.state.work[0].hold,
            Some(if stop {
                AssessmentHold::OperatorStop
            } else {
                AssessmentHold::AuthorityChanged
            })
        );
    }
}

#[test]
fn sh870_retained_existing_managed_claim_is_adopted_only_with_exact_lease_and_one_transition() {
    use storyhook::domain::{StoryCleanupLease, TmuxCleanupTarget};
    use storyhook::service::engine::{EngineService, StartRequest};
    use storyhook::store::{EngineAgent, EngineLaneState, EngineScope};
    for invalid in ["valid", "missing-lease", "changed-state", "foreign-lease"] {
        let f = fixture();
        let initial = decision::ready(&f);
        let ctx = f.ctx();
        let service = ProjectRecoveryService::new(&ctx);
        let view = legacy::retain(&f, initial.clone(), RepairScope::SeparateStory);
        let run = EngineService::new(&ctx, &storyhook_test_support::FakeDispatcher::default())
            .start(StartRequest {
                scope: EngineScope::Project,
                lanes: 1,
                agent: EngineAgent::Codex,
                model: None,
                effort: None,
                speed: None,
            })
            .unwrap();
        StoryService::new(&ctx).claim_story("SH-2", None).unwrap();
        if invalid == "changed-state" {
            StoryService::new(&ctx)
                .set_state("SH-2", "todo", None, None, None)
                .unwrap();
            StoryService::new(&ctx).claim_story("SH-2", None).unwrap();
        }
        let checkout = &initial.state.subjects[0].candidate.checkout;
        let lease = StoryCleanupLease {
            version: 1,
            project_slug: initial.state.subjects[0].candidate.project_slug.clone(),
            story_id: if invalid == "foreign-lease" {
                "SH-3"
            } else {
                "SH-2"
            }
            .into(),
            repository_path: checkout.clone(),
            worktree_path: checkout.join(".codex/worktrees/SH-2"),
            branch: "worktree-SH-2".into(),
            tmux: TmuxCleanupTarget {
                revivify: None,
                socket_path: checkout.join("tmux.sock"),
            },
        };
        let mut lane = f
            .store()
            .read(|tx| tx.engine_lanes(&run.id))
            .unwrap()
            .remove(0);
        lane.state = EngineLaneState::Working;
        lane.story_id = Some("SH-2".into());
        lane.cleanup_lease = (invalid != "missing-lease").then_some(lease.clone());
        f.store().write(|tx| tx.put_engine_lane(&lane)).unwrap();
        let actuator = helper(&f, r#"{"ok":true}"#);
        assert!(
            process_one(
                f.store(),
                f.env(),
                &actuator,
                &VerificationActivity::new(),
                &AtomicBool::new(false)
            )
            .unwrap()
        );
        let current = service.show(&view.record.id).unwrap();
        assert_eq!(
            current.state.work[0].status,
            if invalid == "valid" {
                WorkStatus::Delivered
            } else {
                WorkStatus::Held
            },
            "{invalid}: {}",
            current.state.work[0].detail
        );
        if invalid == "valid" {
            assert_eq!(
                service
                    .delivery_candidate(&view.record.id, StoryNo::new(2))
                    .unwrap()
                    .cleanup_lease,
                Some(lease)
            );
        } else {
            assert!(!checkout.join("recovery-calls").exists());
        }
    }
}

#[test]
fn sh870_retained_transient_operator_block_revokes_work_even_after_it_is_cleared() {
    let f = fixture();
    let initial = decision::ready(&f);
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let view = legacy::retain(&f, initial.clone(), RepairScope::SameStory);
    let work = &view.state.work[0];
    service
        .claim_work(&view.record.id, &work.id)
        .unwrap()
        .unwrap();
    StoryService::new(&ctx)
        .set_awaiting("SH-1", "operator hold")
        .unwrap();
    StoryService::new(&ctx).clear_awaiting("SH-1").unwrap();
    assert!(
        !service
            .delivery_permitted(&view.record.id, Some(&work.id), 1, false)
            .unwrap()
    );
}

#[test]
fn sh870_retained_proven_undelivered_dispatch_is_bounded_but_unconfirmed_delivery_is_not_replayed()
{
    for phase in ["undelivered", "received-unsubmitted"] {
        let f = fixture();
        let initial = decision::ready(&f);
        let ctx = f.ctx();
        let service = ProjectRecoveryService::new(&ctx);
        let view = legacy::retain(&f, initial.clone(), RepairScope::SeparateStory);
        let response = serde_json::json!({"ok":false,"reason":"pane-unavailable","display":"absent", "dispatch_reply":{"ok":false,"reason":"handoff-undelivered","delivery_phase":phase,"display":"provider delivery failed"}});
        let actuator = helper(&f, &response.to_string());
        for attempt in 1..=if phase == "undelivered" { 3 } else { 1 } {
            assert!(
                process_one(
                    f.store(),
                    f.env(),
                    &actuator,
                    &VerificationActivity::new(),
                    &AtomicBool::new(false)
                )
                .unwrap()
            );
            let current = service.show(&view.record.id).unwrap();
            assert_eq!(
                current.state.work[0].failures,
                if phase == "undelivered" { attempt } else { 0 }
            );
        }
        assert!(
            !process_one(
                f.store(),
                f.env(),
                &actuator,
                &VerificationActivity::new(),
                &AtomicBool::new(false)
            )
            .unwrap()
        );
        assert_eq!(
            service.show(&view.record.id).unwrap().state.work[0].hold,
            Some(if phase == "undelivered" {
                AssessmentHold::DeliveryExhausted
            } else {
                AssessmentHold::OwnershipUncertain
            })
        );
    }
}

#[test]
fn sh870_retained_proven_absence_resumes_only_the_exact_registered_lease_and_preserves_work() {
    use storyhook::domain::{CLEANUP_LEASE_MARKER, StoryCleanupLease, TmuxCleanupTarget};
    for changed_identity in [false, true] {
        let f = fixture();
        let original = submitted(&f, "leased assessment");
        let repository = std::fs::canonicalize(&original.checkout).unwrap();
        let git = |root: &std::path::Path, args: &[&str]| {
            let output = storyhook::env::git_env::command(root)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap().trim().to_string()
        };
        git(
            &repository,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.com",
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "--allow-empty",
                "-qm",
                "fixture",
            ],
        );
        let worktree = repository.join(".codex/worktrees/SH-1");
        git(
            &repository,
            &[
                "worktree",
                "add",
                "-qb",
                "worktree-SH-1",
                worktree.to_str().unwrap(),
            ],
        );
        let lease = StoryCleanupLease {
            version: 1,
            project_slug: original.project_slug.clone(),
            story_id: "SH-1".into(),
            repository_path: repository.clone(),
            worktree_path: worktree.clone(),
            branch: "worktree-SH-1".into(),
            tmux: TmuxCleanupTarget {
                revivify: None,
                socket_path: repository.join("absent.sock"),
            },
        };
        let private = git(&worktree, &["rev-parse", "--absolute-git-dir"]);
        std::fs::write(
            std::path::Path::new(&private).join(CLEANUP_LEASE_MARKER),
            serde_json::to_vec(&lease).unwrap(),
        )
        .unwrap();
        std::fs::write(worktree.join("preserved-work"), "dirty work").unwrap();
        f.append_cleanup_lease("SH-1", lease);
        let candidate = VerificationQueue::new(f.store())
            .ordered_for(f.project())
            .unwrap()
            .remove(0);
        let ctx = f.ctx();
        let service = ProjectRecoveryService::new(&ctx);
        let view = service
            .observe(&candidate, &fault(), "observed")
            .unwrap()
            .unwrap();
        let view = legacy::retain(&f, view, RepairScope::SameStory);
        if changed_identity {
            git(&worktree, &["branch", "-m", "different-owner"]);
        }
        let actuator = helper(
            &f,
            r#"{"ok":false,"reason":"pane-unavailable","display":"absent"}"#,
        );
        assert!(
            process_one(
                f.store(),
                f.env(),
                &actuator,
                &VerificationActivity::new(),
                &AtomicBool::new(false)
            )
            .unwrap()
        );
        let current = service.show(&view.record.id).unwrap();
        assert_eq!(
            current.state.work[0].status,
            if changed_identity {
                WorkStatus::Held
            } else {
                WorkStatus::Delivered
            },
            "{}",
            current.state.work[0].detail
        );
        let calls = std::fs::read_to_string(repository.join("recovery-calls")).unwrap();
        assert_eq!(calls.lines().count(), if changed_identity { 1 } else { 2 });
        if !changed_identity {
            let args: Vec<String> = serde_json::from_str(calls.lines().last().unwrap()).unwrap();
            assert!(args.iter().any(|arg| arg == "--resume"));
            // A daemon resume never replaces a live agent (SH-850).
            assert!(args.iter().any(|arg| arg == "--if-absent"));
        }
        assert_eq!(
            std::fs::read_to_string(worktree.join("preserved-work")).unwrap(),
            "dirty work"
        );
    }
}

/// A retained managed lease records the story id its resources were created
/// under. A later prefix change must not make the recovery read as damaged
/// (SH-848): the lease still names the same story by number.
#[test]
fn sh870_retained_a_retained_managed_lease_survives_a_project_prefix_change() {
    use storyhook::domain::{StoryCleanupLease, TmuxCleanupTarget};
    use storyhook::service::engine::{EngineService, StartRequest};
    use storyhook::service::project::ProjectService;
    use storyhook::store::{EngineAgent, EngineLaneState, EngineScope};
    let f = fixture();
    let initial = decision::ready(&f);
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let view = legacy::retain(&f, initial.clone(), RepairScope::SeparateStory);
    let run = EngineService::new(&ctx, &storyhook_test_support::FakeDispatcher::default())
        .start(StartRequest {
            scope: EngineScope::Project,
            lanes: 1,
            agent: EngineAgent::Codex,
            model: None,
            effort: None,
            speed: None,
        })
        .unwrap();
    StoryService::new(&ctx).claim_story("SH-2", None).unwrap();
    let checkout = &initial.state.subjects[0].candidate.checkout;
    let lease = StoryCleanupLease {
        version: 1,
        project_slug: initial.state.subjects[0].candidate.project_slug.clone(),
        story_id: "SH-2".into(),
        repository_path: checkout.clone(),
        worktree_path: checkout.join(".codex/worktrees/SH-2"),
        branch: "worktree-SH-2".into(),
        tmux: TmuxCleanupTarget {
            revivify: None,
            socket_path: checkout.join("tmux.sock"),
        },
    };
    let mut lane = f
        .store()
        .read(|tx| tx.engine_lanes(&run.id))
        .unwrap()
        .remove(0);
    lane.state = EngineLaneState::Working;
    lane.story_id = Some("SH-2".into());
    lane.cleanup_lease = Some(lease.clone());
    f.store().write(|tx| tx.put_engine_lane(&lane)).unwrap();
    assert!(
        process_one(
            f.store(),
            f.env(),
            &helper(&f, r#"{"ok":true}"#),
            &VerificationActivity::new(),
            &AtomicBool::new(false)
        )
        .unwrap()
    );
    let delivered = service.show(&view.record.id).unwrap();
    assert_eq!(delivered.state.work[0].managed_lease, Some(lease.clone()));

    ProjectService::new(f.store(), f.cwd())
        .set_prefix(f.project(), "NW", &f.env().maintenance_backups_dir())
        .unwrap();

    let renamed = service.show(&view.record.id).unwrap();
    assert_eq!(renamed.state.work[0].managed_lease, Some(lease));
    VerificationActivity::new().status(&ctx).unwrap();
}
