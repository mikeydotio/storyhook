//! A verification batch's landing (SH-832; spec B6): one landing intent per
//! member bound to the batch, admitted in one transaction, completed in one
//! transaction, released in one transaction, and checked against the batch
//! record before every commit.

use storyhook::domain::StoryEvent;
use storyhook::domain::gate_verdict::GateVerdict;
use storyhook::service::batch_landing::BatchLandingAdmission;
use storyhook::service::landing::VerifiedSubmission;
use storyhook::service::{
    NewStoryInput, PrLinkService, RelationService, StoryService, VERIFICATION_GREEN_PREFIX,
    VerificationCandidate, VerificationQueue,
};
use storyhook::store::{
    BatchGate, BatchId, BatchLandingIntent, BatchMember, BatchPhase, BatchPullRequest,
    LandingIntent, ReadOps, Store, StoryNo, VerificationBatch, WriteOps,
};
use storyhook_test_support::ServiceFixture;

const BATCH_PR: &str = "https://github.com/acme/widgets/pull/900";
const TIP: &str = "dddddddddddddddddddddddddddddddddddddddd";
const TREE: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

fn member_pr(index: usize) -> String {
    format!("https://github.com/acme/widgets/pull/{}", index + 1)
}

/// A project with `count` verifying stories, each linked to its own
/// close-on-merge pull request #1, #2, ...
fn submitted(count: usize) -> (ServiceFixture, Vec<String>) {
    let f = ServiceFixture::new();
    f.github_checkout("https://github.com/acme/widgets");
    let ctx = f.ctx();
    let ids = (0..count)
        .map(|index| {
            let id = StoryService::new(&ctx)
                .create(&NewStoryInput {
                    title: format!("member {index}"),
                    ..Default::default()
                })
                .unwrap()
                .id;
            PrLinkService::new(&ctx)
                .link(&id, &member_pr(index), true)
                .unwrap();
            StoryService::new(&ctx)
                .set_state(&id, "verifying", None, None, None)
                .unwrap();
            id
        })
        .collect();
    (f, ids)
}

fn candidates(f: &ServiceFixture, ids: &[String]) -> Vec<VerificationCandidate> {
    let queued = VerificationQueue::new(f.store())
        .ordered_for(f.project())
        .unwrap();
    ids.iter()
        .map(|id| {
            queued
                .iter()
                .find(|candidate| &candidate.story_id == id)
                .cloned()
                .unwrap_or_else(|| panic!("{id} is queued"))
        })
        .collect()
}

/// Records a batch of `members` in `gating`, as the SH-831 batch step leaves
/// it when its gate starts.
fn gating(f: &ServiceFixture, members: &[VerificationCandidate]) -> VerificationBatch {
    let id = BatchId::generate();
    let now = "2026-09-29T00:00:00Z";
    let batch = VerificationBatch {
        branch: id.branch(),
        id,
        project: f.project(),
        project_slug: members[0].project_slug.clone(),
        head: members[0].story_id.clone(),
        base_branch: "dev".into(),
        base_commit: "a".repeat(40),
        tip: TIP.into(),
        pull_request: None,
        phase: BatchPhase::Assembled,
        members: members
            .iter()
            .enumerate()
            .map(|(position, candidate)| BatchMember {
                story: StoryNo::parse_id("SH", &candidate.story_id).unwrap(),
                story_id: candidate.story_id.clone(),
                generation: candidate.verifying_generation.unwrap(),
                head_commit: "c".repeat(40),
                pull_request: candidate.pull_request.clone().unwrap().url,
                position: position as u32,
                branch: None,
            })
            .collect(),
        excluded: Vec::new(),
        gate: None,
        detail: None,
        retired: false,
        revision: 0,
        created_at: now.into(),
        updated_at: now.into(),
    };
    let mut submitted = batch.advance(BatchPhase::Submitted, now).unwrap();
    submitted.pull_request = Some(BatchPullRequest {
        url: BATCH_PR.into(),
        number: 900,
    });
    let mut gated = submitted.advance(BatchPhase::Gating, now).unwrap();
    gated.gate = Some(BatchGate {
        verdict: GateVerdict::Certified,
        tree: Some(TREE.into()),
        detail: "batch gate passed".into(),
        seconds: 1,
    });
    f.store()
        .write(|tx| {
            tx.insert_verification_batch(&batch)?;
            assert!(tx.update_verification_batch(&submitted, 0)?);
            assert!(tx.update_verification_batch(&gated, 1)?);
            Ok(())
        })
        .unwrap();
    gated
}

