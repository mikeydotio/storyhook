//! Already-landed work completes without asking its author to repair it.
use super::*;

#[test]
fn landed_submission_completes_without_returning_for_repair() {
    let fixture = ServiceFixture::new();
    fixture.github_checkout("https://github.com/acme/widgets");
    let root = scratch_dir();
    let (id, _) = leased_submission(&fixture, root.path(), "landed elsewhere", Some(PR_ONE));
    let github = storyhook_test_support::FakeGithubApiFactory::new();
    github.seed_pull_request(1, "closed", true);
    storyhook::service::pr_check::run_check(&fixture.ctx(), &github, Some(&id)).unwrap();
    let evidence = landed_evidence();
    let actuator = FakeActuator::new(VerificationOutcome::Cancelled).with_submission(Ok(
        storyhook::domain::landing::SubmissionOutcome::AlreadyLanded(evidence.clone()),
    ));
    let result = tick_with(fixture.store(), fixture.env(), &actuator, fixture.project()).unwrap();
    assert_eq!(result, TickResult::Completed);
    assert_eq!(story_row(&fixture, &id).state, "done");
    let row = story_row(&fixture, &id);
    let comment = row
        .snapshot
        .comments
        .iter()
        .find(|c| c.text.starts_with("CENTRAL VERIFICATION ALREADY LANDED —"))
        .unwrap();
    assert!(comment.text.contains(&evidence.base_oid));
    assert!(comment.text.contains("No central GREEN"));
    assert!(
        !row.snapshot
            .comments
            .iter()
            .any(|c| c.text.starts_with(VERIFICATION_GREEN_PREFIX))
    );
    assert!(
        fixture
            .store()
            .read(|tx| tx.closure_cleanup(fixture.project(), row.story_no))
            .unwrap()
            .is_some()
    );
    assert!(actuator.notified.lock().unwrap().is_empty());
    assert!(actuator.redispatched.lock().unwrap().is_empty());
}

pub(super) fn landed_evidence() -> storyhook::domain::landing::AlreadyLanded {
    storyhook::domain::landing::AlreadyLanded {
        repository: "github.com/acme/widgets".into(),
        head_oid: "a".repeat(40),
        base: "dev".into(),
        base_oid: "b".repeat(40),
        base_tree: "c".repeat(40),
        merged_pr: None,
    }
}

#[test]
fn merged_link_without_a_lease_recovers_and_does_not_borrow_old_green() {
    for green in [None, Some("c".repeat(40)), Some("d".repeat(40))] {
        let fixture = ServiceFixture::new();
        fixture.github_checkout("https://github.com/acme/widgets");
        let id = submitted(&fixture, "external merge", Priority::High, PR_ONE);
        let source = StoryService::new(&fixture.ctx())
            .create(&NewStoryInput {
                title: "batch certification".into(),
                ..Default::default()
            })
            .unwrap()
            .id;
        if let Some(tree) = &green {
            StoryService::new(&fixture.ctx()).comment(&source, &format!("{VERIFICATION_GREEN_PREFIX} merge tree `{tree}` passed `make test` in verification batch 1.")).unwrap();
        }
        let github = storyhook_test_support::FakeGithubApiFactory::new();
        github.seed_pull_request(1, "closed", true);
        storyhook::service::pr_check::run_check(&fixture.ctx(), &github, Some(&id)).unwrap();
        let mut evidence = landed_evidence();
        evidence.merged_pr = Some(PR_ONE.into());
        let actuator = FakeActuator::new(VerificationOutcome::AlreadyLanded { evidence });
        assert_eq!(
            tick_with(fixture.store(), fixture.env(), &actuator, fixture.project()).unwrap(),
            TickResult::Completed
        );
        let row = story_row(&fixture, &id);
        assert_eq!(row.state, "done");
        let comment = row
            .snapshot
            .comments
            .iter()
            .find(|c| c.text.starts_with("CENTRAL VERIFICATION ALREADY LANDED —"))
            .unwrap();
        assert_eq!(
            comment.text.contains("A retained central GREEN"),
            green == Some("c".repeat(40))
        );
        assert!(actuator.notified.lock().unwrap().is_empty());
        assert!(actuator.redispatched.lock().unwrap().is_empty());
        assert_eq!(
            tick_with(fixture.store(), fixture.env(), &actuator, fixture.project()).unwrap(),
            TickResult::Idle
        );
    }
}

