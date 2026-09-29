//! Verification batches (SH-831): assembled over a real repository with a
//! scripted GitHub side, gated as one pull request, recorded, and released to
//! the single-story queue; abandoned when a member changes, the operator
//! stops, or the verifier restarts.

use super::batch_preview::{Board, git};
use super::*;
use std::collections::BTreeSet;
use storyhook::daemon::verification::{
    BatchActuator, BatchPublication, BatchRetirement, MemberOwner, VerificationCancellation,
    abandon_interrupted_batches,
};
use storyhook::domain::gate_verdict::GateVerdict;
use storyhook::service::trial_merge::{PrivateTrialMerger, TrialMerger};
use storyhook::store::{
    BatchExclusionReason, BatchId, BatchMember, BatchPhase, BatchPullRequest, VerificationBatch,
};

const BATCH_PR: &str = "https://github.com/acme/widgets/pull/900";

/// Room for the batch observer's periodic authority check (every recovery
/// wake on a bus nobody publishes to) on a loaded machine.
const OBSERVER_PATIENCE: Duration = Duration::from_secs(120);

/// Three stories that merge cleanly with one another.
const CLEAN: [(&str, &str); 3] = [("a", "head\n"), ("b", "second\n"), ("c", "third\n")];

/// How the scripted batch gate behaves.
#[derive(Clone)]
enum Gate {
    /// It answers this outcome.
    Answer(Box<VerificationOutcome>),
    /// It takes the story at this index out of `verifying`, then waits to be
    /// cancelled.
    MemberLeaves(usize),
    /// It stops the verifier, then waits to be cancelled.
    OperatorStops,
}

/// A gate that answers `outcome`.
fn answer(outcome: VerificationOutcome) -> Gate {
    Gate::Answer(Box::new(outcome))
}

/// An actuator that batches: real trial merges and assembly over the board's
/// repository, a scripted GitHub side, and a record of every call.
struct Batcher<'a> {
    board: &'a Board,
    activity: VerificationActivity,
    batching: bool,
    gate: Gate,
    refuse: BTreeSet<String>,
    moved: BTreeSet<String>,
    calls: Mutex<Vec<String>>,
    publications: Mutex<Vec<BatchPublication>>,
}

impl<'a> Batcher<'a> {
    fn new(board: &'a Board, gate: Gate) -> Self {
        Self {
            board,
            activity: VerificationActivity::new(),
            batching: true,
            gate,
            refuse: BTreeSet::new(),
            moved: BTreeSet::new(),
            calls: Mutex::new(Vec::new()),
            publications: Mutex::new(Vec::new()),
        }
    }

    fn call(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn wait_cancelled(cancellation: &VerificationCancellation) -> VerificationOutcome {
        let deadline = Instant::now() + load_grace::graced_now(OBSERVER_PATIENCE);
        while !cancellation.is_cancelled() {
            assert!(
                Instant::now() < deadline,
                "the batch gate was never cancelled"
            );
            thread::sleep(Duration::from_millis(20));
        }
        VerificationOutcome::Cancelled
    }

    fn receipt(&self, candidate: &VerificationCandidate, head: String) -> SubmittedPullRequest {
        let link = candidate.pull_request.clone().expect("a linked PR");
        SubmittedPullRequest {
            url: link.url,
            number: link.number,
            base: "dev".into(),
            head_oid: head,
            adopted: true,
        }
    }

    fn branch_head(&self, candidate: &VerificationCandidate) -> String {
        let branch = &candidate.cleanup_lease.as_ref().expect("a lease").branch;
        git(&self.board.root, &["rev-parse", branch])
    }
}

impl VerificationActuator for Batcher<'_> {
    fn submit(
        &self,
        candidate: &VerificationCandidate,
    ) -> Result<SubmittedPullRequest, SubmissionFailure> {
        self.call(format!("submit {}", candidate.story_id));
        Ok(self.receipt(candidate, self.branch_head(candidate)))
    }

    fn verify(
        &self,
        candidate: &VerificationCandidate,
        pull_request: &PrLink,
    ) -> VerificationOutcome {
        self.call(format!(
            "verify {} {}",
            candidate.story_id, pull_request.url
        ));
        VerificationOutcome::Certified {
            head: "a".repeat(40),
            tree: "b".repeat(40),
            detail: "gate passed".into(),
            gate: "make test".into(),
        }
    }

