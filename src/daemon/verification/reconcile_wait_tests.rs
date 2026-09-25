//! SH-769: a conflict-reconcile reservation waits on the store alone.
//!
//! The waiter used to read [`VerificationQueue::ordered_for`] on every 100 ms
//! pass and on every bus wake. That read resolves the checkout origin with
//! `git` whenever a queued story links a pull request, so one long reconcile
//! in moshtail started about 1,000 processes a minute. Two seams count what
//! the waiter starts on its own thread: the activity journal, which records
//! every child the daemon runners start and is how the defect was measured,
//! and the `git` tally, which also sees the `git` the journal never records.

use super::*;
use crate::daemon::activity::context::{LogContext, enter};
use crate::daemon::lifecycle::CONTROL_DEADLINE;
use crate::domain::Priority;
use crate::domain::remote::RemoteUrl;
use crate::env::git_env::built_on_this_thread;
use crate::service::{NewStoryInput, PrLinkService};
use crate::store::SqliteStore;
use std::path::Path;
use std::sync::mpsc::channel;
use storyhook_test_support::{ServiceFixture, scratch_dir};

const ORIGIN: &str = "https://github.com/acme/widgets";
const REPLACEMENT: &str = "https://github.com/acme/replacement";
const HELD_PR: &str = "https://github.com/acme/widgets/pull/1";
const QUEUED_PR: &str = "https://github.com/acme/widgets/pull/2";
const ARRIVAL_PR: &str = "https://github.com/acme/replacement/pull/3";
const RETURNED_PR: &str = "https://github.com/acme/widgets/pull/4";

/// Creates a story at `priority`, links `url` as its close-on-merge pull
/// request, and submits it to `verifying`.
fn submitted(ctx: &Ctx<'_, SqliteStore>, title: &str, priority: Priority, url: &str) -> String {
    let id = StoryService::new(ctx)
        .create(&NewStoryInput {
            title: title.into(),
            priority: Some(priority.as_str().to_string()),
            ..NewStoryInput::default()
        })
        .unwrap()
        .id;
    PrLinkService::new(ctx).link(&id, url, true).unwrap();
    StoryService::new(ctx)
        .set_state(&id, "verifying", None, None, None)
        .unwrap();
    id
}

/// The process records the daemon runners journaled under `logs`. Each child
/// they start is journaled with a `child=<pid>` context.
fn child_records(logs: &Path) -> Vec<String> {
    let days = match std::fs::read_dir(logs) {
        Ok(days) => days,
        // The first record creates the directory: nothing was journaled.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => panic!("reading journal {}: {error}", logs.display()),
    };
    let mut records = Vec::new();
    for day in days {
        let path = day.expect("a journal directory entry").path();
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
        records.extend(
            text.lines()
                .filter(|line| {
                    let record: serde_json::Value =
                        serde_json::from_str(line).expect("a journal record");
                    record["context"]
                        .as_str()
                        .is_some_and(|context| context.contains(" child="))
                })
                .map(str::to_string),
        );
    }
    records
}

/// Stops the waiter when the test thread leaves the scope, so a failed
/// assertion ends the wait instead of hanging the scope's join.
struct StopOnExit<'a>(&'a AtomicBool);

impl Drop for StopOnExit<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

