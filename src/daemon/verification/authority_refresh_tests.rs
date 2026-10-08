//! SH-778: refreshed link metadata must not replace a verifier admission.
use super::*;
use crate::daemon::activity::context::{LogContext, enter};
use crate::service::{Clock, NewStoryInput, PrLinkService};
use crate::store::SqliteStore;
use storyhook_test_support::ServiceFixture;

const PR: &str = "https://github.com/acme/widgets/pull/1";
const T0: &str = "2026-09-25T22:56:48Z";
const T1: &str = "2026-09-25T22:57:48Z";

struct Board {
    _fixture: ServiceFixture,
    store: SqliteStore,
    env: Environment,
    project: ProjectId,
    checkout: PathBuf,
    story: String,
}

impl Board {
    fn new() -> Self {
        let fixture = ServiceFixture::new();
        let checkout = fixture.github_checkout("https://github.com/acme/widgets");
        // Unit tests and test-support link separate crate instances.
        let store = SqliteStore::open(fixture.store().path()).unwrap();
        let env = Environment::at(fixture.cwd()).with_subprocess_patience();
        let project = ProjectId::new(fixture.project().get());
        let ctx = Ctx::new(&store, project, checkout.clone(), env.clone()).no_hooks(true);
        let story = StoryService::new(&ctx)
            .create(&NewStoryInput {
                title: "PR metadata refresh".into(),
                ..NewStoryInput::default()
            })
            .unwrap()
            .id;
        let board = Self {
            _fixture: fixture,
            store,
            env,
            project,
            checkout,
            story,
        };
        PrLinkService::new(&board.ctx(T0))
            .link(&board.story, PR, true)
            .unwrap();
        StoryService::new(&board.ctx(T0))
            .set_state(&board.story, "verifying", None, None, None)
            .unwrap();
        board
    }

    fn ctx(&self, at: &str) -> Ctx<'_, SqliteStore> {
        Ctx::new(
            &self.store,
            self.project,
            self.checkout.clone(),
            self.env.clone(),
        )
        .no_hooks(true)
        .clock(Clock::Fixed(at.into()))
    }

    fn candidate(&self) -> VerificationCandidate {
        VerificationQueue::new(&self.store)
            .with_environment(self.env.clone())
            .next()
            .unwrap()
            .unwrap()
    }
}

#[test]
fn same_pr_relink_preserves_attempt_output_and_journal_without_supersession() {
    use crate::service::gate_output::{OutputObservation, OutputReference};
    use std::os::unix::fs::MetadataExt;

    let board = Board::new();
    let mut candidate = board.candidate();
    let original = candidate.clone();
    let activity = VerificationActivity::new();
    let mut guard = activity.acquire(&candidate, T0.into());
    let before = guard.active.clone();
    let inflight = InFlight::new(board.env.clone());
    let entry = inflight.enter();
    name_verification(&entry, &candidate, T0);
    let _log = enter(LogContext::candidate(&candidate, &before.attempt_id));
    super::super::activity::emit("INFO", "test", "event", "SH-778", "log capture is live");
    let progress = journal_path(&board.env, &candidate);
    std::fs::create_dir_all(progress.parent().unwrap()).unwrap();
    std::fs::write(&progress, b"existing attempt progress\n").unwrap();

    // Bind output, then lose it once. The observer must retain that evidence
    // loss even if the file returns; replacing the admission erases it.
    let path = board.checkout.join("gate-output");
    std::fs::write(&path, b"").unwrap();
    let metadata = std::fs::metadata(&path).unwrap();
    let reference = OutputReference {
        attempt_id: before.attempt_id.clone(),
        path: path.clone(),
        dev: metadata.dev(),
        ino: metadata.ino(),
        at: T0.into(),
    };
    assert_eq!(
        activity.observe_output(&before, Some(&reference), T0),
        OutputObservation::Observed(0)
    );
    let moved = path.with_extension("saved");
    std::fs::rename(&path, &moved).unwrap();
    let lost = activity.observe_output(&before, Some(&reference), T1);
    assert!(matches!(&lost, OutputObservation::Unavailable(_)));
    std::fs::rename(&moved, &path).unwrap();

    PrLinkService::new(&board.ctx(T1))
        .link(&board.story, PR, true)
        .unwrap();
    let refreshed = board.candidate();
    assert_eq!(
        refreshed.verifying_generation,
        original.verifying_generation
    );
    assert_ne!(
        refreshed.pull_request, original.pull_request,
        "the detector must really change link metadata"
    );
    assert!(matches!(
        refresh_authority(
            &board.store,
            &VerificationQueue::new(&board.store).with_environment(board.env.clone()),
            &mut guard,
            &entry,
            &board.env,
            &mut candidate
        )
        .unwrap(),
        AuthorityRefresh::Current
    ));
    assert_eq!(
        candidate.pull_request, refreshed.pull_request,
        "derived metadata must still refresh"
    );
    assert_eq!(guard.active, before);
    assert_eq!(activity.active_for(board.project), Some(before.clone()));
    assert_eq!(activity.observe_output(&before, Some(&reference), T1), lost);
    assert_eq!(
        std::fs::read(&progress).unwrap(),
        b"existing attempt progress\n"
    );
    let logs = super::super::activity::day_files(&super::super::activity::project_journal(
        &board.checkout,
    ))
    .unwrap()
    .into_iter()
    .map(|p| std::fs::read_to_string(p).unwrap())
    .collect::<String>();
    assert!(logs.contains("log capture is live"));
    assert!(
        !logs.contains("superseded"),
        "metadata refresh logged a false transfer: {logs}"
    );
}

