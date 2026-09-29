//! Bisection of a red verification batch (SH-833; spec B7): over a real
//! repository with a scripted GitHub side whose gate judges each gated
//! commit by which culprit heads it contains, with the true merge tree. A
//! culprit is found at every position, returned to its own agent with its
//! own tree, log and batch; the certified members before it land; the rest
//! stay queued; and nothing but a red prefix tree ever blames a story.

use super::batch_harness::*;
use super::batch_preview::{Board, git};
use super::*;
use storyhook::daemon::verification::abandon_interrupted_batches;
use storyhook::domain::gate_verdict::GateVerdict;
use storyhook::store::{
    BatchBisection, BatchPhase, BisectionOf, BisectionOutcome, ProbeKind, VerificationBatch,
};

/// Four stories that merge cleanly with one another; the fourth adds a file.
const STORIES: [(&str, &str); 4] = [
    ("a", "head\n"),
    ("b", "second\n"),
    ("c", "third\n"),
    ("d", "fourth\n"),
];

const RED: &str = "CENTRAL VERIFICATION RED —";

/// A board of the first `members` stories whose live run's lanes let all of
/// them batch.
fn board_of(members: usize) -> Board {
    board(&STORIES[..members], Some(members as u32))
}

/// The batch the queue formed: the one a bisection records.
fn parent(board: &Board) -> VerificationBatch {
    batches(board)
        .into_iter()
        .find(|batch| batch.bisects.is_none())
        .expect("the queue formed a batch")
}

/// The probe batches, in the order they were recorded.
fn probes(board: &Board) -> Vec<VerificationBatch> {
    batches(board)
        .into_iter()
        .filter(|batch| batch.bisects.is_some())
        .collect()
}

fn bisection(batch: &VerificationBatch) -> &BatchBisection {
    batch
        .bisection
        .as_ref()
        .expect("the red batch was bisected")
}

fn reds(board: &Board, id: &str) -> Vec<String> {
    story_row(&board.fixture, id)
        .snapshot
        .comments
        .iter()
        .filter(|comment| comment.text.starts_with(RED))
        .map(|comment| comment.text.clone())
        .collect()
}

fn search_runs(batch: &VerificationBatch) -> usize {
    bisection(batch)
        .probes
        .iter()
        .filter(|probe| probe.kind == ProbeKind::Search)
        .count()
}

fn gate_calls(batcher: &Batcher<'_>) -> usize {
    batcher
        .calls()
        .iter()
        .filter(|call| call.starts_with("gate "))
        .count()
}

fn ceil_log2(members: usize) -> usize {
    (usize::BITS - (members - 1).leading_zeros()) as usize
}

/// No story but `culprit` (an index, if any) has a RED comment.
fn only_blamed(board: &Board, culprit: Option<usize>) {
    for (index, id) in board.stories.iter().enumerate() {
        if Some(index) != culprit {
            assert!(reds(board, id).is_empty(), "{id} was blamed");
        }
    }
}