fn certification() -> VerifiedSubmission {
    VerifiedSubmission {
        head: TIP.into(),
        tree: TREE.into(),
        gate: "make test".into(),
    }
}

fn passed() -> BatchGate {
    BatchGate {
        verdict: GateVerdict::Certified,
        tree: Some(TREE.into()),
        detail: "batch gate passed".into(),
        seconds: 1,
    }
}

fn record(f: &ServiceFixture, id: &BatchId) -> VerificationBatch {
    f.store()
        .read(|tx| tx.verification_batches(f.project()))
        .unwrap()
        .into_iter()
        .find(|batch| &batch.id == id)
        .unwrap()
}

fn intents(f: &ServiceFixture) -> Vec<LandingIntent> {
    f.store().read(|tx| tx.landing_intents()).unwrap()
}

fn state(f: &ServiceFixture, id: &str) -> String {
    let story = StoryNo::parse_id("SH", id).unwrap();
    f.store()
        .read(|tx| tx.story(f.project(), story))
        .unwrap()
        .unwrap()
        .state
}

fn admit(
    f: &ServiceFixture,
    members: &[VerificationCandidate],
    batch: &VerificationBatch,
) -> BatchLandingIntent {
    match VerificationQueue::new(f.store())
        .begin_batch_landing(&f.ctx(), batch, members, &certification(), passed())
        .unwrap()
    {
        BatchLandingAdmission::Admitted { intent, .. } => *intent,
        BatchLandingAdmission::Refused(why) => panic!("admission refused: {why}"),
    }
}

#[test]
fn a_certified_batch_lands_every_member_together() {
    let (f, ids) = submitted(3);
    let members = candidates(&f, &ids);
    let batch = gating(&f, &members);

    let intent = admit(&f, &members, &batch);

    assert_eq!(record(&f, &batch.id).phase, BatchPhase::Landing);
    let rows = intents(&f);
    assert_eq!(rows.len(), 3);
    for (index, row) in rows.iter().enumerate() {
        assert_eq!(
            row.pull_request,
            member_pr(index),
            "each row keeps its own PR"
        );
        assert_eq!(row.landing_pull_request(), BATCH_PR);
        assert_eq!(row.landing_attempt(), intent.batch.landing);
        assert_eq!(row.certification, certification());
    }
    assert!(
        candidates(&f, &ids)
            .iter()
            .all(|candidate| candidate.landing_pending),
        "every member is fenced out of the single-story queue"
    );

    let completed = VerificationQueue::new(f.store())
        .complete_batch_landing(&f.ctx(), &intent, "landed as 1234", &members)
        .unwrap();

    assert_eq!(completed, ids);
    assert!(intents(&f).is_empty());
    let landed = record(&f, &batch.id);
    assert_eq!(landed.phase, BatchPhase::Landed);
    for (index, id) in ids.iter().enumerate() {
        assert_eq!(state(&f, id), "done");
        let story = StoryNo::parse_id("SH", id).unwrap();
        let events = f
            .store()
            .read(|tx| tx.events_for(f.project(), story))
            .unwrap();
        let known: Vec<StoryEvent> = events.iter().filter_map(|e| e.known().cloned()).collect();
        assert!(known.iter().any(
            |event| matches!(event, StoryEvent::StoryPrMerged { url, .. } if *url == member_pr(index))
        ));
        let green = known
            .iter()
            .find_map(|event| match event {
                StoryEvent::StoryCommentAdded { text, .. }
                    if text.starts_with(VERIFICATION_GREEN_PREFIX) =>
                {
                    Some(text.clone())
                }
                _ => None,
            })
            .expect("a GREEN comment");
        assert!(
            green.contains(&format!("verification batch {}", batch.id)),
            "{green}"
        );
        assert!(green.contains(BATCH_PR), "{green}");
        assert!(green.contains(&member_pr(index)), "{green}");
        for other in ids.iter().filter(|other| *other != id) {
            assert!(green.contains(other.as_str()), "names {other}: {green}");
        }
        assert!(green.contains("landed as 1234"), "{green}");
    }
}

