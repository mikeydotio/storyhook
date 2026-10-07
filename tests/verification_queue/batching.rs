//! Verification batches (SH-831): assembled over a real repository with a
//! scripted GitHub side, gated as one pull request, recorded, and released to
//! the single-story queue; abandoned when a member changes, the operator
//! stops, or the verifier restarts.

use super::batch_harness::*;
use super::batch_preview::{Board, git};
use super::*;
use storyhook::daemon::verification::{LandingOutcome, abandon_interrupted_batches};
use storyhook::domain::gate_verdict::GateVerdict;
use storyhook::store::{
    BatchExclusionReason, BatchId, BatchMember, BatchPhase, BatchPullRequest, VerificationBatch,
};

#[test]
fn a_partner_already_landed_completes_without_a_repair_return() {
    let board = board(&CLEAN, Some(3));
    let partner = board.stories[1].clone();
    let mut batcher = Batcher::new(&board, answer(certified_batch()));
    batcher.landed.insert(partner.clone());
    let _ = tick(&board, &batcher);
    let row = story_row(&board.fixture, &partner);
    assert_eq!(row.state, "done");
    assert!(
        row.snapshot
            .comments
            .iter()
            .any(|c| c.text.starts_with("CENTRAL VERIFICATION ALREADY LANDED —"))
    );
    assert!(
        !batcher
            .calls()
            .iter()
            .any(|call| call.contains(&format!("notify {partner}"))
                || call.contains(&format!("redispatch {partner}")))
    );
}

/// A batch whose gate certified some other head than the batch tip never
/// lands: its admission is refused, it is released with the reason, and the
/// head is gated alone (SH-832 D4).
#[test]
fn a_batch_certified_at_another_head_is_released_and_the_head_is_gated_alone() {
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
            "base-policy dev".to_owned(),
            format!("submit-member {}", ids[1]),
            format!("submit-member {}", ids[2]),
            format!("publish {}", batch.branch),
            format!("gate {BATCH_PR}"),
            format!("retire {} {BATCH_PR}", batch.id),
            format!("verify {} https://github.com/acme/widgets/pull/1", ids[0]),
            format!("land {} https://github.com/acme/widgets/pull/1", ids[0]),
        ],
        "the batch gate runs first, then the head's own gate on its own PR"
    );
    assert_eq!(batch.phase, BatchPhase::Released);
    assert!(batch.retired);
    assert_eq!(member_ids(batch), ids);
    let detail = batch.detail.as_deref().unwrap_or_default();
    assert!(
        detail.contains("landing was refused") && detail.contains("not the batch tip"),
        "{detail}"
    );
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
    // A certified batch lands (SH-832): the body must not tell a reader the
    // batch pull request is always closed.
    assert!(
        publication
            .body
            .contains("If the gate certifies it, the verifier lands this pull request")
            && !publication.body.contains("not built yet"),
        "{}",
        publication.body
    );
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