/// One red batch of `members` stories whose culprit is at 1-based
/// `position`: the culprit is returned with its own tree, log and batch, the
/// members before it land, the members after it stay queued as they were,
/// and the search stays within its bound.
fn a_single_culprit(members: usize, position: usize) {
    let board = board_of(members);
    let ids = board.stories.clone();
    let before = queued(&board);
    let batcher = Batcher::new(&board, Gate::Culprits(vec![position - 1]));

    let result = tick(&board, &batcher);

    let context = format!("k={members} culprit at {position}");
    let parent = parent(&board);
    assert_eq!(parent.phase, BatchPhase::Released, "{context}");
    assert_eq!(
        parent.gate.as_ref().unwrap().verdict,
        GateVerdict::TestsFailed,
        "{context}"
    );
    assert!(parent.retired, "{context}");
    let Some(BisectionOutcome::Culprit {
        story_id,
        position: found,
        tree,
        log,
        certified,
        ..
    }) = bisection(&parent).outcome.clone()
    else {
        panic!("{context}: {:?}", parent.bisection);
    };
    assert_eq!(story_id, ids[position - 1], "{context}");
    assert_eq!(found as usize, position, "{context}");
    assert_eq!(certified as usize, position - 1, "{context}");
    assert_eq!(
        Some(&tree),
        parent.members[position - 1].merge_tree.as_ref(),
        "{context}: the red tree is the prefix that ends with the culprit"
    );
    assert!(
        search_runs(&parent) <= ceil_log2(members),
        "{context}: {:?}",
        bisection(&parent).probes
    );

    let red = reds(&board, &ids[position - 1]);
    assert_eq!(red.len(), 1, "{context}: {red:?}");
    for named in [tree.as_str(), log.as_str(), parent.id.as_str()] {
        assert!(red[0].contains(named), "{context}: {named} in {}", red[0]);
    }
    assert_eq!(
        story_row(&board.fixture, &ids[position - 1]).state,
        "in-progress"
    );
    only_blamed(&board, Some(position - 1));

    let calls = batcher.calls();
    let head_gated_alone = calls
        .iter()
        .any(|call| call.starts_with(&format!("verify {}", ids[0])));
    match position - 1 {
        0 => {
            assert_eq!(result, TickResult::Returned, "{context}");
            assert!(
                !head_gated_alone,
                "{context}: the head's red probe is its verdict: {calls:?}"
            );
        }
        1 => {
            assert_eq!(result, TickResult::Completed, "{context}");
            assert!(head_gated_alone, "{context}: {calls:?}");
            assert_eq!(story_row(&board.fixture, &ids[0]).state, "done");
        }
        landed => {
            assert_eq!(result, TickResult::Completed, "{context}");
            assert!(!head_gated_alone, "{context}: {calls:?}");
            for id in &ids[..landed] {
                assert_eq!(
                    story_row(&board.fixture, id).state,
                    "done",
                    "{context}: {id}"
                );
            }
            let probe = probes(&board)
                .into_iter()
                .find(|probe| probe.phase == BatchPhase::Landed)
                .expect("the certified prefix landed as a probe batch");
            assert_eq!(
                probe.bisects,
                Some(BisectionOf {
                    parent: parent.id.clone(),
                    prefix: landed as u32
                }),
                "{context}"
            );
        }
    }
    let after = queued(&board);
    for entry in &before[position..] {
        assert!(
            after.contains(entry),
            "{context}: {} stays queued at its generation: {after:?}",
            entry.0
        );
    }
    for probe in probes(&board) {
        assert!(!probe.phase.is_live(), "{context}: {probe:?}");
    }
}

#[test]
fn a_culprit_is_found_at_every_position_of_a_batch_of_two() {
    for position in 1..=2 {
        a_single_culprit(2, position);
    }
}

#[test]
fn a_culprit_is_found_at_every_position_of_a_batch_of_three() {
    for position in 1..=3 {
        a_single_culprit(3, position);
    }
}

#[test]
fn a_culprit_is_found_at_every_position_of_a_batch_of_four() {
    for position in 1..=4 {
        a_single_culprit(4, position);
    }
}

/// A receipt for the head alone makes the second member the culprit with
/// no probe gate at all: the head's own gate reuses the receipt and lands.
#[test]
fn a_receipt_finds_the_culprit_without_a_probe_gate() {
    let board = board_of(2);
    let ids = board.stories.clone();
    let mut batcher = Batcher::new(&board, Gate::Culprits(vec![1]));
    batcher.receipts.insert(1);

    assert_eq!(tick(&board, &batcher), TickResult::Completed);

    assert_eq!(
        gate_calls(&batcher),
        1,
        "only the batch gate: {:?}",
        batcher.calls()
    );
    let parent = parent(&board);
    let probes = &bisection(&parent).probes;
    assert_eq!(probes.len(), 1, "{probes:?}");
    assert_eq!((probes[0].prefix, probes[0].kind), (1, ProbeKind::Receipt));
    assert_eq!(search_runs(&parent), 0);
    assert_eq!(reds(&board, &ids[1]).len(), 1);
    assert!(
        reds(&board, &ids[1])[0].contains("gate receipt"),
        "the RED says how the head was certified"
    );
    assert_eq!(story_row(&board.fixture, &ids[0]).state, "done");
}

