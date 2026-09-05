//! Regression coverage for daemon shutdown while centralized verification runs.

use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use storyhook::daemon::lifecycle::{self, InFlight};
use storyhook::daemon::verification::{
    TickResult, VERIFICATION_TIMEOUT, VerificationActivity, VerificationActuator,
    VerificationOutcome, tick_with_activity,
};
use storyhook::domain::Priority;
use storyhook::error::AppError;
use storyhook::service::{
    NewStoryInput, PrLinkService, StoryService, VerificationCandidate, VerificationQueue,
};
use storyhook::store::PrLink;
use storyhook_test_support::ServiceFixture;

const LOW_PR: &str = "https://github.com/acme/widgets/pull/1";
const HIGH_PR: &str = "https://github.com/acme/widgets/pull/2";

fn submitted(fixture: &ServiceFixture, title: &str, priority: Priority, url: &str) -> String {
    let ctx = fixture.ctx();
    let id = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: title.into(),
            priority: Some(priority.as_str().to_string()),
            ..NewStoryInput::default()
        })
        .expect("creating a verification candidate")
        .id;
    PrLinkService::new(&ctx)
        .link(&id, url, true)
        .expect("linking the submitted pull request");
    StoryService::new(&ctx)
        .set_state(&id, "verifying", None, None, None)
        .expect("submitting the story for verification");
    id
}

struct BlockingActuator {
    entered: Sender<String>,
    release: Mutex<Receiver<()>>,
}

impl VerificationActuator for BlockingActuator {
    fn verify(
        &self,
        candidate: &VerificationCandidate,
        _pull_request: &PrLink,
    ) -> VerificationOutcome {
        self.entered
            .send(candidate.story_id.clone())
            .expect("the observer must remain connected");
        self.release
            .lock()
            .expect("locking the release channel")
            .recv_timeout(lifecycle::CONTROL_DEADLINE)
            .expect("the test must release the blocked verifier");
        VerificationOutcome::InfrastructureFailure {
            detail: "deterministic fixture outcome".into(),
        }
    }

    fn notify(&self, _candidate: &VerificationCandidate, _message: &str) -> Result<(), AppError> {
        Ok(())
    }

    fn reap(&self, _candidate: &VerificationCandidate) -> Result<(), AppError> {
        Ok(())
    }
}

#[test]
fn verification_is_published_as_in_flight_until_its_outcome_is_recorded() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let low = submitted(&fixture, "already under test", Priority::Low, LOW_PR);
    let low_generation = VerificationQueue::new(fixture.store())
        .next()
        .expect("reading the original queue")
        .expect("the low-priority candidate must be queued")
        .verifying_generation
        .expect("a verification transition must have a generation")
        .get();
    let activity = VerificationActivity::new();
    std::fs::create_dir_all(fixture.env().daemon_state_dir())
        .expect("creating the daemon state directory");
    let daemon_in_flight = InFlight::new(fixture.env().clone());
    let (entered_tx, entered_rx) = channel();
    let (release_tx, release_rx) = channel();
    let actuator = BlockingActuator {
        entered: entered_tx,
        release: Mutex::new(release_rx),
    };

    let (in_flight_during_verify, result, high) = thread::scope(|scope| {
        let worker = scope.spawn(|| {
            tick_with_activity(
                fixture.store(),
                fixture.env(),
                &actuator,
                &activity,
                &daemon_in_flight,
            )
        });

        assert_eq!(
            entered_rx
                .recv_timeout(lifecycle::CONTROL_DEADLINE)
                .expect("the verifier must reach its blocking actuator"),
            low
        );
        assert_eq!(
            activity
                .active()
                .expect("process-local ownership must be active")
                .story_id,
            low
        );

        let high = submitted(&fixture, "higher priority arrival", Priority::High, HIGH_PR);
        assert_eq!(
            VerificationQueue::new(fixture.store())
                .next()
                .expect("reading the queue")
                .expect("the queue must remain populated")
                .story_id,
            high,
            "the new high-priority story must wait at the head of the queue"
        );

        let published = lifecycle::read_inflight(fixture.env());
        assert_eq!(
            daemon_in_flight.len(),
            published.len(),
            "the daemon registry and its durable publication must agree"
        );
        release_tx.send(()).expect("releasing the blocked verifier");
        let result = worker
            .join()
            .expect("the verifier thread must not panic")
            .expect("the verification tick must finish");
        (published, result, high)
    });

    assert_eq!(result, TickResult::RetryLater);
    assert_eq!(
        activity.active(),
        None,
        "the low-priority story must leave active execution after its outcome is recorded"
    );
    assert!(
        lifecycle::read_inflight(fixture.env()).is_empty(),
        "the verification entry must retract after outcome recording"
    );
    assert_eq!(
        VerificationQueue::new(fixture.store())
            .next()
            .expect("reading the queue after the attempt")
            .expect("the high-priority candidate must remain queued")
            .story_id,
        high,
        "after the low attempt records its outcome, the queue must select the waiting high story"
    );

    assert_eq!(in_flight_during_verify.len(), 1);
    let observed = &in_flight_during_verify[0];
    assert_eq!(observed.command, "verify");
    assert_eq!(
        observed.request_id,
        format!("verify:fixture:{low}:{low_generation}")
    );
    assert_eq!(observed.project.as_deref(), Some("fixture"));
    assert_eq!(observed.pid, std::process::id());
    assert!(!observed.started_at.is_empty());
    assert_eq!(
        observed.served_deadline_secs,
        VERIFICATION_TIMEOUT.as_secs()
    );
    assert_eq!(observed.cwd, std::path::PathBuf::from("/checkouts/fixture"));
}
