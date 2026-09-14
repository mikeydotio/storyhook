//! SH-703: public verifier controls must expose their actual admission state.

use storyhook::cli::parse_invocation;

#[test]
fn verifier_control_grammar_is_available() {
    for words in [
        vec!["verifier", "status"],
        vec!["verifier", "start"],
        vec!["verifier", "stop"],
        vec!["verifier", "drain"],
        vec!["verifier", "ack", "2:123", "--leave-stopped"],
    ] {
        let args: Vec<String> = words.iter().map(|word| (*word).into()).collect();
        assert!(
            parse_invocation(&args).is_ok(),
            "missing control: {words:?}"
        );
    }
}

use storyhook::cli::{Invocation, VerifierAction};
use storyhook::daemon::verification::VerificationActivity;
use storyhook::invoke::dispatch;
use storyhook::service::verification_control::VerificationAction;
use storyhook::service::{NewStoryInput, StoryService, VerificationQueue};
use storyhook::store::{ReadOps, Store, VerificationFailureDisposition, WriteOps};
use storyhook_test_support::ServiceFixture;

fn incident(f: &ServiceFixture) -> String {
    let id = StoryService::new(&f.ctx())
        .create(&NewStoryInput {
            title: "Verifier observability".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    StoryService::new(&f.ctx())
        .set_state(&id, "verifying", None, None, None)
        .unwrap();
    let queue = VerificationQueue::new(f.store());
    let candidate = queue.ordered_for(f.project()).unwrap().remove(0);
    f.store()
        .write(|tx| {
            tx.put_verification_incident(&storyhook::store::VerificationIncident {
                incident_id: "fixture:1".into(),
                project: f.project(),
                story: storyhook::store::StoryNo::new(1),
                generation: candidate.verifying_generation.unwrap(),
                disposition: VerificationFailureDisposition::Permanent,
                halted: true,
                attempts: 1,
                detail: "missing verifier executable".into(),
                first_failed_at: f.env().now(),
                last_failed_at: f.env().now(),
            })
        })
        .unwrap();
    f.store()
        .read(|tx| tx.verification_incident(f.project()))
        .unwrap()
        .unwrap()
        .incident_id
}

#[test]
fn cli_ack_enables_admission_and_preserves_correlated_evidence() {
    let f = ServiceFixture::new();
    let activity = VerificationActivity::new();
    let id = incident(&f);
    activity
        .control(f.store(), f.project(), VerificationAction::Stop)
        .unwrap();
    let ctx = f.ctx().with_verification_activity(Some(&activity));
    dispatch(
        &ctx,
        Invocation::Verifier {
            action: VerifierAction::Ack {
                incident_id: id.clone(),
            },
        },
    )
    .unwrap();
    assert!(
        f.store()
            .read(|tx| tx.verification_enabled(f.project()))
            .unwrap(),
        "ack must actually enable admission"
    );
    let receipt = f
        .store()
        .read(|tx| tx.verification_recovery(f.project()))
        .unwrap();
    let acknowledgement = receipt.acknowledgement.unwrap();
    assert_eq!(acknowledgement.incident.incident_id, id);
    assert_eq!(
        acknowledgement.action,
        storyhook::store::VerificationAcknowledgementIntent::Retry
    );
    assert!(
        receipt.request.is_some(),
        "retry must have a durable receipt"
    );
}

#[test]
fn stopped_recovery_evidence_survives_a_later_start() {
    use storyhook::service::verification_control::VerificationAcknowledgement;
    use storyhook::store::VerificationAcknowledgementIntent;
    for (action, intent) in [
        (
            Some(VerificationAcknowledgement::LeaveStopped),
            VerificationAcknowledgementIntent::LeaveStopped,
        ),
        (None, VerificationAcknowledgementIntent::PreserveAdmission),
    ] {
        let f = ServiceFixture::new();
        let activity = VerificationActivity::new();
        let id = incident(&f);
        activity
            .control(f.store(), f.project(), VerificationAction::Stop)
            .unwrap();
        activity.acknowledge(&f.ctx(), &id, action).unwrap();
        activity
            .control(f.store(), f.project(), VerificationAction::Start)
            .unwrap();
        let receipt = f
            .store()
            .read(|tx| tx.verification_recovery(f.project()))
            .unwrap();
        let acknowledgement = receipt.acknowledgement.unwrap();
        assert_eq!(acknowledgement.incident.incident_id, id);
        assert_eq!(acknowledgement.action, intent);
        assert!(!acknowledgement.enabled);
        assert!(receipt.request.is_some());
    }
}

#[test]
fn rejected_ack_does_not_change_any_recovery_evidence() {
    let f = ServiceFixture::new();
    let activity = VerificationActivity::new();
    incident(&f);
    activity
        .control(f.store(), f.project(), VerificationAction::Stop)
        .unwrap();
    let before = f
        .store()
        .read(|tx| tx.verification_recovery(f.project()))
        .unwrap();
    assert!(
        activity
            .acknowledge(
                &f.ctx(),
                "stale",
                Some(storyhook::service::verification_control::VerificationAcknowledgement::Retry)
            )
            .is_err()
    );
    assert!(
        !f.store()
            .read(|tx| tx.verification_enabled(f.project()))
            .unwrap()
    );
    assert_eq!(
        before,
        f.store()
            .read(|tx| tx.verification_recovery(f.project()))
            .unwrap()
    );
}

#[test]
fn status_names_halt_and_held_stories_without_inventing_an_owner() {
    let f = ServiceFixture::new();
    let activity = VerificationActivity::new();
    let id = incident(&f);
    let status = activity.status(&f.ctx()).unwrap();
    assert_eq!(status.incident.unwrap().incident_id, id);
    assert_eq!(status.held_stories, ["SH-1"]);
    assert!(status.active.is_none());
    assert!(status.warning.unwrap().contains("story verifier ack"));
}

#[test]
fn new_recovery_is_scheduled_not_claimed_as_a_started_gate() {
    let f = ServiceFixture::new();
    let activity = VerificationActivity::new();
    activity
        .control(f.store(), f.project(), VerificationAction::Start)
        .unwrap();
    let status = activity.status(&f.ctx()).unwrap();
    assert!(status.render_human().contains("scheduled"));
    assert!(status.active.is_none());
    assert!(status.recovery.request.unwrap().admission.is_none());
}

#[test]
fn notices_preserve_singular_empty_and_context_json_shapes() {
    use storyhook::output::render_response;
    let f = ServiceFixture::new();
    let activity = VerificationActivity::new();
    incident(&f);
    let ctx = f.ctx().with_verification_activity(Some(&activity));
    for words in [
        vec!["next"],
        vec!["next", "--count", "3"],
        vec!["summary"],
        vec!["load-context", "--format", "json"],
    ] {
        let invocation =
            parse_invocation(&words.iter().map(|s| (*s).into()).collect::<Vec<_>>()).unwrap();
        let response = dispatch(&ctx, invocation).unwrap();
        let rendered = render_response(&response, true, false);
        let json: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert!(
            json["verifier"]["incident"]["halted"].as_bool().unwrap(),
            "{rendered}"
        );
        assert_eq!(json["warnings"].as_array().unwrap().len(), 1);
        if words[0] == "load-context" {
            assert!(json.get("total_stories").is_some());
            assert!(
                serde_json::from_str::<serde_json::Value>(&render_response(
                    &response, false, false
                ))
                .is_ok()
            );
        }
    }
}

#[test]
fn evidence_threshold_uses_active_ownership_and_matching_journal() {
    use storyhook::daemon::verification::journal_path;
    use storyhook::service::Clock;
    let f = ServiceFixture::new();
    let activity = VerificationActivity::new();
    let id = incident(&f);
    f.store()
        .write(|tx| tx.clear_verification_incident(&id))
        .unwrap();
    let candidate = VerificationQueue::new(f.store())
        .ordered_for(f.project())
        .unwrap()
        .remove(0);
    let _guard = activity.acquire(&candidate, "2026-01-01T00:00:00Z".into());
    let waiting = StoryService::new(&f.ctx())
        .create(&NewStoryInput {
            title: "Waiting ahead by priority".into(),
            priority: Some("critical".into()),
            ..Default::default()
        })
        .unwrap();
    StoryService::new(&f.ctx())
        .set_state(&waiting.id, "verifying", None, None, None)
        .unwrap();
    for (seconds, overdue) in [(60, false), (61, true)] {
        let at = (chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z").unwrap()
            + chrono::Duration::seconds(seconds))
        .to_rfc3339();
        let ctx = f.ctx().clock(Clock::Fixed(at));
        let status = activity.status(&ctx).unwrap();
        assert_eq!(status.silence_seconds, Some(seconds as u64));
        assert_eq!(status.warning.is_some(), overdue);
    }
    let path = journal_path(f.env(), &candidate);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, format!("{{\"kind\":\"run\",\"generation\":{},\"at\":\"2026-01-01T00:00:00Z\"}}\n{{\"kind\":\"case\",\"path\":\"gate/test\",\"outcome\":\"pass\"}}\n", candidate.verifying_generation.unwrap().get())).unwrap();
    let modified: chrono::DateTime<chrono::Utc> =
        std::fs::metadata(&path).unwrap().modified().unwrap().into();
    let ctx = f.ctx().clock(Clock::Fixed(modified.to_rfc3339()));
    let status = activity.status(&ctx).unwrap();
    assert_eq!(status.silence_seconds, Some(0));
    assert_eq!(status.last_evidence_at, Some(modified.to_rfc3339()));
    assert_eq!(status.verifying[0], waiting.id);
    assert_eq!(status.active.unwrap().story_id, candidate.story_id);
    assert!(status.warning.is_none(), "{:?}", status.warning);
    std::fs::write(
        &path,
        "{\"kind\":\"run\",\"generation\":0,\"at\":\"2026-01-01T00:00:00Z\"}\n",
    )
    .unwrap();
    assert!(activity.status(&ctx).unwrap().evidence_error.is_some());
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(
        activity
            .status(&ctx)
            .unwrap()
            .evidence_error
            .unwrap()
            .contains("cannot inspect")
    );
}

#[test]
fn resume_hook_reports_stopped_admission_once_and_rejections_do_not_emit() {
    use storyhook::service::verification_control::VerificationAcknowledgement;
    let f = ServiceFixture::new();
    let activity = VerificationActivity::new();
    let id = incident(&f);
    let marker = f.cwd().join("resume-events.jsonl");
    std::fs::write(f.cwd().join(".storyhook.toml"), format!("schema = 1\nuuid = \"fixture-uuid\"\nprefix = \"SH\"\n[hooks.on_verification_resumed]\ncommand = \"cat >> '{}'\"\n", marker.display())).unwrap();
    let ctx = f.ctx().no_hooks(false);
    activity
        .acknowledge(&ctx, &id, Some(VerificationAcknowledgement::LeaveStopped))
        .unwrap();
    assert!(
        activity
            .acknowledge(&ctx, &id, Some(VerificationAcknowledgement::Retry))
            .is_err()
    );
    let body = std::fs::read_to_string(marker).unwrap();
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["event_type"], "verification_resumed");
    assert_eq!(value["enabled"], false);
    assert_eq!(value["remedy"], "story verifier start");
}