#[test]
fn landed_submissions_remain_held_by_human_only_or_an_open_blocker() {
    for human in [true, false] {
        let fixture = ServiceFixture::new();
        fixture.github_checkout("https://github.com/acme/widgets");
        let root = scratch_dir();
        let (id, _) = leased_submission(&fixture, root.path(), "held merge", None);
        if human {
            StoryService::new(&fixture.ctx())
                .set_labels(&id, &["human-only".into()], &[])
                .unwrap();
        } else {
            let blocker = StoryService::new(&fixture.ctx())
                .create(&NewStoryInput {
                    title: "dependency".into(),
                    ..Default::default()
                })
                .unwrap()
                .id;
            RelationService::new(&fixture.ctx())
                .relate(&id, "blocked-by", &blocker, false)
                .unwrap();
        }
        let actuator = FakeActuator::new(VerificationOutcome::Cancelled).with_submission(Ok(
            storyhook::domain::landing::SubmissionOutcome::AlreadyLanded(landed_evidence()),
        ));
        assert_eq!(
            tick_with(fixture.store(), fixture.env(), &actuator, fixture.project()).unwrap(),
            TickResult::Idle
        );
        assert_eq!(story_row(&fixture, &id).state, "verifying");
        assert!(actuator.submitted.lock().unwrap().is_empty());
        assert!(actuator.notified.lock().unwrap().is_empty());
    }
}

#[test]
fn landed_receipts_are_validated_before_the_daemon_accepts_them() {
    let fixture = ServiceFixture::new();
    fixture.github_checkout("https://github.com/acme/widgets");
    let root = scratch_dir();
    let mut candidate = cleanup_candidate(&fixture, root.path());
    // The registered GitHub checkout, not the fixture's arbitrary initial cwd.
    candidate.checkout = fixture
        .store()
        .read(|tx| tx.checkout_path(fixture.project()))
        .unwrap()
        .unwrap();
    let evidence = serde_json::to_string(&landed_evidence()).unwrap();
    let valid = format!(".pull_request = null | .pushed = false | .already_landed = {evidence}");
    for mutation in [
        valid.clone(),
        format!(".already_landed = {evidence}"),
        format!("{valid} | .already_landed.base_oid = \"short\""),
        format!("{valid} | .already_landed.repository = \"github.com/other/repo\""),
        format!("{valid} | .story_id = \"SH-999\""),
    ] {
        let actuator = submit_actuator(
            root.path(),
            write_submit_receipt_helper(root.path(), &mutation, 0),
        );
        let outcome = actuator.submit(&candidate);
        if mutation == valid {
            assert_eq!(
                outcome.unwrap(),
                storyhook::domain::landing::SubmissionOutcome::AlreadyLanded(landed_evidence())
            );
        } else {
            assert!(
                matches!(outcome, Err(SubmissionFailure::Infrastructure { .. })),
                "{mutation}: {outcome:?}"
            );
        }
    }
}

#[test]
fn merged_links_are_not_borrowed_from_a_closed_lifecycle_or_chosen_ambiguously() {
    let f = ServiceFixture::new();
    f.github_checkout("https://github.com/acme/widgets");
    let id = submitted(&f, "merged links", Priority::High, PR_ONE);
    PrLinkService::new(&f.ctx())
        .link(&id, PR_TWO, true)
        .unwrap();
    let github = storyhook_test_support::FakeGithubApiFactory::new();
    github.seed_pull_request(1, "closed", true);
    github.seed_pull_request(2, "closed", true);
    storyhook::service::pr_check::run_check(&f.ctx(), &github, Some(&id)).unwrap();
    let queue = VerificationQueue::new(f.store());
    assert!(matches!(
        queue.next().unwrap().unwrap().pull_request,
        Err(VerificationProblem::MultiplePullRequests(_))
    ));
    StoryService::new(&f.ctx())
        .set_state(&id, "done", Some("manual completion"), None, None)
        .unwrap();
    StoryService::new(&f.ctx()).reopen(&id).unwrap();
    StoryService::new(&f.ctx())
        .set_state(&id, "verifying", None, None, None)
        .unwrap();
    assert!(matches!(
        queue.next().unwrap().unwrap().pull_request,
        Err(VerificationProblem::MissingPullRequest)
    ));
}

/// Runs a concurrent tracker mutation at the external submission boundary.
struct MutatingSubmission<'a> {
    inner: FakeActuator,
    mutate: Box<dyn Fn(&VerificationCandidate) + Send + Sync + 'a>,
}