    fn land(
        &self,
        candidate: &VerificationCandidate,
        _intent: &storyhook::store::LandingIntent,
    ) -> storyhook::daemon::verification::LandingOutcome {
        self.call(format!("land {}", candidate.story_id));
        storyhook::daemon::verification::LandingOutcome::Merged {
            detail: "test merge confirmed".into(),
        }
    }

    fn recover_landing(
        &self,
        _candidate: &VerificationCandidate,
        _intent: &storyhook::store::LandingIntent,
    ) -> storyhook::daemon::verification::LandingOutcome {
        panic!("these tests leave no landing authority unresolved")
    }

    fn notify(
        &self,
        _candidate: &VerificationCandidate,
        _message: &str,
    ) -> Result<NotifyDelivery, AppError> {
        Ok(NotifyDelivery::Delivered)
    }

    fn redispatch(
        &self,
        _candidate: &VerificationCandidate,
        _plan: &ResumePlan,
    ) -> Result<(), AppError> {
        panic!("a delivered notification never re-dispatches")
    }

    fn reap(&self, _candidate: &VerificationCandidate) -> Result<(), AppError> {
        Ok(())
    }

    fn trial_merges(
        &self,
        repository: &Path,
        deadline: Instant,
        cancellation: &VerificationCancellation,
    ) -> Option<Result<Box<dyn TrialMerger>, AppError>> {
        Some(PrivateTrialMerger::open(repository).map(|merger| {
            Box::new(
                merger
                    .with_deadline(deadline)
                    .with_cancellation(cancellation.clone()),
            ) as Box<dyn TrialMerger>
        }))
    }

    fn batch(&self) -> Option<&dyn BatchActuator> {
        self.batching.then_some(self as &dyn BatchActuator)
    }
}

impl BatchActuator for Batcher<'_> {
    fn submit_member(
        &self,
        member: &VerificationCandidate,
        _owner: MemberOwner<'_>,
        _cancellation: &VerificationCancellation,
    ) -> Result<SubmittedPullRequest, SubmissionFailure> {
        self.call(format!("submit-member {}", member.story_id));
        if self.refuse.contains(&member.story_id) {
            return Err(SubmissionFailure::Refused {
                reason: "dirty-worktree".into(),
                display: "fixture: the worktree has uncommitted changes".into(),
            });
        }
        let head = if self.moved.contains(&member.story_id) {
            "f".repeat(40)
        } else {
            self.branch_head(member)
        };
        Ok(self.receipt(member, head))
    }

    fn publish(
        &self,
        _head: &VerificationCandidate,
        publication: &BatchPublication,
        _cancellation: &VerificationCancellation,
    ) -> Result<BatchPullRequest, AppError> {
        self.call(format!("publish {}", publication.branch));
        self.publications.lock().unwrap().push(publication.clone());
        Ok(BatchPullRequest {
            url: BATCH_PR.into(),
            number: 900,
        })
    }

    fn gate(
        &self,
        _head: &VerificationCandidate,
        pull_request: &PrLink,
        cancellation: &VerificationCancellation,
    ) -> VerificationOutcome {
        self.call(format!("gate {}", pull_request.url));
        match &self.gate {
            Gate::Answer(outcome) => (**outcome).clone(),
            Gate::MemberLeaves(index) => {
                StoryService::new(&self.board.fixture.ctx())
                    .set_state(
                        &self.board.stories[*index],
                        "in-progress",
                        Some("fixture: the agent takes the story back"),
                        None,
                        None,
                    )
                    .unwrap();
                Self::wait_cancelled(cancellation)
            }
            Gate::OperatorStops => {
                self.activity
                    .control(
                        self.board.fixture.store(),
                        self.board.fixture.project(),
                        VerificationAction::Stop,
                    )
                    .unwrap();
                Self::wait_cancelled(cancellation)
            }
        }
    }

    fn retire(
        &self,
        _head: &VerificationCandidate,
        batch: &VerificationBatch,
        _comment: &str,
    ) -> Result<BatchRetirement, AppError> {
        self.call(format!(
            "retire {} {}",
            batch.id,
            batch
                .pull_request
                .as_ref()
                .map_or("-", |pull_request| pull_request.url.as_str())
        ));
        Ok(BatchRetirement {
            closed: batch.pull_request.is_some(),
            merged: false,
            deleted: true,
        })
    }
}

/// A board whose repository has the identity batch merge commits need.
fn board(stories: &[(&str, &str)], lanes: Option<u32>) -> Board {
    let board = Board::new(stories);
    git(&board.root, &["config", "user.name", "t"]);
    git(&board.root, &["config", "user.email", "t@t"]);
    if let Some(lanes) = lanes {
        board.live_run(lanes);
    }
    board
}