#[test]
fn a_fault_before_commit_completes_no_member() {
    use storyhook::store::fault::{FaultAction, FaultPoint, arm};
    let (f, ids) = submitted(2);
    let members = candidates(&f, &ids);
    let batch = gating(&f, &members);
    let intent = admit(&f, &members, &batch);
    let queue = VerificationQueue::new(f.store());

    let guard = arm(
        FaultPoint::BeforeCommit,
        FaultAction::Fail("completion fault".into()),
    );
    assert!(
        queue
            .complete_batch_landing(&f.ctx(), &intent, "confirmed", &members)
            .is_err()
    );
    drop(guard);

    assert_eq!(intents(&f), intent.rows, "every member stays fenced");
    assert!(ids.iter().all(|id| state(&f, id) == "verifying"));
    assert_eq!(record(&f, &batch.id).phase, BatchPhase::Landing);
    assert_eq!(
        queue
            .complete_batch_landing(&f.ctx(), &intent, "confirmed retry", &members)
            .unwrap(),
        ids
    );
    assert!(
        queue
            .complete_batch_landing(&f.ctx(), &intent, "duplicate", &members)
            .unwrap()
            .is_empty(),
        "a second completion finds nothing to do"
    );
}

#[test]
fn no_commit_resolves_one_member_before_the_merge_is_confirmed() {
    let (f, ids) = submitted(2);
    let members = candidates(&f, &ids);
    let batch = gating(&f, &members);
    let intent = admit(&f, &members, &batch);

    let alone = f
        .store()
        .write(|tx| tx.remove_landing_intent(&intent.rows[0]))
        .unwrap_err()
        .to_string();
    assert!(alone.contains("resolved before the batch merge"), "{alone}");
    assert!(
        VerificationQueue::new(f.store())
            .complete_landing(&f.ctx(), &intent.rows[0], "one member only")
            .is_err(),
        "a single-story completion cannot take one member out of its batch"
    );
    let mut abandoned = record(&f, &batch.id);
    let revision = abandoned.revision;
    abandoned.revision += 1;
    abandoned.phase = BatchPhase::Released;
    let refused = f
        .store()
        .write(|tx| tx.update_verification_batch(&abandoned, revision))
        .unwrap_err()
        .to_string();
    assert!(refused.contains("not landing"), "{refused}");
    assert_eq!(intents(&f), intent.rows);
    assert!(ids.iter().all(|id| state(&f, id) == "verifying"));
}

#[test]
fn a_member_a_person_holds_keeps_its_intent_while_the_others_complete() {
    let (f, ids) = submitted(3);
    let members = candidates(&f, &ids);
    let batch = gating(&f, &members);
    let intent = admit(&f, &members, &batch);
    StoryService::new(&f.ctx())
        .set_labels(&ids[1], &["human-only".into()], &[])
        .unwrap();
    let queue = VerificationQueue::new(f.store());

    let completed = queue
        .complete_batch_landing(&f.ctx(), &intent, "landed", &members)
        .unwrap();

    assert_eq!(completed, [ids[0].clone(), ids[2].clone()]);
    assert_eq!(state(&f, &ids[1]), "verifying");
    let rows = intents(&f);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].story_id, ids[1]);
    assert_eq!(record(&f, &batch.id).phase, BatchPhase::Landed);
    assert!(
        queue
            .complete_batch_landing(&f.ctx(), &intent, "still held", &members)
            .unwrap()
            .is_empty(),
        "the person's story is never completed for them"
    );

    StoryService::new(&f.ctx())
        .set_labels(&ids[1], &[], &["human-only".into()])
        .unwrap();
    let fresh = candidates(&f, &ids[1..2]);
    let remaining = BatchLandingIntent::collect(&intents(&f));
    assert_eq!(remaining.len(), 1);
    assert_eq!(
        queue
            .complete_batch_landing(&f.ctx(), &remaining[0], "reconciled", &fresh)
            .unwrap(),
        [ids[1].clone()]
    );
    assert!(intents(&f).is_empty());
    assert_eq!(state(&f, &ids[1]), "done");
}

#[test]
fn the_record_of_a_batch_named_by_a_pending_intent_is_never_pruned() {
    let (f, ids) = submitted(2);
    let members = candidates(&f, &ids);
    let batch = gating(&f, &members);
    let intent = admit(&f, &members, &batch);
    StoryService::new(&f.ctx())
        .set_labels(&ids[1], &["human-only".into()], &[])
        .unwrap();
    VerificationQueue::new(f.store())
        .complete_batch_landing(&f.ctx(), &intent, "landed", &members)
        .unwrap();
    let mut retired = record(&f, &batch.id);
    let revision = retired.revision;
    retired.revision += 1;
    retired.retired = true;
    f.store()
        .write(|tx| {
            assert!(tx.update_verification_batch(&retired, revision)?);
            assert_eq!(tx.prune_verification_batches(f.project(), 0)?, 0);
            Ok(())
        })
        .unwrap();
    assert_eq!(record(&f, &batch.id).phase, BatchPhase::Landed);
}

