//! SH-772: a verifier landing is a story mutation like any other, so the
//! stories it unblocks get their Resume and the episodes it ends are retired.
//!
//! MT-32 was `in-progress` and `blocked-by` MT-13. The verifier landed MT-13
//! and the edge cleared, but the landing transaction never compared blocking
//! before and after, so no Resume was recorded and the agent was never told.

use storyhook::daemon::verification::{
    LandingOutcome, NotifyDelivery, ResumePlan, TickResult, VerificationActuator,
    VerificationOutcome, tick_with,
};
use storyhook::error::AppError;
use storyhook::service::landing::{LandingAdmission, VerifiedSubmission};
use storyhook::service::{
    ConfigService, NewStoryInput, PrLinkService, RelationService, StoryService,
    VerificationCandidate, VerificationQueue,
};
use storyhook::store::{
    BlockAction, BlockDelivery, DeliveryStatus, LandingIntent, PrLink, ReadOps, Store, StoryNo,
};
use storyhook_test_support::ServiceFixture;

const PR: &str = "https://github.com/acme/widgets/pull/1";

/// A story in `verifying` with a linked pull request, ready to be landed.
fn submitted(f: &ServiceFixture) -> String {
    f.github_checkout("https://github.com/acme/widgets");
    let id = story(f, "submission", None);
    PrLinkService::new(&f.ctx()).link(&id, PR, true).unwrap();
    StoryService::new(&f.ctx())
        .set_state(&id, "verifying", None, None, None)
        .unwrap();
    id
}

fn story(f: &ServiceFixture, title: &str, story_type: Option<&str>) -> String {
    StoryService::new(&f.ctx())
        .create(&NewStoryInput {
            title: title.into(),
            story_type: story_type.map(str::to_owned),
            ..Default::default()
        })
        .unwrap()
        .id
}

/// A story an agent is working, blocked by every story in `blockers`.
fn dependent(f: &ServiceFixture, state: &str, blockers: &[&str]) -> String {
    let id = story(f, "dependent", None);
    StoryService::new(&f.ctx())
        .set_state(&id, state, None, None, None)
        .unwrap();
    for blocker in blockers {
        RelationService::new(&f.ctx())
            .relate(&id, "blocked-by", blocker, false)
            .unwrap();
    }
    id
}

fn certification() -> VerifiedSubmission {
    VerifiedSubmission {
        head: "a".repeat(40),
        tree: "b".repeat(40),
        gate: "make test".into(),
    }
}

fn admit(f: &ServiceFixture) -> LandingIntent {
    let queue = VerificationQueue::new(f.store());
    let candidate = queue.next().unwrap().unwrap();
    let LandingAdmission::Admitted(intent) = queue
        .begin_landing(&f.ctx(), &candidate, &certification())
        .unwrap()
    else {
        panic!("admission refused")
    };
    intent
}

fn land(f: &ServiceFixture, intent: &LandingIntent) -> bool {
    VerificationQueue::new(f.store())
        .complete_landing(&f.ctx(), intent, "remote merged")
        .unwrap()
}

fn deliveries(f: &ServiceFixture, id: &str) -> Vec<BlockDelivery> {
    let story = StoryNo::parse_id("SH", id).unwrap();
    f.store()
        .read(|tx| tx.block_deliveries(f.project()))
        .unwrap()
        .into_iter()
        .filter(|delivery| delivery.story == story)
        .collect()
}

fn actions(f: &ServiceFixture, id: &str) -> Vec<BlockAction> {
    deliveries(f, id).into_iter().map(|d| d.action).collect()
}

fn blocked_by(f: &ServiceFixture, id: &str) -> Vec<String> {
    let story = StoryNo::parse_id("SH", id).unwrap();
    f.store()
        .read(|tx| tx.story(f.project(), story))
        .unwrap()
        .expect("the story exists")
        .snapshot
        .relationships
        .into_iter()
        .filter(|r| r.relation == "blocked-by")
        .map(|r| r.other_id)
        .collect()
}

#[test]
fn landing_a_blocker_resumes_its_active_dependent() {
    let f = ServiceFixture::new();
    let blocker = submitted(&f);
    let id = dependent(&f, "in-progress", &[&blocker]);
    assert_eq!(actions(&f, &id), [BlockAction::Interrupt]);

    let intent = admit(&f);
    assert!(land(&f, &intent));

    assert!(
        blocked_by(&f, &id).is_empty(),
        "the landing retracts the edge"
    );
    assert_eq!(
        actions(&f, &id),
        [BlockAction::Interrupt, BlockAction::Resume],
        "the landing that unblocked an active story must record its Resume"
    );
    assert_eq!(deliveries(&f, &id)[1].status, DeliveryStatus::Pending);
}