fn tick(board: &Board, batcher: &Batcher<'_>) -> TickResult {
    let env = board.env();
    tick_with_activity(
        board.fixture.store(),
        &env,
        batcher,
        &batcher.activity,
        &InFlight::new(env.clone()),
        board.fixture.project(),
    )
    .unwrap()
}

fn batches(board: &Board) -> Vec<VerificationBatch> {
    board
        .fixture
        .store()
        .read(|tx| tx.verification_batches(board.fixture.project()))
        .unwrap()
}

fn queued(board: &Board) -> Vec<(String, Option<GlobalSeq>)> {
    VerificationQueue::new(board.fixture.store())
        .ordered_for(board.fixture.project())
        .unwrap()
        .into_iter()
        .map(|candidate| (candidate.story_id, candidate.verifying_generation))
        .collect()
}

fn submitted_comments(board: &Board, id: &str) -> usize {
    story_row(&board.fixture, id)
        .snapshot
        .comments
        .iter()
        .filter(|comment| comment.text.starts_with(VERIFICATION_SUBMITTED_PREFIX))
        .count()
}

fn member_ids(batch: &VerificationBatch) -> Vec<String> {
    batch.members.iter().map(|m| m.story_id.clone()).collect()
}

fn certified_batch() -> VerificationOutcome {
    VerificationOutcome::Certified {
        head: "d".repeat(40),
        tree: "e".repeat(40),
        detail: "batch gate passed".into(),
        gate: "make test".into(),
    }
}

#[test]
fn a_green_batch_is_released_and_the_head_is_then_gated_alone() {
    let board = board(&CLEAN, Some(3));
    let ids = board.stories.clone();
    let before = queued(&board);
    let batcher = Batcher::new(&board, answer(certified_batch()));

    assert_eq!(tick(&board, &batcher), TickResult::Completed);

    let batch = &batches(&board)[0];
    let calls = batcher.calls();
    assert_eq!(
        calls,
        [
            format!("submit {}", ids[0]),
            format!("submit-member {}", ids[1]),
            format!("submit-member {}", ids[2]),
            format!("publish {}", batch.branch),
            format!("gate {BATCH_PR}"),
            format!("retire {} {BATCH_PR}", batch.id),
            format!("verify {} https://github.com/acme/widgets/pull/1", ids[0]),
            format!("land {}", ids[0]),
        ],
        "the batch gate runs first, then the head's own gate on its own PR"
    );
    assert_eq!(batch.phase, BatchPhase::Released);
    assert!(batch.retired);
    assert_eq!(member_ids(batch), ids);
    let gate = batch.gate.as_ref().expect("the verdict is recorded");
    assert_eq!(gate.verdict, GateVerdict::Certified);
    assert_eq!(gate.tree.as_deref(), Some("e".repeat(40).as_str()));
    assert_eq!(
        batch.pull_request,
        Some(BatchPullRequest {
            url: BATCH_PR.into(),
            number: 900
        })
    );
    assert_eq!(batch.branch, format!("storyhook/verify-batch/{}", batch.id));
    assert_eq!(
        batch.base_commit,
        git(&board.root, &["rev-parse", "origin/dev"])
    );
    for (member, id) in batch.members.iter().zip(&ids) {
        assert_eq!(
            member.head_commit,
            git(&board.root, &["rev-parse", &format!("worktree-{id}")])
        );
        assert!(
            storyhook::env::git_env::command(&board.root)
                .args([
                    "merge-base",
                    "--is-ancestor",
                    &member.head_commit,
                    &batch.tip
                ])
                .status()
                .unwrap()
                .success(),
            "{id} is reachable from the batch tip"
        );
    }
    assert_eq!(
        git(
            &board.root,
            &["rev-list", "--first-parent", "--count", &batch.tip]
        ),
        "4",
        "base, then one merge commit per member"
    );

    let publication = &batcher.publications.lock().unwrap()[0];
    assert_eq!(publication.tip, batch.tip);
    assert_eq!(publication.base, "dev");
    assert!(
        publication.title.contains(&ids.join(", ")),
        "{}",
        publication.title
    );
    for number in ["#1", "#2", "#3"] {
        assert!(publication.body.contains(number), "{}", publication.body);
    }
    // Linking a member is a plain `#N` reference, never a closing keyword:
    // a member pull request closes when its own story lands, not the batch's.
    let body = publication.body.to_lowercase();
    for keyword in [
        "close", "closes", "closed", "fix", "fixes", "fixed", "resolve", "resolves", "resolved",
    ] {
        assert!(!body.contains(&format!("{keyword} #")), "{body}");
    }

    // Members are back in the single-story queue exactly as they were, with
    // the submission the batch recorded; the head landed on its own gate.
    let after = queued(&board);
    assert_eq!(after, before[1..].to_vec());
    for id in &ids[1..] {
        assert_eq!(submitted_comments(&board, id), 1, "{id}");
    }
    assert_eq!(story_row(&board.fixture, &ids[0]).state, "done");

    let record = &board.records()[0];
    assert_eq!(record["batch"]["id"], batch.id.as_str());
    assert_eq!(record["batch"]["verdict"], "certified");
    assert_eq!(record["batch"]["phase"], "released");
    assert_eq!(record["verdict"], "certified", "the head's own gate");
}