/// A receipt for the first two of three members: the third is the culprit
/// without a search gate, and the certified pair lands through a fresh
/// probe batch whose gate the receipt makes a reuse.
#[test]
fn a_receipt_for_a_longer_prefix_lands_it_through_a_fresh_probe() {
    let board = board_of(3);
    let ids = board.stories.clone();
    let mut batcher = Batcher::new(&board, Gate::Culprits(vec![2]));
    batcher.receipts.insert(2);

    assert_eq!(tick(&board, &batcher), TickResult::Completed);

    let calls = batcher.calls();
    assert!(calls.contains(&"certified 2".to_owned()), "{calls:?}");
    assert!(
        !calls.contains(&"certified 1".to_owned()),
        "the largest certified prefix ends the receipt check: {calls:?}"
    );
    let parent = parent(&board);
    assert_eq!(search_runs(&parent), 0);
    let kinds: Vec<_> = bisection(&parent)
        .probes
        .iter()
        .map(|probe| (probe.prefix, probe.kind))
        .collect();
    assert_eq!(kinds, [(2, ProbeKind::Receipt), (2, ProbeKind::Landing)]);
    assert_eq!(gate_calls(&batcher), 2, "the batch and the landing probe");
    for id in &ids[..2] {
        assert_eq!(story_row(&board.fixture, id).state, "done", "{id}");
    }
    assert_eq!(story_row(&board.fixture, &ids[2]).state, "in-progress");
    only_blamed(&board, Some(2));
}

/// Two independent culprits: the first is returned, the members before it
/// land, and the second stays queued to meet its own next gate.
#[test]
fn of_two_culprits_the_first_is_returned_and_the_second_stays_queued() {
    let board = board_of(3);
    let ids = board.stories.clone();
    let before = queued(&board);
    let batcher = Batcher::new(&board, Gate::Culprits(vec![1, 2]));

    assert_eq!(tick(&board, &batcher), TickResult::Completed);

    assert_eq!(reds(&board, &ids[1]).len(), 1);
    only_blamed(&board, Some(1));
    assert_eq!(story_row(&board.fixture, &ids[0]).state, "done");
    let third = before.iter().find(|entry| entry.0 == ids[2]).unwrap();
    assert!(queued(&board).contains(third), "{:?}", queued(&board));
}

/// An infrastructure failure of a probe blames nobody: the bisection is
/// inconclusive, every member stays queued, and the head takes its own gate.
#[test]
fn an_infrastructure_failure_during_bisection_blames_no_story() {
    for (members, culprit) in [(3, 2), (4, 3)] {
        let board = board_of(members);
        let ids = board.stories.clone();
        let before = queued(&board);
        let batcher = Batcher::new(&board, Gate::Culprits(vec![culprit]));
        // The first probe: the head alone of three, a probe batch of two of four.
        batcher.fault(
            1,
            answer(VerificationOutcome::InfrastructureFailure {
                detail: "fixture: the gate's disk is full".into(),
                disposition: VerificationFailureDisposition::Retryable,
            }),
        );

        assert_eq!(tick(&board, &batcher), TickResult::Completed);

        only_blamed(&board, None);
        let parent = parent(&board);
        let Some(BisectionOutcome::Inconclusive { detail }) = &bisection(&parent).outcome else {
            panic!("{:?}", parent.bisection);
        };
        assert!(detail.contains("infrastructure"), "{detail}");
        assert_eq!(story_row(&board.fixture, &ids[0]).state, "done");
        assert_eq!(queued(&board), before[1..].to_vec(), "k={members}");
        for probe in probes(&board) {
            assert_eq!(probe.phase, BatchPhase::Released);
            assert!(probe.retired);
            assert_eq!(
                probe.gate.as_ref().unwrap().verdict,
                GateVerdict::InfrastructureFailure
            );
        }
    }
}