#[test]
fn landing_the_last_child_of_an_epic_resumes_what_the_epic_blocked() {
    let f = ServiceFixture::new();
    ConfigService::new(&f.ctx())
        .add_type("epic", None, None)
        .unwrap();
    let child = submitted(&f);
    let epic = story(&f, "epic", Some("epic"));
    RelationService::new(&f.ctx())
        .relate(&epic, "parent-of", &child, false)
        .unwrap();
    let id = dependent(&f, "in-progress", &[&epic]);
    assert_eq!(actions(&f, &id), [BlockAction::Interrupt]);

    let intent = admit(&f);
    assert!(land(&f, &intent));

    assert_eq!(
        actions(&f, &id),
        [BlockAction::Interrupt, BlockAction::Resume],
        "closing the epic's last open child closes the epic's computed state"
    );
}

#[test]
fn landing_does_not_resume_a_story_that_is_still_held() {
    // A second open blocker, and an independent awaiting hold.
    for hold in ["second blocker", "awaiting"] {
        let f = ServiceFixture::new();
        let blocker = submitted(&f);
        let other = story(&f, "still open", None);
        let id = if hold == "second blocker" {
            dependent(&f, "in-progress", &[&blocker, &other])
        } else {
            let id = dependent(&f, "in-progress", &[&blocker]);
            StoryService::new(&f.ctx())
                .set_awaiting(&id, "an operator decision")
                .unwrap();
            id
        };
        let before = actions(&f, &id);

        let intent = admit(&f);
        assert!(land(&f, &intent));

        assert_eq!(actions(&f, &id), before, "{hold}: still blocked, no Resume");
    }
}

#[test]
fn landing_does_not_resume_a_dependent_nobody_is_working() {
    let f = ServiceFixture::new();
    let blocker = submitted(&f);
    let id = dependent(&f, "todo", &[&blocker]);
    // Blocking a story nobody works records its Interrupt already retired.
    assert_eq!(actions(&f, &id), [BlockAction::Interrupt]);
    assert_eq!(deliveries(&f, &id)[0].status, DeliveryStatus::Superseded);

    let intent = admit(&f);
    assert!(land(&f, &intent));

    assert!(blocked_by(&f, &id).is_empty());
    assert_eq!(
        actions(&f, &id),
        [BlockAction::Interrupt],
        "no active execution to resume"
    );
}

#[test]
fn landing_retires_the_landed_storys_pending_effects() {
    use storyhook::store::WriteOps;
    let f = ServiceFixture::new();
    let landed = submitted(&f);
    let story = StoryNo::parse_id("SH", &landed).unwrap();
    // An effect enqueued for the submission and not yet started by the worker.
    f.store()
        .write(|tx| tx.enqueue_block_delivery(f.project(), story, BlockAction::Resume))
        .unwrap();

    let intent = admit(&f);
    assert!(land(&f, &intent));

    let rows = deliveries(&f, &landed);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].status,
        DeliveryStatus::Superseded,
        "a done story has no execution left to deliver to: {rows:?}"
    );
}

#[test]
fn a_refused_landing_records_no_delivery() {
    let f = ServiceFixture::new();
    let blocker = submitted(&f);
    let id = dependent(&f, "in-progress", &[&blocker]);
    let intent = admit(&f);
    StoryService::new(&f.ctx())
        .set_labels(&blocker, &["human-only".into()], &[])
        .unwrap();

    assert!(
        !land(&f, &intent),
        "a human-only reservation refuses completion"
    );

    assert_eq!(blocked_by(&f, &id), [blocker]);
    assert_eq!(actions(&f, &id), [BlockAction::Interrupt]);
}

