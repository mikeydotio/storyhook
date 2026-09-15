//! SH-714: retained infrastructure history must not mask an authenticated retry.

use std::sync::atomic::{AtomicUsize, Ordering};

use storyhook::api::http::TrustedHosts;
use storyhook::api::rest;
use storyhook::daemon::http1::{Header, Method};
use storyhook::daemon::lifecycle::InFlight;
use storyhook::daemon::verification::{
    NotifyDelivery, ResumePlan, SubmissionFailure, TickResult, VerificationActivity,
    VerificationActuator, VerificationOutcome, journal_path, tick_with_activity,
};
use storyhook::daemon::verification_progress::{
    VerificationStatus, publish_once, status_snapshot_with_incident,
};
use storyhook::domain::SubmittedPullRequest;
use storyhook::error::AppError;
use storyhook::service::gate_progress::GATE_PROGRESS_PREFIX;
use storyhook::service::{
    Clock, NewStoryInput, PrLinkService, StoryService, VerificationCandidate, VerificationQueue,
};
use storyhook::store::{PrLink, ReadOps, Store, StoryNo, VerificationFailureDisposition, WriteOps};
use storyhook_test_support::ServiceFixture;

struct RetryObserver<'a> {
    fixture: &'a ServiceFixture,
    activity: &'a VerificationActivity,
    calls: AtomicUsize,
}