/// A probe judged on another tree (the base moved) blames nobody.
#[test]
fn a_probe_red_on_another_tree_blames_no_story() {
    let board = board_of(2);
    let batcher = Batcher::new(&board, Gate::Culprits(vec![1]));
    batcher.fault(
        1,
        answer(VerificationOutcome::TestsFailed {
            tree: "c".repeat(40),
            log: "/tmp/moved.log".into(),
            detail: "fixture: red on a moved base".into(),
            gate: "make test".into(),
        }),
    );

    assert_eq!(tick(&board, &batcher), TickResult::Completed);

    only_blamed(&board, None);
    let parent = parent(&board);
    assert!(matches!(
        bisection(&parent).outcome,
        Some(BisectionOutcome::Inconclusive { .. })
    ));
}

/// An operator stop during a probe interrupts the head, returns nobody and
/// leaves retirement to the next batch.
#[test]
fn an_operator_stop_during_a_probe_interrupts_the_head_and_blames_nobody() {
    let board = board_of(3);
    let ids = board.stories.clone();
    let batcher = Batcher::new(&board, Gate::Culprits(vec![2]));
    batcher.fault(1, Gate::OperatorStops);

    assert_eq!(tick(&board, &batcher), TickResult::Stopped);

    only_blamed(&board, None);
    let parent = parent(&board);
    assert!(matches!(
        bisection(&parent).outcome,
        Some(BisectionOutcome::Interrupted { .. })
    ));
    assert!(
        !parent.retired,
        "a stopped verifier does not reach GitHub again"
    );
    let head = story_row(&board.fixture, &ids[0]);
    assert_eq!(head.state, "verifying");
    assert!(
        head.snapshot
            .comments
            .iter()
            .any(|comment| comment.text.contains("INTERRUPTED"))
    );
}

/// A member still in the search that changes during a probe cancels it:
/// the bisection is inconclusive and blames nobody.
#[test]
fn a_member_in_the_search_that_changes_ends_the_bisection_without_blame() {
    let board = board_of(3);
    let batcher = Batcher::new(&board, Gate::Culprits(vec![2]));
    batcher.fault(1, Gate::MemberLeaves(1));

    assert_eq!(tick(&board, &batcher), TickResult::Completed);

    only_blamed(&board, None);
    let parent = parent(&board);
    assert!(matches!(
        bisection(&parent).outcome,
        Some(BisectionOutcome::Inconclusive { .. })
    ));
}

/// A member the search already put back in the queue may change freely: the
/// probe it no longer belongs to runs on and finds the culprit.
#[test]
fn a_member_outside_the_search_that_changes_does_not_stop_it() {
    let board = board_of(4);
    let ids = board.stories.clone();
    let batcher = Batcher::new(&board, Gate::Culprits(vec![0]));
    // Gate 1 is prefix 2 (red: members 3 and 4 leave the search); gate 2 is
    // the head alone, while member 4 leaves verifying.
    batcher.fault(
        2,
        Gate::Then {
            action: Action::Leaves(3),
            then: Box::new(Gate::Culprits(vec![0])),
        },
    );

    assert_eq!(tick(&board, &batcher), TickResult::Returned);

    assert_eq!(reds(&board, &ids[0]).len(), 1);
    only_blamed(&board, Some(0));
    assert_eq!(story_row(&board.fixture, &ids[3]).state, "in-progress");
}

/// A probe gate that cannot clean up halts the queue, as a batch gate's does.
#[test]
fn a_probe_cleanup_failure_halts_the_queue() {
    use storyhook::daemon::verification::{CompletedVerification, VerificationCleanupFailure};
    let board = board_of(2);
    let batcher = Batcher::new(&board, Gate::Culprits(vec![1]));
    batcher.fault(
        1,
        answer(VerificationOutcome::CleanupFailed {
            verdict: CompletedVerification::GatePassed {
                tree: "e".repeat(40),
                log: "/tmp/probe.log".into(),
                detail: "passed".into(),
                gate: "make test".into(),
            },
            cleanup: VerificationCleanupFailure {
                phase: "outer census".into(),
                detail: "a survivor held the worktree".into(),
                owner: None,
                worktree: None,
                disposition: VerificationFailureDisposition::Permanent,
            },
        }),
    );

    assert_eq!(tick(&board, &batcher), TickResult::Halted);

    let incident = board
        .fixture
        .store()
        .read(|tx| tx.verification_incident(board.fixture.project()))
        .unwrap()
        .expect("the queue halts on an incident");
    assert!(incident.detail.contains("a survivor held the worktree"));
    only_blamed(&board, None);
}