#[test]
fn admission_writes_nothing_unless_every_member_is_still_what_the_batch_gated() {
    let (f, ids) = submitted(3);
    let members = candidates(&f, &ids);
    let queue = VerificationQueue::new(f.store());

    let batch = gating(&f, &members);
    let mut wrong_head = certification();
    wrong_head.head = "f".repeat(40);
    let BatchLandingAdmission::Refused(why) = queue
        .begin_batch_landing(&f.ctx(), &batch, &members, &wrong_head, passed())
        .unwrap()
    else {
        panic!("a certification of another head must not land the batch")
    };
    assert!(why.contains("not the batch tip"), "{why}");

    let blocker = StoryService::new(&f.ctx())
        .create(&NewStoryInput {
            title: "blocker".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    RelationService::new(&f.ctx())
        .relate(&ids[2], "blocked-by", &blocker, false)
        .unwrap();
    let BatchLandingAdmission::Refused(why) = queue
        .begin_batch_landing(&f.ctx(), &batch, &members, &certification(), passed())
        .unwrap()
    else {
        panic!("a held member must not land")
    };
    assert!(why.contains(&ids[2]), "{why}");
    RelationService::new(&f.ctx())
        .relate(&ids[2], "blocked-by", &blocker, true)
        .unwrap();

    StoryService::new(&f.ctx())
        .set_state(&ids[1], "in-progress", Some("taken back"), None, None)
        .unwrap();
    StoryService::new(&f.ctx())
        .set_state(&ids[1], "verifying", None, None, None)
        .unwrap();
    let BatchLandingAdmission::Refused(why) = queue
        .begin_batch_landing(&f.ctx(), &batch, &members, &certification(), passed())
        .unwrap()
    else {
        panic!("a resubmitted member must not land on the old gate")
    };
    assert!(why.contains(&ids[1]), "{why}");

    assert!(intents(&f).is_empty(), "no refusal leaves an intent");
    assert_eq!(record(&f, &batch.id).phase, BatchPhase::Gating);
}

#[test]
fn a_merge_never_requested_releases_every_member_and_the_batch() {
    let (f, ids) = submitted(2);
    let members = candidates(&f, &ids);
    let batch = gating(&f, &members);
    let intent = admit(&f, &members, &batch);

    let released = VerificationQueue::new(f.store())
        .release_unattempted_batch_landing(&f.ctx(), &intent, "base moved before the merge")
        .unwrap();

    assert_eq!(released.phase, BatchPhase::Released);
    assert_eq!(
        released.detail.as_deref(),
        Some("base moved before the merge")
    );
    assert!(intents(&f).is_empty());
    assert!(
        candidates(&f, &ids)
            .iter()
            .all(|candidate| !candidate.landing_pending),
        "every member is back in the single-story queue"
    );
}

/// GitHub marks every member pull request merged when the batch lands; the
/// poller leaves each to the verifier's completion (SH-832 D7, spec B6).
#[cfg(feature = "github-pr")]
#[test]
fn the_pr_poller_never_calls_a_landing_batch_members_merge_uncertified() {
    use storyhook::service::VERIFICATION_UNCERTIFIED_MERGE_PREFIX;
    use storyhook::service::pr_check::run_check;
    use storyhook_test_support::FakeGithubApiFactory;
    let (f, ids) = submitted(2);
    let members = candidates(&f, &ids);
    let batch = gating(&f, &members);
    let intent = admit(&f, &members, &batch);
    let fake = FakeGithubApiFactory::new();
    fake.seed_pull_request(1, "closed", true);
    fake.seed_pull_request(2, "closed", true);

    let message = format!("{:?}", run_check(&f.ctx(), &fake, None).unwrap());

    assert!(message.contains("landing in progress"), "{message}");
    for id in &ids {
        let story = StoryNo::parse_id("SH", id).unwrap();
        let row = f
            .store()
            .read(|tx| tx.story(f.project(), story))
            .unwrap()
            .unwrap();
        assert_eq!(row.state, "verifying");
        assert!(row.snapshot.comments.iter().all(|comment| {
            !comment
                .text
                .starts_with(VERIFICATION_UNCERTIFIED_MERGE_PREFIX)
        }));
    }
    assert_eq!(intents(&f), intent.rows);
}