impl VerificationActuator for RetryObserver<'_> {
    fn land(
        &self,
        _: &VerificationCandidate,
        _: &storyhook::store::LandingIntent,
    ) -> storyhook::daemon::verification::LandingOutcome {
        panic!("retryable infrastructure has no certificate to land")
    }

    fn recover_landing(
        &self,
        _: &VerificationCandidate,
        _: &storyhook::store::LandingIntent,
    ) -> storyhook::daemon::verification::LandingOutcome {
        panic!("retryable infrastructure never acquires landing authority")
    }

    fn submit(&self, _: &VerificationCandidate) -> Result<SubmittedPullRequest, SubmissionFailure> {
        panic!("linked, unleased fixture must not submit")
    }

    fn verify(&self, candidate: &VerificationCandidate, _: &PrLink) -> VerificationOutcome {
        if self.calls.fetch_add(1, Ordering::SeqCst) > 0 {
            let f = self.fixture;
            let active = self.activity.active_for(f.project()).unwrap();
            let incident = f
                .store()
                .read(|tx| tx.verification_incident(f.project()))
                .unwrap()
                .unwrap();
            assert_eq!(incident.attempts, 1);
            assert!(!incident.halted);
            let journal = journal_path(f.env(), candidate);
            std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
            let now = f.env().now();
            let rows = [
                serde_json::json!({"kind":"run", "generation":candidate.verifying_generation.unwrap().get(), "attempt_id":active.attempt_id, "at":now}),
                serde_json::json!({"kind":"item", "path":"merge preflight", "status":"passed", "at":now}),
                serde_json::json!({"kind":"item", "path":"release gate", "status":"running", "at":now}),
            ];
            std::fs::write(
                &journal,
                rows.iter()
                    .map(|row| format!("{row}\n"))
                    .collect::<String>(),
            )
            .unwrap();
            let ordered = VerificationQueue::new(f.store())
                .ordered_for(f.project())
                .unwrap();
            let statuses = status_snapshot_with_incident(
                &ordered,
                Some(&active),
                Some(&incident),
                f.env(),
                &now,
            );
            let without_incident =
                status_snapshot_with_incident(&ordered, Some(&active), None, f.env(), &now);
            assert!(
                matches!(&without_incident[0].2,
                VerificationStatus::Running { current_step: Some(step), .. }
                if step.label == "release gate"),
                "{:?}",
                without_incident[0].2
            );
            assert!(
                matches!(statuses[0].2, VerificationStatus::Running { .. }),
                "an admitted retry with matching preflight and live gate evidence must be Running; got {:?}",
                statuses[0].2
            );
            assert!(
                matches!(
                    statuses[1].2,
                    VerificationStatus::Queued {
                        position: 1,
                        blocked_by: None,
                        ..
                    }
                ),
                "{:?}",
                statuses[1].2
            );
            let status = self
                .activity
                .status(&f.ctx().clock(Clock::Fixed(now.clone())))
                .unwrap();
            assert!(status.held_stories.is_empty(), "{:?}", status.held_stories);
            assert_eq!(status.incident.as_ref(), Some(&incident));
            assert!(!status.incident_is_current);
            assert!(
                status
                    .render_human()
                    .contains("Previous infrastructure failure; current retry running")
            );
            let mut legacy = serde_json::to_value(&status).unwrap();
            legacy
                .as_object_mut()
                .unwrap()
                .remove("incident_is_current");
            legacy["active"]
                .as_object_mut()
                .unwrap()
                .remove("retry_origin");
            let legacy: storyhook::daemon::verification::status::VerifierStatus =
                serde_json::from_value(legacy).unwrap();
            assert!(legacy.incident_is_current);
            assert!(legacy.active.unwrap().retry_origin.is_none());
            assert!(status.warning.is_none(), "{:?}", status.warning);
            let ctx = f
                .ctx()
                .clock(Clock::Fixed(now.clone()))
                .with_verification_activity(Some(self.activity));
            for words in [
                vec!["load-context", "--format", "json"],
                vec!["summary"],
                vec!["next"],
            ] {
                let invocation = storyhook::cli::parse_invocation(
                    &words.iter().map(|word| (*word).into()).collect::<Vec<_>>(),
                )
                .unwrap();
                let response = storyhook::invoke::dispatch(&ctx, invocation).unwrap();
                let rendered = storyhook::output::render_response(&response, true, false);
                let response: serde_json::Value = serde_json::from_str(&rendered).unwrap();
                assert_eq!(
                    response["verifier"]["incident_is_current"], false,
                    "{words:?}"
                );
                assert_eq!(response["verifier"]["incident"]["attempts"], 1);
                assert_eq!(response["verifier"]["held_stories"], serde_json::json!([]));
                assert!(
                    response
                        .get("warnings")
                        .is_none_or(|warnings| warnings.as_array().is_some_and(Vec::is_empty)),
                    "{rendered}"
                );
            }
            let routed = rest::route_with_activity(
                f.store(),
                f.env(),
                self.activity,
                rest::RouteRequest::new(
                    &Method::Get,
                    "/api/repos/fixture/data",
                    &[Header::from_bytes("Host", "127.0.0.1:3456").unwrap()],
                    "",
                ),
                &TrustedHosts::default(),
            );
            assert_eq!(routed.reply.status, 200);
            let data: serde_json::Value =
                serde_json::from_str(routed.reply.text_body().unwrap()).unwrap();
            let stories = data["stories"].as_array().unwrap();
            let running = stories
                .iter()
                .find(|view| view["story"]["id"] == "SH-1")
                .unwrap();
            let queued = stories
                .iter()
                .find(|view| view["story"]["id"] == "SH-2")
                .unwrap();
            assert_eq!(running["verification"]["status"], "running");
            assert_eq!(queued["verification"]["status"], "queued");
            assert!(queued["verification"].get("blocked_by").is_none());
            publish_once(f.store(), f.env(), &now, self.activity).unwrap();
            let row = f
                .store()
                .read(|tx| tx.story(f.project(), StoryNo::new(1)))
                .unwrap()
                .unwrap();
            let progress = row
                .snapshot
                .comments
                .iter()
                .find(|comment| comment.text.starts_with(GATE_PROGRESS_PREFIX))
                .unwrap();
            assert!(
                !progress.text.contains("RETRYING INFRASTRUCTURE"),
                "{}",
                progress.text
            );
            assert!(progress.text.contains("release gate"), "{}", progress.text);
            assert_eq!(
                f.store()
                    .read(|tx| tx.verification_incident(f.project()))
                    .unwrap()
                    .as_ref(),
                Some(&incident)
            );
            for defect in [
                "missing_uuid",
                "old_uuid",
                "wrong_generation",
                "no_preflight",
                "failed_preflight",
                "no_gate",
                "implied_gate",
                "reversed",
                "malformed_gate",
            ] {
                let mut invalid = rows.to_vec();
                match defect {
                    "missing_uuid" => {
                        invalid[0].as_object_mut().unwrap().remove("attempt_id");
                    }
                    "old_uuid" => invalid[0]["attempt_id"] = "previous-attempt".into(),
                    "wrong_generation" => invalid[0]["generation"] = 999999.into(),
                    "no_preflight" => {
                        invalid.remove(1);
                    }
                    "failed_preflight" => invalid[1]["status"] = "failed".into(),
                    "no_gate" => {
                        invalid.remove(2);
                    }
                    "implied_gate" => invalid[2]["path"] = "release gate/clippy".into(),
                    "reversed" => invalid.swap(1, 2),
                    "malformed_gate" => {
                        invalid[2]["status"] = serde_json::json!({"not":"a status"})
                    }
                    _ => unreachable!(),
                }
                std::fs::write(
                    &journal,
                    invalid
                        .iter()
                        .map(|row| format!("{row}\n"))
                        .collect::<String>(),
                )
                .unwrap();
                let projected = status_snapshot_with_incident(
                    &ordered,
                    Some(&active),
                    Some(&incident),
                    f.env(),
                    &now,
                );
                assert!(
                    matches!(projected[0].2, VerificationStatus::Stalled { .. }),
                    "{defect}: {:?}",
                    projected[0].2
                );
                assert!(
                    matches!(
                        projected[1].2,
                        VerificationStatus::Queued {
                            blocked_by: Some(_),
                            ..
                        }
                    ),
                    "{defect}: {:?}",
                    projected[1].2
                );
                assert_eq!(
                    self.activity.status(&f.ctx()).unwrap().held_stories.len(),
                    2,
                    "{defect}"
                );
            }
            std::fs::write(
                &journal,
                rows.iter()
                    .map(|row| format!("{row}\n"))
                    .collect::<String>(),
            )
            .unwrap();
            for terminal in ["passed", "failed", "reused"] {
                let mut completed = rows.to_vec();
                completed[2]["status"] = terminal.into();
                completed.push(serde_json::json!({"kind":"item","path":"land pull request","status":"running","at":now}));
                std::fs::write(
                    &journal,
                    completed
                        .iter()
                        .map(|row| format!("{row}\n"))
                        .collect::<String>(),
                )
                .unwrap();
                let projected = status_snapshot_with_incident(
                    &ordered,
                    Some(&active),
                    Some(&incident),
                    f.env(),
                    &now,
                );
                assert!(
                    matches!(projected[0].2, VerificationStatus::Running { .. }),
                    "{terminal}: {:?}",
                    projected[0].2
                );
            }
            // Completion can record another failure before the owned guard drops.
            for halted in [false, true] {
                let mut latest = incident.clone();
                latest.attempts += 1;
                latest.halted = halted;
                f.store()
                    .write(|tx| tx.put_verification_incident(&latest))
                    .unwrap();
                let status = self.activity.status(&f.ctx()).unwrap();
                assert_eq!(status.held_stories.len(), 2);
                let projected = status_snapshot_with_incident(
                    &ordered,
                    Some(&active),
                    Some(&latest),
                    f.env(),
                    &now,
                );
                assert!(
                    matches!(projected[0].2, VerificationStatus::Stalled { .. }),
                    "new failure: {:?}",
                    projected[0].2
                );
            }
            f.store()
                .write(|tx| tx.put_verification_incident(&incident))
                .unwrap();
            let unowned =
                status_snapshot_with_incident(&ordered, None, Some(&incident), f.env(), &now);
            assert!(
                matches!(unowned[0].2, VerificationStatus::Stalled { .. }),
                "stale journal without owner: {:?}",
                unowned[0].2
            );
        }
        VerificationOutcome::InfrastructureFailure {
            detail: "PR #1 head-ref convergence prerequisite unavailable".into(),
            disposition: VerificationFailureDisposition::Retryable,
        }
    }

    fn notify(&self, _: &VerificationCandidate, _: &str) -> Result<NotifyDelivery, AppError> {
        panic!("retryable infrastructure must not notify the agent")
    }

    fn redispatch(&self, _: &VerificationCandidate, _: &ResumePlan) -> Result<(), AppError> {
        panic!("retryable infrastructure must not redispatch")
    }

    fn reap(&self, _: &VerificationCandidate) -> Result<(), AppError> {
        panic!("unsettled verification must not reap")
    }
}

#[test]
fn authenticated_retry_projects_running_without_erasing_its_failure_history() {
    let f = ServiceFixture::new();
    f.github_checkout("https://github.com/acme/widgets");
    for number in 1..=2 {
        let id = StoryService::new(&f.ctx())
            .create(&NewStoryInput {
                title: format!("retry projection {number}"),
                ..Default::default()
            })
            .unwrap()
            .id;
        PrLinkService::new(&f.ctx())
            .link(
                &id,
                &format!("https://github.com/acme/widgets/pull/{number}"),
                true,
            )
            .unwrap();
        StoryService::new(&f.ctx())
            .set_state(&id, "verifying", None, None, None)
            .unwrap();
    }
    let activity = VerificationActivity::new();
    let actuator = RetryObserver {
        fixture: &f,
        activity: &activity,
        calls: AtomicUsize::new(0),
    };
    std::fs::create_dir_all(f.env().daemon_state_dir()).unwrap();
    let inflight = InFlight::new(f.env().clone());
    for _ in 0..2 {
        assert_eq!(
            tick_with_activity(
                f.store(),
                f.env(),
                &actuator,
                &activity,
                &inflight,
                f.project()
            )
            .unwrap(),
            TickResult::RetryLater
        );
    }
}