/// A culprit a person holds by the time it is found is not returned; the
/// members before it still land.
#[test]
fn a_culprit_a_person_holds_is_not_returned() {
    let board = board_of(3);
    let ids = board.stories.clone();
    let mut batcher = Batcher::new(&board, Gate::Culprits(vec![2]));
    batcher.receipts.insert(2);
    // Gate 1 lands the certified pair; the culprit is frozen by then, so a
    // label on it does not cancel that gate.
    batcher.fault(
        1,
        Gate::Then {
            action: Action::Holds(2),
            then: Box::new(Gate::Culprits(vec![2])),
        },
    );

    assert_eq!(tick(&board, &batcher), TickResult::Completed);

    only_blamed(&board, None);
    assert_eq!(story_row(&board.fixture, &ids[2]).state, "verifying");
    for id in &ids[..2] {
        assert_eq!(story_row(&board.fixture, id).state, "done", "{id}");
    }
    let parent = parent(&board);
    let Some(BisectionOutcome::Culprit { detail, .. }) = &bisection(&parent).outcome else {
        panic!("{:?}", parent.bisection);
    };
    assert!(detail.contains("a person holds it"), "{detail}");
}

/// A culprit whose agent is absent is re-dispatched under its own lock and
/// its diagnosis pasted again, as a single-story return is.
#[test]
fn an_absent_culprit_agent_is_redispatched() {
    let board = board_of(2);
    let ids = board.stories.clone();
    let mut batcher = Batcher::new(&board, Gate::Culprits(vec![1]));
    batcher.absent.insert(ids[1].clone());

    assert_eq!(tick(&board, &batcher), TickResult::Completed);

    let calls = batcher.calls();
    let delivery: Vec<_> = calls
        .iter()
        .filter(|call| call.starts_with("notify-member ") || call.starts_with("redispatch-member "))
        .cloned()
        .collect();
    assert_eq!(
        delivery,
        [
            format!("notify-member {}", ids[1]),
            format!("redispatch-member {}", ids[1]),
            format!("notify-member {}", ids[1]),
        ]
    );
    assert!(
        story_row(&board.fixture, &ids[1])
            .snapshot
            .comments
            .iter()
            .any(|comment| comment.text.starts_with("CENTRAL VERIFICATION RESUME"))
    );
}

/// A project recovery that starts during the bisection needs the head's own
/// admitted gate, so a red head alone is not handed to the tick.
#[test]
fn a_recovery_that_starts_during_bisection_sends_the_head_to_its_own_gate() {
    let board = board_of(2);
    let ids = board.stories.clone();
    let batcher = Batcher::new(&board, Gate::Culprits(vec![0]));
    batcher.fault(
        1,
        Gate::Then {
            action: Action::Recovers,
            then: Box::new(Gate::Culprits(vec![0])),
        },
    );

    tick(&board, &batcher);

    assert!(
        batcher
            .calls()
            .iter()
            .any(|call| call.starts_with(&format!("verify {}", ids[0]))),
        "{:?}",
        batcher.calls()
    );
    let parent = parent(&board);
    let Some(BisectionOutcome::Culprit { detail, .. }) = &bisection(&parent).outcome else {
        panic!("{:?}", parent.bisection);
    };
    assert!(detail.contains("project recovery"), "{detail}");
}