/// The whole production flow: the daemon's verifier tick lands the blocker,
/// and the daemon's delivery worker hands the exact prompt to the helper for
/// the session the block interrupted.
#[test]
fn a_verifier_tick_that_lands_a_blocker_delivers_the_resume_prompt() {
    struct Merging;
    impl VerificationActuator for Merging {
        fn submit(
            &self,
            _: &VerificationCandidate,
        ) -> Result<
            storyhook::domain::SubmittedPullRequest,
            storyhook::daemon::verification::SubmissionFailure,
        > {
            panic!("a linked fixture does not submit")
        }
        fn verify(&self, _: &VerificationCandidate, _: &PrLink) -> VerificationOutcome {
            let certificate = certification();
            VerificationOutcome::Certified {
                head: certificate.head,
                tree: certificate.tree,
                gate: certificate.gate,
                detail: "gate passed".into(),
            }
        }
        fn land(&self, _: &VerificationCandidate, _: &LandingIntent) -> LandingOutcome {
            LandingOutcome::Merged {
                detail: "merged".into(),
            }
        }
        fn recover_landing(&self, _: &VerificationCandidate, _: &LandingIntent) -> LandingOutcome {
            panic!("no landing is pending before this tick")
        }
        fn notify(&self, _: &VerificationCandidate, _: &str) -> Result<NotifyDelivery, AppError> {
            panic!("a green landing returns no work to its author")
        }
        fn redispatch(&self, _: &VerificationCandidate, _: &ResumePlan) -> Result<(), AppError> {
            panic!("a green landing redispatches nothing")
        }
        fn reap(&self, _: &VerificationCandidate) -> Result<(), AppError> {
            Ok(())
        }
    }

    let f = ServiceFixture::new();
    let blocker = submitted(&f);
    let id = dependent(&f, "in-progress", &[&blocker]);
    let prompt = f.cwd().join("resume-prompt");
    let helper = f.cwd().join("provider.sh");
    std::fs::write(
        &helper,
        format!(
            r#"if [ "$5" = --interrupt ]; then
printf '{{"ok":true,"target":"blocked-session","display":"interrupted"}}'
else
[ "$6" = --expected-target ] && [ "$7" = blocked-session ] || exit 22
printf '%s' "$5" > '{}'
printf '{{"ok":true,"display":"prompt delivered"}}'
fi
"#,
            prompt.display()
        ),
    )
    .unwrap();
    let deliver = || {
        storyhook::daemon::block_delivery::process_one(f.store(), f.env(), Some(&helper)).unwrap()
    };
    assert!(deliver(), "the block's interrupt is delivered first");

    assert_eq!(
        tick_with(f.store(), f.env(), &Merging, f.project()).unwrap(),
        TickResult::Completed
    );
    assert!(deliver(), "the landing left a Resume for the worker");

    assert_eq!(
        std::fs::read_to_string(&prompt).unwrap(),
        storyhook::service::block_delivery::UNBLOCK_PROMPT
    );
    let rows = deliveries(&f, &id);
    assert_eq!(
        rows.iter()
            .map(|d| (d.action, d.status))
            .collect::<Vec<_>>(),
        [
            (BlockAction::Interrupt, DeliveryStatus::Delivered),
            (BlockAction::Resume, DeliveryStatus::Delivered)
        ]
    );
}

/// MT-32 exactly: the block's interrupt never reached the session (its bind
/// step timed out), the agent parked by itself, and the verifier later landed
/// the blocker. The Resume must still reach the registered session (SH-772 D5).
#[test]
fn a_landing_resumes_an_agent_whose_interrupt_never_arrived() {
    let f = ServiceFixture::new();
    let blocker = submitted(&f);
    let id = dependent(&f, "in-progress", &[&blocker]);
    let calls = f.cwd().join("calls");
    let helper = f.cwd().join("provider.sh");
    std::fs::write(
        &helper,
        format!(
            r#"printf '%s|%s\n' "$5" "$6" >> '{}'
if [ "$5" = --interrupt ]; then
printf '{{"ok":false,"reason":"pane-query-failed","display":"ps timed out after 5 seconds"}}'
else
printf '{{"ok":true,"target":"parked-session","display":"resumed"}}'
fi
"#,
            calls.display()
        ),
    )
    .unwrap();
    let deliver = || {
        storyhook::daemon::block_delivery::process_one(f.store(), f.env(), Some(&helper)).unwrap()
    };
    assert!(deliver());
    assert_eq!(deliveries(&f, &id)[0].status, DeliveryStatus::Unreached);

    let intent = admit(&f);
    assert!(land(&f, &intent));
    assert!(deliver());

    let resume = &deliveries(&f, &id)[1];
    assert_eq!(resume.action, BlockAction::Resume);
    assert_eq!(resume.status, DeliveryStatus::Delivered, "{resume:?}");
    assert_eq!(resume.target.as_deref(), Some("parked-session"));
    assert_eq!(
        std::fs::read_to_string(&calls).unwrap(),
        format!(
            "--interrupt|\n{}|--registered-session\n",
            storyhook::service::block_delivery::UNBLOCK_PROMPT
        )
    );
}
