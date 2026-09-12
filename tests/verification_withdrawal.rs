//! State withdrawal through the real supervisor, store and subprocess capture.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use storyhook::daemon::bus::{Change, ChangeBus};
use storyhook::daemon::lifecycle::{InFlight, read_owned_processes};
use storyhook::daemon::verification::{
    ShellVerificationActuator, VerificationActivity, journal_path, poll_verification_with,
};
use storyhook::domain::Priority;
use storyhook::service::verification_control::VerificationAction;
use storyhook::service::{
    NewStoryInput, PrLinkService, StoryService, VerificationCandidate, VerificationQueue,
};
use storyhook::store::{ReadOps, Store, WriteOps};
use storyhook_test_support::ServiceFixture;

fn submitted(f: &ServiceFixture, title: &str, number: u64) -> VerificationCandidate {
    let id = StoryService::new(&f.ctx())
        .create(&NewStoryInput {
            title: title.into(),
            priority: Some(Priority::High.as_str().into()),
            ..NewStoryInput::default()
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
    VerificationQueue::new(f.store())
        .ordered_for(f.project())
        .unwrap()
        .into_iter()
        .find(|c| c.story_id == id)
        .unwrap()
}

fn marker(f: &ServiceFixture, c: &VerificationCandidate, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}.{suffix}", journal_path(f.env(), c).display()))
}

fn wait_for(description: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !ready() {
        assert!(Instant::now() < deadline, "{description}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

struct StopOnDrop<'a> {
    fixture: &'a ServiceFixture,
    stop: &'a AtomicBool,
    bus: &'a ChangeBus,
    activity: &'a VerificationActivity,
}

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.activity
            .control(
                self.fixture.store(),
                self.fixture.project(),
                VerificationAction::Stop,
            )
            .unwrap();
        self.bus.publish(Change::Catalog);
    }
}

fn run_with_gate(
    test: impl FnOnce(
        &ServiceFixture,
        &ChangeBus,
        &VerificationActivity,
        &VerificationCandidate,
        &VerificationCandidate,
    ),
) {
    let f = ServiceFixture::new();
    f.link_origin("https://github.com/acme/widgets");
    f.store()
        .write(|tx| tx.set_checkout_path(f.project(), Some(f.cwd())))
        .unwrap();
    for args in [
        vec!["init", "-q"],
        vec![
            "config",
            "remote.origin.url",
            "https://github.com/acme/widgets",
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(f.cwd())
                .status()
                .unwrap()
                .success()
        );
    }
    let first = submitted(&f, "Operator-owned decision", 1);
    let second = submitted(&f, "Next queued change", 2);
    let script = f.cwd().join("gate-probe.sh");
    std::fs::write(
        &script,
        r#"[ "$STORYHOOK_VERIFIER_CLEANUP_GRACE_MS" = 5000 ] || exit 99
trap 'printf terminated > "$STORYHOOK_GATE_PROGRESS.terminated"; exit 0' TERM
printf ready > "$STORYHOOK_GATE_PROGRESS.started"
while :; do sleep 30; done
"#,
    )
    .unwrap();
    let activity = VerificationActivity::new();
    let inflight = InFlight::new(f.env().clone());
    let bus = ChangeBus::new();
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let _cleanup = StopOnDrop {
            fixture: &f,
            stop: &stop,
            bus: &bus,
            activity: &activity,
        };
        scope.spawn(|| {
            poll_verification_with(
                f.store(),
                f.env(),
                &bus,
                &stop,
                &activity,
                &inflight,
                |_| {
                    ShellVerificationActuator::with_paths_and_timing(
                        f.env().clone(),
                        Path::new("/unused-helper").into(),
                        Path::new("/unused-story").into(),
                        Duration::from_secs(20),
                        Duration::from_secs(2),
                        Duration::from_secs(5),
                    )
                    .with_verifier_script(script.clone())
                    .with_activity(activity.clone())
                },
            )
        });
        wait_for("first gate never started", || {
            marker(&f, &first, "started").exists()
        });
        test(&f, &bus, &activity, &first, &second);
    });
    assert!(activity.active_all().is_empty());
    assert!(read_owned_processes(f.env()).is_empty());
    assert!(
        f.store()
            .read(|tx| tx.verification_incident(f.project()))
            .unwrap()
            .is_none()
    );
}

#[test]
fn leaving_verifying_interrupts_the_gate_and_advances_the_queue() {
    run_with_gate(|f, bus, activity, first, second| {
        StoryService::new(&f.ctx())
            .set_state(&first.story_id, "done", None, Some("verifying"), None)
            .unwrap();
        bus.publish(Change::Project(first.project_slug.clone()));
        wait_for("withdrawn gate was not terminated", || {
            marker(f, first, "terminated").exists()
        });
        wait_for("the next queued gate did not start", || {
            marker(f, second, "started").exists()
        });
        assert_eq!(
            activity.active_for(f.project()).unwrap().story_id,
            second.story_id
        );
        assert!(
            f.store()
                .read(|tx| tx.verification_enabled(f.project()))
                .unwrap()
        );
        assert!(
            VerificationQueue::new(f.store())
                .ordered_for(f.project())
                .unwrap()
                .iter()
                .all(|c| c.story_id != first.story_id)
        );
    });
}

#[test]
fn rapid_resubmission_interrupts_only_the_old_generation() {
    run_with_gate(|f, bus, activity, first, second| {
        std::fs::remove_file(marker(f, first, "started")).unwrap();
        StoryService::new(&f.ctx())
            .set_state(
                &first.story_id,
                "in-progress",
                None,
                Some("verifying"),
                None,
            )
            .unwrap();
        StoryService::new(&f.ctx())
            .set_state(
                &first.story_id,
                "verifying",
                None,
                Some("in-progress"),
                None,
            )
            .unwrap();
        // Only the final snapshot is observable; history still distinguishes it.
        bus.publish(Change::Resync);
        wait_for("superseded gate was not terminated", || {
            marker(f, first, "terminated").exists()
        });
        wait_for("replacement generation did not start", || {
            marker(f, first, "started").exists()
        });
        let active = activity.active_for(f.project()).unwrap();
        assert_eq!(active.story_id, first.story_id);
        assert_ne!(active.generation, first.verifying_generation);
        assert!(!marker(f, second, "started").exists());
        assert!(
            f.store()
                .read(|tx| tx.verification_enabled(f.project()))
                .unwrap()
        );
    });
}

#[test]
fn nonterminal_withdrawal_preserves_admission_and_operator_state() {
    run_with_gate(|f, bus, _, first, second| {
        StoryService::new(&f.ctx())
            .set_state(
                &first.story_id,
                "in-progress",
                None,
                Some("verifying"),
                None,
            )
            .unwrap();
        bus.publish(Change::Catalog);
        wait_for("returned gate was not terminated", || {
            marker(f, first, "terminated").exists()
        });
        wait_for("queue did not advance after withdrawal", || {
            marker(f, second, "started").exists()
        });
        // A compare-and-set against the chosen state proves it was not overwritten.
        StoryService::new(&f.ctx())
            .set_state(&first.story_id, "todo", None, Some("in-progress"), None)
            .unwrap();
        assert!(
            f.store()
                .read(|tx| tx.verification_enabled(f.project()))
                .unwrap()
        );
    });
}