#[test]
fn a_red_batch_is_released_with_its_verdict_and_changes_no_member() {
    let board = board(&CLEAN, Some(3));
    let before = queued(&board);
    let batcher = Batcher::new(
        &board,
        answer(VerificationOutcome::TestsFailed {
            tree: "c".repeat(40),
            log: "/tmp/batch.log".into(),
            detail: "1 failed".into(),
            gate: "make test".into(),
        }),
    );

    assert_eq!(tick(&board, &batcher), TickResult::Completed);

    let batch = &batches(&board)[0];
    assert_eq!(batch.phase, BatchPhase::Released);
    let gate = batch.gate.as_ref().unwrap();
    assert_eq!(gate.verdict, GateVerdict::TestsFailed);
    assert_eq!(gate.tree.as_deref(), Some("c".repeat(40).as_str()));
    assert!(gate.detail.contains("/tmp/batch.log"), "{}", gate.detail);
    assert_eq!(queued(&board), before[1..].to_vec());
    assert!(batcher.calls().contains(&format!(
        "verify {} https://github.com/acme/widgets/pull/1",
        board.stories[0]
    )));
}

#[test]
fn no_batch_forms_without_a_partner_or_when_batching_is_off() {
    // No live Full Auto run: the cap is one, so the head is alone.
    let alone = board(&CLEAN, None);
    let batcher = Batcher::new(&alone, answer(certified_batch()));
    assert_eq!(tick(&alone, &batcher), TickResult::Completed);
    assert!(batches(&alone).is_empty());
    assert!(
        !batcher
            .calls()
            .iter()
            .any(|call| call.starts_with("submit-member"))
    );

    // The same queue with batching off: the actuator offers no batch.
    let off = board(&CLEAN, Some(3));
    let mut batcher = Batcher::new(&off, answer(certified_batch()));
    batcher.batching = false;
    assert_eq!(tick(&off, &batcher), TickResult::Completed);
    assert!(batches(&off).is_empty());
    assert_eq!(
        batcher.calls(),
        [
            format!("submit {}", off.stories[0]),
            format!(
                "verify {} https://github.com/acme/widgets/pull/1",
                off.stories[0]
            ),
            format!("land {}", off.stories[0]),
        ]
    );
}

#[test]
fn members_that_cannot_join_are_left_out_and_a_batch_of_one_is_never_recorded() {
    let board = board(&CLEAN, Some(3));
    let ids = board.stories.clone();
    let mut batcher = Batcher::new(&board, answer(certified_batch()));
    batcher.moved.insert(ids[1].clone());

    assert_eq!(tick(&board, &batcher), TickResult::Completed);

    let batch = &batches(&board)[0];
    assert_eq!(member_ids(batch), [ids[0].clone(), ids[2].clone()]);
    assert_eq!(batch.excluded.len(), 1);
    assert_eq!(batch.excluded[0].story_id, ids[1]);
    assert_eq!(batch.excluded[0].reason, BatchExclusionReason::HeadMoved);
    assert_eq!(
        batch.members.iter().map(|m| m.position).collect::<Vec<_>>(),
        [0, 1]
    );

    let pair = board_pair();
    let mut batcher = Batcher::new(&pair, answer(certified_batch()));
    batcher.refuse.insert(pair.stories[1].clone());
    assert_eq!(tick(&pair, &batcher), TickResult::Completed);
    assert!(
        batches(&pair).is_empty(),
        "one member left: no record, no pull request"
    );
    assert!(
        !batcher
            .calls()
            .iter()
            .any(|call| call.starts_with("publish"))
    );
    let record = &pair.records()[0];
    assert!(record["batch"].get("id").is_none(), "{record}");
    assert!(
        record["batch"]["detail"]
            .as_str()
            .unwrap()
            .contains("fewer than two members")
    );
}