#[test]
fn different_pr_number_and_new_generation_still_replace_the_attempt() {
    for new_generation in [false, true] {
        let board = Board::new();
        let mut candidate = board.candidate();
        let activity = VerificationActivity::new();
        let mut guard = activity.acquire(&candidate, T0.into());
        let before = guard.active.attempt_id.clone();
        let inflight = InFlight::new(board.env.clone());
        let entry = inflight.enter();
        if new_generation {
            StoryService::new(&board.ctx(T1))
                .set_state(&board.story, "verifying", None, Some("verifying"), None)
                .unwrap();
        } else {
            PrLinkService::new(&board.ctx(T1))
                .unlink(&board.story, PR)
                .unwrap();
            PrLinkService::new(&board.ctx(T1))
                .link(&board.story, "https://github.com/acme/widgets/pull/2", true)
                .unwrap();
        }
        assert!(matches!(
            candidate_authority(
                &VerificationQueue::new(&board.store).with_environment(board.env.clone()),
                &candidate
            )
            .unwrap(),
            CandidateAuthority::Superseded(Some(_))
        ));
        assert!(matches!(
            refresh_authority(
                &board.store,
                &VerificationQueue::new(&board.store).with_environment(board.env.clone()),
                &mut guard,
                &entry,
                &board.env,
                &mut candidate
            )
            .unwrap(),
            AuthorityRefresh::Replaced
        ));
        assert_ne!(guard.active.attempt_id, before);
        assert_eq!(candidate.pull_request, board.candidate().pull_request);
        assert_eq!(
            guard.active.generation,
            board.candidate().verifying_generation
        );
    }
}

#[test]
fn repository_identity_and_invalid_link_transitions_remain_authority_changes() {
    let board = Board::new();
    let original = board.candidate();
    let queue = VerificationQueue::new(&board.store).with_environment(board.env.clone());
    for owner_changes in [false, true] {
        let mut stale = original.clone();
        let link = stale.pull_request.as_mut().unwrap();
        if owner_changes {
            link.owner = "different-owner".into();
        } else {
            link.repo = "different-repository".into();
        }
        assert!(matches!(
            candidate_authority(&queue, &stale).unwrap(),
            CandidateAuthority::Superseded(Some(_))
        ));
    }
    PrLinkService::new(&board.ctx(T1))
        .unlink(&board.story, PR)
        .unwrap();
    assert!(matches!(
        candidate_authority(&queue, &original).unwrap(),
        CandidateAuthority::Superseded(Some(_))
    ));
    let missing = board.candidate();
    assert!(matches!(
        missing.pull_request,
        Err(VerificationProblem::MissingPullRequest)
    ));
    assert!(matches!(
        candidate_authority(&queue, &missing).unwrap(),
        CandidateAuthority::Current(_)
    ));
    PrLinkService::new(&board.ctx(T1))
        .link(&board.story, PR, true)
        .unwrap();
    assert!(matches!(
        candidate_authority(&queue, &missing).unwrap(),
        CandidateAuthority::Superseded(Some(_))
    ));
}