#[test]
fn a_reconcile_wait_starts_no_process_until_its_story_resubmits() {
    let fixture = ServiceFixture::new();
    fixture.github_checkout(ORIGIN);
    // Unit tests link a second crate instance through test-support. Reopen its
    // seeded database with this crate's types.
    let store = SqliteStore::open(fixture.store().path()).unwrap();
    let project = ProjectId::new(fixture.project().get());
    let ctx = Ctx::new(
        &store,
        project,
        fixture.cwd(),
        Environment::at(fixture.cwd()),
    )
    .no_hooks(true);
    let held = submitted(&ctx, "reserved", Priority::Low, HELD_PR);
    let reserved = VerificationQueue::new(&store)
        .ordered_for(project)
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.story_id == held)
        .expect("the held story is queued");
    StoryService::new(&ctx)
        .set_state(&held, "in-progress", None, Some("verifying"), None)
        .unwrap();
    // The trigger: a queued story whose pull request an origin check resolves.
    submitted(&ctx, "queued", Priority::Medium, QUEUED_PR);
    // Repoint the origin. From here only origin validation can turn the held
    // story's link into `UnregisteredPullRequest`.
    store
        .write(|tx| {
            tx.unlink_remote(project, &RemoteUrl::normalize(ORIGIN).unwrap())
                .map(|_| ())
        })
        .unwrap();
    fixture.github_checkout(REPLACEMENT);

    let journal = scratch_dir();
    let logs = journal.path().join("logs");
    let bus = ChangeBus::new();
    let subscription = bus.subscribe();
    let stop = AtomicBool::new(false);
    let idle = Cancellation::default();
    let (idle_tx, idle_rx) = channel();
    let (resumed_tx, resumed_rx) = channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let _journal = enter(Some(LogContext {
                directory: logs.clone(),
                label: "project=fixture reservation".into(),
            }));
            let before = built_on_this_thread();
            let result = wait_for_reconciled_candidate_cancellable(
                &store,
                &subscription,
                &stop,
                &reserved,
                &idle,
            );
            idle_tx
                .send((result, built_on_this_thread() - before))
                .unwrap();
            let result = wait_for_reconciled_candidate_cancellable(
                &store,
                &subscription,
                &stop,
                &reserved,
                &Cancellation::default(),
            );
            resumed_tx
                .send((result, built_on_this_thread() - before))
                .unwrap();
        });
        let _stop = StopOnExit(&stop);

        // A higher-priority arrival reorders the queue, as MT-24 did.
        submitted(&ctx, "arrival", Priority::Critical, ARRIVAL_PR);
        for _ in 0..10 {
            bus.publish(Change::Project("fixture".into()));
            bus.publish(Change::Ping);
        }
        let early = idle_rx.recv_timeout(CONTROL_DEADLINE / 10);
        idle.cancel();
        assert!(
            early.is_err(),
            "an arrival or a wake must not end the reservation wait: {early:?}"
        );
        let (idle_result, idle_git) = idle_rx
            .recv_timeout(CONTROL_DEADLINE)
            .expect("cancellation ends the idle wait");
        assert!(
            idle_result.unwrap().is_none(),
            "a cancelled wait returns no candidate"
        );
        assert_eq!(idle_git, 0, "an idle reconcile wait must build no git");
        let idle_children = child_records(&logs);
        assert!(
            idle_children.is_empty(),
            "an idle reconcile wait must start no process: {idle_children:#?}"
        );

        StoryService::new(&ctx)
            .set_state(&held, "verifying", None, Some("in-progress"), None)
            .unwrap();
        bus.publish(Change::Project("fixture".into()));
        let (resumed, total_git) = resumed_rx
            .recv_timeout(CONTROL_DEADLINE)
            .expect("the resubmission ends the wait promptly");
        let resumed = resumed
            .unwrap()
            .expect("the reserved story's resubmission wakes its waiter");
        assert_eq!(resumed.story_id, held);
        assert!(resumed.verifying_generation.is_some());
        assert_ne!(resumed.verifying_generation, reserved.verifying_generation);
        assert_eq!(
            resumed.pull_request,
            Err(VerificationProblem::UnregisteredPullRequest {
                url: HELD_PR.into(),
                registered: vec!["github.com/acme/replacement".into()],
            }),
            "the waiter returns the resubmission as the origin-validated queue reports it"
        );
        // One origin check on return: both seams must see it, or their zeros
        // above prove nothing.
        assert!(
            total_git > 0,
            "the git tally must see the origin check on return"
        );
        assert!(
            !child_records(&logs).is_empty(),
            "the journal must see the origin check on return"
        );
    });
}

#[test]
fn the_store_only_generation_read_agrees_with_the_validated_queue() {
    let fixture = ServiceFixture::new();
    fixture.github_checkout(ORIGIN);
    let store = SqliteStore::open(fixture.store().path()).unwrap();
    let project = ProjectId::new(fixture.project().get());
    let ctx = Ctx::new(
        &store,
        project,
        fixture.cwd(),
        Environment::at(fixture.cwd()),
    )
    .no_hooks(true);
    let verifying = submitted(&ctx, "verifying", Priority::High, HELD_PR);
    let human = submitted(&ctx, "human-only", Priority::High, QUEUED_PR);
    StoryService::new(&ctx)
        .set_labels(&human, &["human-only".into()], &[])
        .unwrap();
    let returned = submitted(&ctx, "returned", Priority::High, RETURNED_PR);
    StoryService::new(&ctx)
        .set_state(&returned, "in-progress", None, Some("verifying"), None)
        .unwrap();
    let unknown = format!("{}-999", verifying.split('-').next().unwrap());

    let queue = VerificationQueue::new(&store);
    let ordered = queue.ordered_for(project).unwrap();
    let template = ordered
        .iter()
        .find(|candidate| candidate.story_id == verifying)
        .cloned()
        .expect("the verifying story is queued");
    assert!(
        queue.current_generation_for(&template).unwrap().is_some(),
        "a queued story reports its generation"
    );
    for id in [&verifying, &human, &returned, &unknown] {
        let probe = VerificationCandidate {
            story_id: id.clone(),
            ..template.clone()
        };
        let validated = ordered
            .iter()
            .find(|candidate| &candidate.story_id == id)
            .and_then(|candidate| candidate.verifying_generation);
        assert_eq!(
            queue.current_generation_for(&probe).unwrap(),
            validated,
            "{id}: the store-only read must report what the validated queue reports"
        );
    }
}