#[test]
fn runtime_is_required_for_controls_and_status() {
    let f = ServiceFixture::new();
    for action in [
        VerifierAction::Status,
        VerifierAction::Stop,
        VerifierAction::Start,
    ] {
        let error = dispatch(&f.ctx(), Invocation::Verifier { action }).unwrap_err();
        assert!(error.to_string().contains("runtime unavailable"));
    }
}

#[test]
fn cli_rpc_controls_and_local_lane_budget_share_daemon_status() {
    use storyhook_test_support::{TestEnv, scratch_dir};
    let env = TestEnv::isolated();
    let cwd = scratch_dir();
    env.story(cwd.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    env.story(cwd.path())
        .args(["verifier", "stop"])
        .assert()
        .success();
    let output = env
        .story(cwd.path())
        .args(["verifier", "status", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(status["verifier"]["control"], "stopped");
    let output = env
        .story(cwd.path())
        .args(["lane-budget", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let budget: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert!(budget.get("probe").is_some(), "{budget}");
    assert_eq!(budget["verifier"]["control"], "stopped");
}

#[test]
fn engine_status_preserves_run_and_exposes_the_halted_verifier() {
    use storyhook::service::engine::{EngineService, ShellDispatcher, StartRequest};
    use storyhook::store::{EngineAgent, EngineScope};
    let f = ServiceFixture::new();
    let activity = VerificationActivity::new();
    incident(&f);
    let ctx = f.ctx().with_verification_activity(Some(&activity));
    // Starting a run records idle lanes; only reconciliation dispatches a process.
    let dispatcher = ShellDispatcher::new(f.cwd().join("unused-dispatcher"), f.env().clone());
    let run = EngineService::new(&ctx, &dispatcher)
        .start(StartRequest {
            scope: EngineScope::Project,
            lanes: 1,
            agent: EngineAgent::Codex,
            model: None,
            effort: None,
            speed: None,
        })
        .unwrap();
    let invocation = parse_invocation(&["engine".into(), "status".into()]).unwrap();
    let response = dispatch(&ctx, invocation).unwrap();
    let json: serde_json::Value =
        serde_json::from_str(&storyhook::output::render_response(&response, true, false)).unwrap();
    assert_eq!(json["run"]["id"], run.id);
    assert!(json["verifier"]["incident"]["halted"].as_bool().unwrap());
    assert_eq!(json["warnings"].as_array().unwrap().len(), 1);
}