fn board_pair() -> Board {
    board(&CLEAN[..2], Some(2))
}

#[test]
fn a_member_that_leaves_verifying_during_the_gate_abandons_the_batch() {
    let board = board(&CLEAN, Some(3));
    let ids = board.stories.clone();
    let batcher = Batcher::new(&board, Gate::MemberLeaves(2));

    assert_eq!(tick(&board, &batcher), TickResult::Completed);

    let batch = &batches(&board)[0];
    assert_eq!(batch.phase, BatchPhase::Abandoned);
    assert!(
        batch.detail.as_deref().unwrap().contains(&ids[2]),
        "{batch:?}"
    );
    assert_eq!(batch.gate.as_ref().unwrap().verdict, GateVerdict::Withdrawn);
    assert!(batch.retired, "an abandoned batch is retired too");
    let calls = batcher.calls();
    assert!(
        calls.contains(&format!(
            "verify {} https://github.com/acme/widgets/pull/1",
            ids[0]
        )),
        "the head is still gated alone: {calls:?}"
    );
}

#[test]
fn an_operator_stop_during_the_batch_gate_interrupts_the_head() {
    let board = board(&CLEAN, Some(3));
    let ids = board.stories.clone();
    let batcher = Batcher::new(&board, Gate::OperatorStops);

    assert_eq!(tick(&board, &batcher), TickResult::Stopped);

    let batch = &batches(&board)[0];
    assert_eq!(batch.phase, BatchPhase::Abandoned);
    assert_eq!(
        batch.gate.as_ref().unwrap().verdict,
        GateVerdict::Interrupted
    );
    assert!(
        !batch.retired,
        "a stopped verifier does not reach GitHub again"
    );
    let calls = batcher.calls();
    assert!(
        !calls.iter().any(|call| call.starts_with("verify")),
        "{calls:?}"
    );
    assert!(
        !calls.iter().any(|call| call.starts_with("retire")),
        "{calls:?}"
    );
    let head = story_row(&board.fixture, &ids[0]);
    assert_eq!(head.state, "verifying");
    assert!(
        head.snapshot
            .comments
            .iter()
            .any(|comment| comment.text.contains("INTERRUPTED")),
        "the head's attempt was interrupted during a gate"
    );
    assert_eq!(board.records()[0]["verdict"], "interrupted");
}

#[test]
fn a_batch_gate_cleanup_failure_halts_the_queue() {
    use storyhook::daemon::verification::{CompletedVerification, VerificationCleanupFailure};
    let board = board(&CLEAN, Some(3));
    let cleanup = VerificationCleanupFailure {
        phase: "outer census".into(),
        detail: "a survivor held the worktree".into(),
        owner: Some("/state/owner.json".into()),
        worktree: Some("/common/storyhook/verification-worktree".into()),
        disposition: VerificationFailureDisposition::Permanent,
    };
    let batcher = Batcher::new(
        &board,
        answer(VerificationOutcome::CleanupFailed {
            verdict: CompletedVerification::GatePassed {
                tree: "e".repeat(40),
                log: "/tmp/batch.log".into(),
                detail: "passed".into(),
                gate: "make test".into(),
            },
            cleanup,
        }),
    );

    assert_eq!(tick(&board, &batcher), TickResult::Halted);

    let incident = board
        .fixture
        .store()
        .read(|tx| tx.verification_incident(board.fixture.project()))
        .unwrap()
        .expect("the queue halts on an incident");
    assert!(incident.halted);
    assert!(
        incident.detail.contains("verification batch"),
        "{}",
        incident.detail
    );
    assert!(incident.detail.contains("a survivor held the worktree"));
    let batch = &batches(&board)[0];
    assert_eq!(batch.phase, BatchPhase::Released);
    assert_eq!(
        batch.gate.as_ref().unwrap().verdict,
        GateVerdict::CleanupFailed
    );
    assert!(
        !batcher
            .calls()
            .iter()
            .any(|call| call.starts_with("verify"))
    );
}