/// A red verdict on a tree other than the batch tip's (the base moved)
/// says nothing about the batch's own trees: it is released without a
/// bisection (SH-833 D9) and changes no member.
#[test]
fn a_red_batch_on_another_tree_is_released_unbisected_and_changes_no_member() {
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
    assert_eq!(
        batch.bisection, None,
        "no bisection of a tree the batch did not make"
    );
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
            format!(
                "land {} https://github.com/acme/widgets/pull/1",
                off.stories[0]
            ),
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
            merge_commit: None,
            merge_tree: None,
            resolution: None,
            story: StoryNo::parse_id("SH", id).unwrap(),
            story_id: id.clone(),
            generation: generation.unwrap(),
            head_commit: git(&board.root, &["rev-parse", &format!("worktree-{id}")]),
            pull_request: format!("https://github.com/acme/widgets/pull/{}", position + 1),
            position: position as u32,
            branch: None,
        })
        .collect();
    let id = BatchId::generate();
    let left = VerificationBatch {
        bisects: None,
        bisection: None,
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
        withdrawn: Vec::new(),
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
        // Never panic while the worker runs: the scope would wait on it.
        let mut patience = Patience::new(OBSERVER_PATIENCE);
        let abandoned = loop {
            if batches(&board)[0].phase == BatchPhase::Abandoned {
                break true;
            }
            if patience.expired() {
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

fn landing_batcher(board: &Board) -> Batcher<'_> {
    Batcher::new(board, Gate::CertifiesTip)
}

fn comments_with(board: &Board, id: &str, prefix: &str) -> Vec<String> {
    story_row(&board.fixture, id)
        .snapshot
        .comments
        .iter()
        .filter(|comment| comment.text.starts_with(prefix))
        .map(|comment| comment.text.clone())
        .collect()
}

fn landing_intents(board: &Board) -> Vec<storyhook::store::LandingIntent> {
    board
        .fixture
        .store()
        .read(|tx| tx.landing_intents())
        .unwrap()
}

/// A certified batch lands through its own pull request, every member is
/// done in one transaction and reaped under its own lock, and the head is
/// never gated alone (SH-832 B5, B6).
#[test]
fn a_certified_batch_lands_and_every_member_has_durable_cleanup() {
    let board = board(&CLEAN, Some(3));
    let ids = board.stories.clone();
    let batcher = landing_batcher(&board);

    assert_eq!(tick(&board, &batcher), TickResult::Completed);

    let batch = &batches(&board)[0];
    assert_eq!(batch.phase, BatchPhase::Landed);
    assert!(batch.retired, "{:?}", batch.detail);
    assert_eq!(
        batch.gate.as_ref().map(|gate| gate.verdict),
        Some(GateVerdict::Certified)
    );
    let branches: Vec<String> = ids.iter().map(|id| format!("worktree-{id}")).collect();
    assert_eq!(
        batcher.calls(),
        [
            format!("submit {}", ids[0]),
            "base-policy dev".to_owned(),
            format!("submit-member {}", ids[1]),
            format!("submit-member {}", ids[2]),
            format!("publish {}", batch.branch),
            format!("gate {BATCH_PR}"),
            format!("land {} {BATCH_PR}", ids[0]),
            format!("prune-members {}", branches.join(" ")),
        ],
        "one merge of the batch pull request; no single gate for the head"
    );
    assert!(landing_intents(&board).is_empty());
    assert!(queued(&board).is_empty());
    for id in &ids {
        assert_eq!(story_row(&board.fixture, id).state, "done", "{id}");
        let green = comments_with(&board, id, storyhook::service::VERIFICATION_GREEN_PREFIX);
        assert_eq!(green.len(), 1, "{id}: {green:?}");
        assert!(green[0].contains(&format!("verification batch {}", batch.id)));
        assert!(green[0].contains(BATCH_PR), "{}", green[0]);
        assert_eq!(
            comments_with(&board, id, "CENTRAL VERIFICATION CLEANUP COMPLETE").len(),
            0,
            "{id} is not reaped by the verifier"
        );
        let no = StoryNo::parse_id("SH", id).unwrap();
        let request = board
            .fixture
            .store()
            .read(|tx| tx.closure_cleanup(board.fixture.project(), no))
            .unwrap()
            .unwrap();
        assert!(!request.completed);
    }
    let record = &board.records()[0];
    assert_eq!(record["batch"]["phase"], "landed", "{record}");
    assert_eq!(record["verdict"], "certified");

    // B9: while the batch gates and lands, status names it and each member
    // reads `running` with the batch; the head keeps its own running status.
    let observed = batcher.observed.lock().unwrap().clone();
    assert_eq!(
        observed.len(),
        2,
        "one read at the gate, one at the landing"
    );
    for ((data, text), phase) in observed.iter().zip(["gating", "landing"]) {
        let shown = &data["verifier"]["batch"];
        assert_eq!(shown["id"], batch.id.as_str(), "{shown}");
        assert_eq!(shown["head"], ids[0].as_str());
        assert_eq!(shown["members"], serde_json::json!(ids));
        assert_eq!(shown["phase"], phase);
        let story = |id: &str| {
            data["stories"]
                .as_array()
                .unwrap()
                .iter()
                .find(|view| view["story"]["id"] == id)
                .unwrap()["verification"]
                .clone()
        };
        assert!(
            story(&ids[0]).get("batch").is_none(),
            "the head is the owner"
        );
        for id in &ids[1..] {
            let member = story(id);
            if phase == "gating" {
                assert_eq!(member["status"], "running", "{id}: {member}");
                assert_eq!(member["batch"], batch.id.as_str(), "{id}: {member}");
                assert_eq!(member["head"], ids[0].as_str());
                assert_eq!(member["phase"], "gating");
            } else {
                assert_eq!(
                    member["status"], "landingpending",
                    "a fenced member reads as landing: {member}"
                );
            }
        }
        assert!(
            text.contains(&format!(
                "Verification batch {} running: {} · {phase}",
                batch.id,
                ids.join(", ")
            )),
            "{text}"
        );
    }
    assert!(
        batcher
            .activity
            .status(&board.fixture.ctx())
            .unwrap()
            .batch
            .is_none(),
        "no batch once it landed"
    );
}

/// A landing whose outcome is uncertain leaves every member fenced in
/// `verifying`; a restarted verifier does not abandon the batch and
/// recovers the merge once from the intents, then completes and reaps every
/// member (B10: a crash between the merge and the completion).
#[test]
fn an_uncertain_batch_landing_is_recovered_from_its_intents_after_a_restart() {
    let board = board(&CLEAN, Some(3));
    let ids = board.stories.clone();
    let mut batcher = landing_batcher(&board);
    batcher.landing = LandingOutcome::Uncertain {
        detail: "the daemon stopped after the merge request".into(),
    };

    assert_eq!(tick(&board, &batcher), TickResult::RetryLater);

    let batch = batches(&board)[0].clone();
    assert_eq!(batch.phase, BatchPhase::Landing);
    assert_eq!(landing_intents(&board).len(), 3);
    for id in &ids {
        assert_eq!(story_row(&board.fixture, id).state, "verifying", "{id}");
        assert_eq!(
            comments_with(&board, id, "CENTRAL LANDING PENDING").len(),
            1,
            "{id}"
        );
    }

    // A new verifier process: worker start leaves the landing batch alone.
    let env = board.env();
    assert!(
        abandon_interrupted_batches(board.fixture.store(), &env, board.fixture.project())
            .unwrap()
            .is_empty()
    );
    let mut restarted = landing_batcher(&board);
    restarted.recovery = Some(LandingOutcome::Merged {
        detail: "confirmed on recovery".into(),
    });

    assert_eq!(tick(&board, &restarted), TickResult::Completed);

    assert_eq!(
        restarted.calls(),
        [
            format!("recover {} {BATCH_PR}", ids[0]),
            format!(
                "prune-members {}",
                ids.iter()
                    .map(|id| format!("worktree-{id}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        ],
        "one recovery for the batch, never a second merge request"
    );
    assert!(landing_intents(&board).is_empty());
    assert_eq!(batches(&board)[0].phase, BatchPhase::Landed);
    for id in &ids {
        assert_eq!(story_row(&board.fixture, id).state, "done", "{id}");
    }
}

/// A merge that was never requested releases every member's intent and the
/// batch in one transaction; the head then goes on to its own gate.
#[test]
fn a_batch_merge_never_requested_is_released_and_the_head_is_gated_alone() {
    let board = board(&CLEAN, Some(3));
    let ids = board.stories.clone();
    let mut batcher = landing_batcher(&board);
    batcher.landing = LandingOutcome::NotAttempted {
        detail: "the base moved before the merge".into(),
    };

    let result = tick(&board, &batcher);

    let batch = &batches(&board)[0];
    assert_eq!(batch.phase, BatchPhase::Released);
    assert!(
        batch
            .detail
            .as_deref()
            .unwrap_or_default()
            .contains("never requested"),
        "{:?}",
        batch.detail
    );
    let calls = batcher.calls();
    assert!(
        calls.contains(&format!(
            "verify {} https://github.com/acme/widgets/pull/1",
            ids[0]
        )),
        "the head is gated alone: {calls:?}"
    );
    assert_eq!(
        result,
        TickResult::RetryLater,
        "the head's own landing is not attempted either"
    );
    assert!(landing_intents(&board).is_empty());
    for id in &ids {
        assert_eq!(story_row(&board.fixture, id).state, "verifying", "{id}");
    }
}

/// A member a person takes while the batch merges keeps its landing intent
/// (as a single human-only landing does); the others are done, and the
/// worker does not spin on the held one.
#[test]
fn a_member_a_person_holds_at_landing_keeps_its_intent_and_the_worker_does_not_spin() {
    let board = board(&CLEAN, Some(3));
    let ids = board.stories.clone();
    let mut batcher = landing_batcher(&board);
    batcher.held_at_landing = Some(ids[1].clone());

    assert_eq!(tick(&board, &batcher), TickResult::Completed);

    assert_eq!(story_row(&board.fixture, &ids[0]).state, "done");
    assert_eq!(story_row(&board.fixture, &ids[2]).state, "done");
    assert_eq!(story_row(&board.fixture, &ids[1]).state, "verifying");
    let rows = landing_intents(&board);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].story_id, ids[1]);
    let batch = &batches(&board)[0];
    assert_eq!(batch.phase, BatchPhase::Landed);
    assert!(
        !batcher
            .calls()
            .iter()
            .any(|call| call.contains(&format!("worktree-{}", ids[1]))),
        "the held member's branch is neither reaped nor pruned: {:?}",
        batcher.calls()
    );

    let quiet = landing_batcher(&board);
    assert_eq!(tick(&board, &quiet), TickResult::Idle);
    assert!(quiet.calls().is_empty(), "{:?}", quiet.calls());
}

/// A base that requires signed commits forms no batch: nothing is submitted
/// for the partners, and the head is gated alone (SH-832 D8).
#[test]
fn a_base_that_requires_signed_commits_forms_no_batch() {
    let board = board(&CLEAN, Some(3));
    let ids = board.stories.clone();
    let mut batcher = landing_batcher(&board);
    batcher.signed_base = true;

    assert_eq!(tick(&board, &batcher), TickResult::Completed);

    assert!(batches(&board).is_empty());
    assert_eq!(
        batcher.calls(),
        [
            format!("submit {}", ids[0]),
            "base-policy dev".to_owned(),
            format!("verify {} https://github.com/acme/widgets/pull/1", ids[0]),
            format!("land {} https://github.com/acme/widgets/pull/1", ids[0]),
        ]
    );
    let record = &board.records()[0];
    assert!(
        record["batch"]["detail"]
            .as_str()
            .unwrap()
            .contains("signed commits"),
        "{record}"
    );
}

#[test]
fn the_verifier_help_topic_names_the_running_batch_and_its_landing() {
    let topic = storyhook::help_topics::get_help_topic("verifier").unwrap();
    assert!(topic.contains("status reports batch"), "{topic}");
    assert!(topic.contains("Verification batch <id> running"), "{topic}");
    assert!(topic.contains("done in one transaction"), "{topic}");
}