impl VerificationActuator for MutatingSubmission<'_> {
    fn submit(
        &self,
        c: &VerificationCandidate,
    ) -> Result<storyhook::domain::landing::SubmissionOutcome, SubmissionFailure> {
        (self.mutate)(c);
        self.inner.submit(c)
    }
    fn verify(&self, _: &VerificationCandidate, _: &PrLink) -> VerificationOutcome {
        panic!("no gate is needed")
    }
    fn land(
        &self,
        _: &VerificationCandidate,
        _: &storyhook::store::LandingIntent,
    ) -> storyhook::daemon::verification::LandingOutcome {
        panic!("no merge is needed")
    }
    fn recover_landing(
        &self,
        _: &VerificationCandidate,
        _: &storyhook::store::LandingIntent,
    ) -> storyhook::daemon::verification::LandingOutcome {
        panic!("no landing intent exists")
    }
    fn notify(&self, c: &VerificationCandidate, message: &str) -> Result<NotifyDelivery, AppError> {
        self.inner.notify(c, message)
    }
    fn redispatch(&self, c: &VerificationCandidate, plan: &ResumePlan) -> Result<(), AppError> {
        self.inner.redispatch(c, plan)
    }
    fn reap(&self, _: &VerificationCandidate) -> Result<(), AppError> {
        panic!("uncompleted work cannot be reaped")
    }
}

#[test]
fn a_block_or_withdrawal_during_landed_inspection_prevents_completion() {
    for blocked in [true, false] {
        let f = ServiceFixture::new();
        f.github_checkout("https://github.com/acme/widgets");
        let root = scratch_dir();
        let (id, _) = leased_submission(&f, root.path(), "concurrent change", None);
        let blocker = StoryService::new(&f.ctx())
            .create(&NewStoryInput {
                title: "blocker".into(),
                ..Default::default()
            })
            .unwrap()
            .id;
        let actuator = MutatingSubmission {
            inner: FakeActuator::new(VerificationOutcome::Cancelled).with_submission(Ok(
                storyhook::domain::landing::SubmissionOutcome::AlreadyLanded(landed_evidence()),
            )),
            mutate: Box::new(|candidate| {
                if blocked {
                    RelationService::new(&f.ctx())
                        .relate(&candidate.story_id, "blocked-by", &blocker, false)
                        .unwrap();
                } else {
                    StoryService::new(&f.ctx())
                        .set_state(&candidate.story_id, "in-progress", None, None, None)
                        .unwrap();
                }
            }),
        };
        assert_eq!(
            tick_with(f.store(), f.env(), &actuator, f.project()).unwrap(),
            TickResult::Returned
        );
        assert_ne!(story_row(&f, &id).state, "done");
        assert!(actuator.inner.notified.lock().unwrap().is_empty());
        assert!(actuator.inner.redispatched.lock().unwrap().is_empty());
    }
}

#[test]
fn observed_merge_records_the_pr_event_and_releases_dependents_once() {
    let f = ServiceFixture::new();
    f.github_checkout("https://github.com/acme/widgets");
    let root = scratch_dir();
    let (id, _) = leased_submission(&f, root.path(), "observed merge", Some(PR_ONE));
    let dependent = StoryService::new(&f.ctx())
        .create(&NewStoryInput {
            title: "dependent".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    RelationService::new(&f.ctx())
        .relate(&dependent, "blocked-by", &id, false)
        .unwrap();
    let mut evidence = landed_evidence();
    evidence.merged_pr = Some(PR_ONE.into());
    let actuator = FakeActuator::new(VerificationOutcome::Cancelled).with_submission(Ok(
        storyhook::domain::landing::SubmissionOutcome::AlreadyLanded(evidence),
    ));
    assert_eq!(
        tick_with(f.store(), f.env(), &actuator, f.project()).unwrap(),
        TickResult::Completed
    );
    let number = story_row(&f, &id).story_no;
    let events = f
        .store()
        .read(|tx| tx.events_for(f.project(), number))
        .unwrap();
    assert_eq!(events.iter().filter(|event| matches!(event.known(), Some(StoryEvent::StoryPrMerged { url, .. }) if url == PR_ONE)).count(), 1);
    assert!(
        !story_row(&f, &dependent)
            .snapshot
            .relationships
            .iter()
            .any(|relation| relation.relation == "blocked-by")
    );
    assert!(actuator.notified.lock().unwrap().is_empty());
    assert!(actuator.redispatched.lock().unwrap().is_empty());
}