/// Status reads `bisecting` with the members still in the search: the
/// members a red prefix puts back in the queue read as queued again, not
/// as running in the batch. The per-dequeue record carries the bisection.
#[test]
fn status_and_the_record_show_the_bisection() {
    let board = board_of(4);
    let ids = board.stories.clone();
    let batcher = Batcher::new(&board, Gate::Culprits(vec![0]));

    assert_eq!(tick(&board, &batcher), TickResult::Returned);

    let parent = parent(&board);
    let seen = batcher.probe_status.lock().unwrap().clone();
    // Gate 1 is prefix 2 (red), gate 2 the head alone.
    let expected = [(1, ids.clone()), (2, ids[..2].to_vec())];
    assert_eq!(seen.len(), expected.len(), "{seen:?}");
    for ((call, data, text), (want_call, members)) in seen.iter().zip(expected) {
        assert_eq!(*call, want_call);
        let shown = &data["verifier"]["batch"];
        assert_eq!(shown["phase"], "bisecting", "{shown}");
        assert_eq!(shown["id"], parent.id.as_str());
        assert_eq!(shown["members"], serde_json::json!(members), "gate {call}");
        assert!(text.contains("bisecting"), "{text}");
        for id in &ids[1..] {
            let verification = data["stories"]
                .as_array()
                .unwrap()
                .iter()
                .find(|view| view["story"]["id"] == id.as_str())
                .unwrap()["verification"]
                .clone();
            assert_eq!(
                verification.get("batch").is_some(),
                members.contains(id),
                "gate {call}: {id}: {verification}"
            );
        }
    }
    let record = &board.records()[0];
    assert_eq!(record["batch"]["bisection"]["outcome"]["kind"], "culprit");
    assert_eq!(
        record["batch"]["bisection"]["outcome"]["story_id"],
        ids[0].as_str()
    );
}

/// A verifier that stopped mid-bisection leaves a live probe and a parent
/// with no outcome; the next worker start settles both, once.
#[test]
fn a_restart_settles_a_live_probe_and_an_unfinished_bisection() {
    let board = board_of(3);
    let parent = gating_batch(&board);
    let store = board.fixture.store();
    let mut released = parent.advance(BatchPhase::Released, FIXTURE_NOW).unwrap();
    released.bisection = Some(BatchBisection::default());
    assert!(
        store
            .write(|tx| tx.update_verification_batch(&released, parent.revision))
            .unwrap()
    );
    let id = storyhook::store::BatchId::generate();
    let probe = VerificationBatch {
        bisects: Some(BisectionOf {
            parent: parent.id.clone(),
            prefix: 2,
        }),
        bisection: None,
        branch: id.branch(),
        id,
        members: parent.members[..2].to_vec(),
        phase: BatchPhase::Gating,
        revision: 0,
        ..parent.clone()
    };
    store
        .write(|tx| tx.insert_verification_batch(&probe))
        .unwrap();

    let settled =
        abandon_interrupted_batches(store, &board.env(), board.fixture.project()).unwrap();

    assert_eq!(settled, [parent.id.clone(), probe.id.clone()]);
    let all = batches(&board);
    assert!(matches!(
        bisection(&all[0]).outcome,
        Some(BisectionOutcome::Interrupted { .. })
    ));
    assert_eq!(all[0].phase, BatchPhase::Released);
    assert_eq!(all[1].phase, BatchPhase::Abandoned);
    assert!(
        abandon_interrupted_batches(store, &board.env(), board.fixture.project())
            .unwrap()
            .is_empty(),
        "settled once"
    );
}

/// A batch of every queued story, recorded live in `Gating` as a verifier
/// that stopped mid-gate would leave it.
fn gating_batch(board: &Board) -> VerificationBatch {
    use storyhook::store::{BatchId, BatchMember, BatchPullRequest, StoryNo};
    let members: Vec<BatchMember> = queued(board)
        .iter()
        .enumerate()
        .map(|(position, (id, generation))| BatchMember {
            story: StoryNo::parse_id("SH", id).unwrap(),
            story_id: id.clone(),
            generation: generation.unwrap(),
            head_commit: git(&board.root, &["rev-parse", &format!("worktree-{id}")]),
            pull_request: format!("https://github.com/acme/widgets/pull/{}", position + 1),
            position: position as u32,
            branch: None,
            merge_commit: None,
            merge_tree: None,
        })
        .collect();
    let id = BatchId::generate();
    let batch = VerificationBatch {
        branch: id.branch(),
        id,
        project: board.fixture.project(),
        project_slug: board.slug(),
        head: members[0].story_id.clone(),
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
        bisects: None,
        bisection: None,
        retired: false,
        revision: 0,
        created_at: FIXTURE_NOW.into(),
        updated_at: FIXTURE_NOW.into(),
    };
    board
        .fixture
        .store()
        .write(|tx| tx.insert_verification_batch(&batch))
        .unwrap();
    batch
}