#[test]
fn different_pr_host_or_port_replaces_attempt_after_origin_and_link_change() {
    for origin in [
        "https://enterprise.example/acme/widgets",
        "https://github.com:8443/acme/widgets",
    ] {
        let board = Board::new();
        let mut candidate = board.candidate();
        let original = candidate.clone();
        let activity = VerificationActivity::new();
        let mut guard = activity.acquire(&candidate, T0.into());
        let before = guard.active.attempt_id.clone();
        let inflight = InFlight::new(board.env.clone());
        let entry = inflight.enter();

        // Change the fixture's registered authority and link together. Both
        // candidates must be eligible; an invalid-link refusal would conceal
        // a comparison that incorrectly drops the host or port.
        assert_eq!(board._fixture.github_checkout(origin), board.checkout);
        let url = format!("{origin}/pull/1");
        PrLinkService::new(&board.ctx(T1))
            .link(&board.story, &url, true)
            .unwrap();
        let refreshed = board.candidate();
        let previous = original.pull_request.as_ref().unwrap();
        let current = refreshed.pull_request.as_ref().unwrap();
        assert_eq!(
            (&current.owner, &current.repo, current.number),
            (&previous.owner, &previous.repo, previous.number)
        );
        assert_ne!(
            parse_pr_url(&current.url).unwrap().host,
            parse_pr_url(&previous.url).unwrap().host
        );
        assert_eq!(
            refreshed.verifying_generation,
            original.verifying_generation
        );
        assert_eq!(refreshed.checkout, original.checkout);
        assert!(matches!(
            refresh_authority(
                &board.store,
                &VerificationQueue::new(&board.store).with_environment(board.env.clone()),
                &mut guard,
                &entry,
                &board.env,
                &mut candidate
            )
            .unwrap(),
            AuthorityRefresh::Replaced
        ));
        assert_ne!(guard.active.attempt_id, before);
        assert_eq!(candidate.pull_request, refreshed.pull_request);
    }
}

#[test]
fn normalized_same_pr_url_spelling_keeps_the_admitted_attempt() {
    for url in [
        "https://GitHub.com/ACME/Widgets/pull/1/",
        "  https://github.com/acme/widgets/pull/1  ",
        "http://github.com/acme/widgets/pull/1",
    ] {
        let board = Board::new();
        let mut candidate = board.candidate();
        let activity = VerificationActivity::new();
        let mut guard = activity.acquire(&candidate, T0.into());
        let before = guard.active.clone();
        let inflight = InFlight::new(board.env.clone());
        let entry = inflight.enter();
        PrLinkService::new(&board.ctx(T1))
            .link(&board.story, url, true)
            .unwrap();
        let refreshed = board.candidate();
        assert_ne!(refreshed.pull_request, candidate.pull_request);
        assert!(matches!(
            refresh_authority(
                &board.store,
                &VerificationQueue::new(&board.store).with_environment(board.env.clone()),
                &mut guard,
                &entry,
                &board.env,
                &mut candidate
            )
            .unwrap(),
            AuthorityRefresh::Current
        ));
        assert_eq!(guard.active, before);
        assert_eq!(candidate.pull_request, refreshed.pull_request);
    }
}

#[test]
fn malformed_pr_urls_never_supply_equal_authority() {
    let board = Board::new();
    let valid = board.candidate().pull_request;
    for url in [
        "not-a-pull-request",
        "https://github.com/acme/widgets/pull/1?untrusted=1",
    ] {
        let mut malformed = valid.clone();
        malformed.as_mut().unwrap().url = url.into();
        assert!(!same_pull_request_authority(&valid, &malformed));
        assert!(!same_pull_request_authority(&malformed, &valid));
        assert!(!same_pull_request_authority(&malformed, &malformed));
    }
}

/// Origin validation must keep the caller's declaration when it builds its
/// per-project context, including when the queue runs on a worker thread.
#[test]
fn queue_origin_validation_requires_and_carries_explicit_subprocess_policy() {
    let board = Board::new();
    let undeclared = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        VerificationQueue::new(&board.store).ordered().unwrap()
    }));
    let message = undeclared.expect_err("the default queue must not invent a test policy");
    assert!(
        message
            .downcast_ref::<String>()
            .is_some_and(|message| message.contains("neither patience nor proof"))
    );
    for env in [
        board.env.clone().with_subprocess_proof(),
        board.env.clone().with_subprocess_patience_under(2.0),
    ] {
        let queue = VerificationQueue::new(&board.store).with_environment(env);
        std::thread::scope(|scope| {
            let candidates = scope.spawn(|| queue.ordered()).join().unwrap().unwrap();
            assert_eq!(candidates.len(), 1);
            assert_eq!(candidates[0].pull_request.as_ref().unwrap().url, PR);
        });
    }
}