/// A batch of the board's first two stories, recorded live in `Gating` as a
/// verifier that stopped mid-gate would leave it.
fn live_batch(board: &Board) -> VerificationBatch {
    let ids = board.stories.clone();
    let members: Vec<BatchMember> = queued(board)
        .iter()
        .take(2)
        .enumerate()
        .map(|(position, (id, generation))| BatchMember {
            story: StoryNo::parse_id("SH", id).unwrap(),
            story_id: id.clone(),
            generation: generation.unwrap(),
            head_commit: git(&board.root, &["rev-parse", &format!("worktree-{id}")]),
            pull_request: format!("https://github.com/acme/widgets/pull/{}", position + 1),
            position: position as u32,
        })
        .collect();
    let id = BatchId::generate();
    let left = VerificationBatch {
        branch: id.branch(),
        id,
        project: board.fixture.project(),
        project_slug: board.slug(),
        head: ids[0].clone(),
        base_branch: "dev".into(),
        base_commit: git(&board.root, &["rev-parse", "origin/dev"]),
        tip: git(&board.root, &["rev-parse", "origin/dev"]),
        pull_request: Some(BatchPullRequest {
            url: "https://github.com/acme/widgets/pull/899".into(),
            number: 899,
        }),
        phase: BatchPhase::Gating,
        members,
        excluded: Vec::new(),
        gate: None,
        detail: None,
        retired: false,
        revision: 0,
        created_at: FIXTURE_NOW.into(),
        updated_at: FIXTURE_NOW.into(),
    };
    board
        .fixture
        .store()
        .write(|tx| tx.insert_verification_batch(&left))
        .unwrap();
    left
}

#[test]
fn a_batch_left_live_by_a_restart_is_abandoned_and_retired_by_the_next_batch() {
    let board = board(&CLEAN, Some(3));
    let before = queued(&board);
    let left = live_batch(&board);

    let abandoned =
        abandon_interrupted_batches(board.fixture.store(), &board.env(), board.fixture.project())
            .unwrap();

    assert_eq!(abandoned, std::slice::from_ref(&left.id));
    let after = &batches(&board)[0];
    assert_eq!(after.phase, BatchPhase::Abandoned);
    assert!(after.detail.as_deref().unwrap().contains("stopped before"));
    assert_eq!(
        queued(&board),
        before,
        "members keep their generations and stay queued"
    );
    assert!(
        abandon_interrupted_batches(board.fixture.store(), &board.env(), board.fixture.project())
            .unwrap()
            .is_empty(),
        "an ended batch is never abandoned twice"
    );

    let batcher = Batcher::new(&board, answer(certified_batch()));
    assert_eq!(tick(&board, &batcher), TickResult::Completed);
    let calls = batcher.calls();
    assert_eq!(
        calls[1],
        format!(
            "retire {} https://github.com/acme/widgets/pull/899",
            left.id
        ),
        "the leftover is retired before the next batch forms: {calls:?}"
    );
    let all = batches(&board);
    assert_eq!(all.len(), 2);
    assert!(all[0].retired);
    assert_eq!(all[1].phase, BatchPhase::Released);
}

#[test]
fn the_verifier_worker_abandons_a_batch_left_live_when_it_starts() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use storyhook::daemon::bus::{Change, ChangeBus};
    use storyhook::daemon::verification::poll_verification_with;

    let board = board(&CLEAN, Some(3));
    let left = live_batch(&board);
    let bus = ChangeBus::new();
    let activity = VerificationActivity::new().with_bus(bus.clone());
    // Admission stopped: the worker starts, settles its restart, and
    // verifies nothing.
    activity
        .control(
            board.fixture.store(),
            board.fixture.project(),
            VerificationAction::Stop,
        )
        .unwrap();
    let env = board.env();
    let inflight = InFlight::new(env.clone());
    let stop = AtomicBool::new(false);
    let abandoned = std::thread::scope(|scope| {
        scope.spawn(|| {
            poll_verification_with(
                board.fixture.store(),
                &env,
                &bus,
                &stop,
                &activity,
                &inflight,
                |_| Batcher::new(&board, answer(certified_batch())),
            )
        });
        let deadline = Instant::now() + load_grace::graced_now(OBSERVER_PATIENCE);
        let abandoned = loop {
            if batches(&board)[0].phase == BatchPhase::Abandoned {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            thread::sleep(Duration::from_millis(20));
        };
        // Always release the worker before asserting.
        stop.store(true, Ordering::Relaxed);
        bus.publish(Change::Resync);
        abandoned
    });
    assert!(abandoned, "the worker never abandoned batch {}", left.id);
    assert_eq!(
        batches(&board)[0]
            .detail
            .as_deref()
            .map(|d| d.contains("stopped before")),
        Some(true)
    );
}
