//! Observation scheduling uses the real bounded bus; authority uses real stores.

use super::*;
use crate::service::{NewStoryInput, PrLinkService};
use storyhook_test_support::ServiceFixture;

struct Fixture {
    _seed: ServiceFixture,
    store: crate::store::SqliteStore,
    env: Environment,
    project: ProjectId,
}

impl Fixture {
    fn new() -> Self {
        let seed = ServiceFixture::new();
        seed.link_origin("https://github.com/acme/widgets");
        // Test-support links another crate instance; reopen the real seed
        // with this unit-test crate's types, as the control tests do.
        let store = crate::store::SqliteStore::open(seed.store().path()).unwrap();
        let env = Environment::at(seed.cwd());
        let project = ProjectId::new(seed.project().get());
        Self {
            _seed: seed,
            store,
            env,
            project,
        }
    }

    fn ctx(&self) -> Ctx<'_, crate::store::SqliteStore> {
        Ctx::new(
            &self.store,
            self.project,
            self.env.home().to_path_buf(),
            self.env.clone(),
        )
        .no_hooks(true)
    }

    fn store(&self) -> &crate::store::SqliteStore {
        &self.store
    }
}

fn candidate(f: &Fixture) -> VerificationCandidate {
    let id = StoryService::new(&f.ctx())
        .create(&NewStoryInput {
            title: "Withdrawn verification".into(),
            ..NewStoryInput::default()
        })
        .unwrap()
        .id;
    PrLinkService::new(&f.ctx())
        .link(&id, "https://github.com/acme/widgets/pull/1", true)
        .unwrap();
    StoryService::new(&f.ctx())
        .set_state(&id, "verifying", None, None, None)
        .unwrap();
    VerificationQueue::new(f.store()).next().unwrap().unwrap()
}

#[test]
fn stale_authority_prevents_spawn() {
    let f = Fixture::new();
    let c = candidate(&f);
    StoryService::new(&f.ctx())
        .set_state(&c.story_id, "done", None, None, None)
        .unwrap();
    assert!(
        verify(
            f.store(),
            &ChangeBus::new(),
            &c,
            &Cancellation::default(),
            |_| panic!("stale attempt started")
        )
        .unwrap()
        .is_none()
    );
}

#[test]
fn completed_outcomes_are_rechecked_even_without_a_notification() {
    for outcome in [
        VerificationOutcome::Merged {
            tree: "tree".into(),
            detail: "landed".into(),
            gate: "gate".into(),
        },
        VerificationOutcome::TestsFailed {
            tree: "tree".into(),
            detail: "red".into(),
            gate: "gate".into(),
            log: "log".into(),
        },
        VerificationOutcome::InfrastructureFailure {
            detail: "failed".into(),
            disposition: VerificationFailureDisposition::Permanent,
        },
    ] {
        let f = Fixture::new();
        let c = candidate(&f);
        let result = verify(
            f.store(),
            &ChangeBus::new(),
            &c,
            &Cancellation::default(),
            |_| {
                StoryService::new(&f.ctx())
                    .set_state(&c.story_id, "done", None, None, None)
                    .unwrap();
                outcome
            },
        )
        .unwrap();
        assert!(result.is_none(), "stale outcome was accepted");
    }
}

#[test]
fn unchanged_generation_edits_keep_the_outcome() {
    let f = Fixture::new();
    let c = candidate(&f);
    let bus = ChangeBus::new();
    let outcome = VerificationOutcome::Conflict {
        detail: "expected".into(),
    };
    let result = verify(f.store(), &bus, &c, &Cancellation::default(), |token| {
        StoryService::new(&f.ctx())
            .comment(&c.story_id, "ordinary comment")
            .unwrap();
        bus.publish(Change::Project(c.project_slug.clone()));
        bus.publish(Change::Resync);
        assert!(!token.is_cancelled());
        outcome.clone()
    })
    .unwrap();
    assert_eq!(result, Some(outcome));
}

#[test]
fn missing_events_and_unrelated_flood_cannot_extend_recovery() {
    for flood in [false, true] {
        let bus = ChangeBus::new();
        let subscription = bus.subscribe();
        let done = AtomicBool::new(false);
        let manual = Cancellation::default();
        let attempt = Cancellation::default();
        std::thread::scope(|scope| {
            let _finish = Finish(&done);
            if flood {
                scope.spawn(|| {
                    while !done.load(Ordering::Acquire) {
                        bus.publish(Change::Project("other".into()));
                        std::thread::sleep(Duration::from_millis(1));
                    }
                });
            }
            let started = Instant::now();
            assert!(
                !observe(
                    &subscription,
                    &done,
                    &manual,
                    &attempt,
                    "fixture",
                    Duration::from_millis(80),
                    || Ok(false)
                )
                .unwrap()
            );
            assert!(started.elapsed() < Duration::from_secs(2));
        });
        assert!(attempt.is_cancelled());
        assert!(!manual.is_cancelled());
    }
}

#[test]
fn overflow_rechecks_authority_before_the_recovery_deadline() {
    let bus = ChangeBus::new();
    let subscription = bus.subscribe();
    bus.publish(Change::Project("fixture".into()));
    for _ in 0..64 {
        bus.publish(Change::Ping);
    }
    assert!(subscription.dropped() > 0);
    let attempt = Cancellation::default();
    let started = Instant::now();
    assert!(
        !observe(
            &subscription,
            &AtomicBool::new(false),
            &Cancellation::default(),
            &attempt,
            "fixture",
            Duration::from_secs(30),
            || Ok(false)
        )
        .unwrap()
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(attempt.is_cancelled());
}

#[test]
fn observation_error_cancels_without_poisoning_manual_control() {
    let bus = ChangeBus::new();
    let subscription = bus.subscribe();
    bus.publish(Change::Resync);
    let manual = Cancellation::default();
    let attempt = Cancellation::default();
    let error = observe(
        &subscription,
        &AtomicBool::new(false),
        &manual,
        &attempt,
        "fixture",
        Duration::from_secs(30),
        || Err(AppError::Storage("read failed".into())),
    )
    .unwrap_err();
    assert!(error.to_string().contains("read failed"));
    assert!(attempt.is_cancelled());
    assert!(!manual.is_cancelled());
}

#[test]
fn manual_cancellation_is_propagated_even_without_store_events() {
    let bus = ChangeBus::new();
    let manual = Cancellation::default();
    manual.cancel();
    let attempt = Cancellation::default();
    assert!(
        observe(
            &bus.subscribe(),
            &AtomicBool::new(false),
            &manual,
            &attempt,
            "fixture",
            Duration::from_secs(30),
            || panic!("manual stop needs no store read")
        )
        .unwrap()
    );
    assert!(attempt.is_cancelled());
    assert!(manual.is_cancelled());
}

#[test]
fn panic_in_actuator_stops_and_joins_the_monitor() {
    let f = Fixture::new();
    let c = candidate(&f);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = verify(
            f.store(),
            &ChangeBus::new(),
            &c,
            &Cancellation::default(),
            |_| panic!("actuator panic"),
        );
    }));
    assert!(result.is_err());
}
