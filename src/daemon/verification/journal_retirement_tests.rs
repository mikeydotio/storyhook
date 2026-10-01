//! SH-776: publishing a new owner retires the story's previous progress
//! journal, under the lock that publishes it.
//!
//! The journal is one file per story and an attempt writes its `run` line
//! only when its gate starts, so until then status read the predecessor's
//! journal and reported a mismatch for every resubmission, retry, landing
//! recovery and reconcile hand-over. These tests pin the two publication
//! points, `admit` and `VerificationGuard::replace`, and what a journal that
//! cannot be removed does.

use super::*;
use crate::daemon::activity::context::{LogContext, enter};
use crate::service::NewStoryInput;
use crate::service::verification_control::VerificationAction;
use crate::store::SqliteStore;
use storyhook_test_support::{ServiceFixture, scratch_dir};

/// One verifying story in the fixture's project, and what the tick needs.
struct Board {
    _fixture: ServiceFixture,
    store: SqliteStore,
    env: Environment,
    project: ProjectId,
    story: String,
}

impl Board {
    fn new() -> Self {
        let fixture = ServiceFixture::new();
        // Unit tests link a second crate instance through test-support. Reopen
        // its seeded database with this crate's types instead of duplicating the seed.
        let store = SqliteStore::open(fixture.store().path()).unwrap();
        let project = ProjectId::new(fixture.project().get());
        let env = Environment::at(fixture.cwd());
        let ctx = Ctx::new(&store, project, env.home().to_path_buf(), env.clone()).no_hooks(true);
        let story = StoryService::new(&ctx)
            .create(&NewStoryInput {
                title: "Journal retirement".into(),
                ..NewStoryInput::default()
            })
            .unwrap()
            .id;
        StoryService::new(&ctx)
            .set_state(&story, "verifying", None, None, None)
            .unwrap();
        Self {
            _fixture: fixture,
            store,
            env,
            project,
            story,
        }
    }

    fn ctx(&self) -> Ctx<'_, SqliteStore> {
        Ctx::new(
            &self.store,
            self.project,
            self.env.home().to_path_buf(),
            self.env.clone(),
        )
        .no_hooks(true)
    }

    fn candidate(&self) -> VerificationCandidate {
        VerificationQueue::new(&self.store).next().unwrap().unwrap()
    }

    /// Returns the story and submits it again: a newer generation.
    fn resubmit(&self) -> VerificationCandidate {
        StoryService::new(&self.ctx())
            .set_state(&self.story, "in-progress", None, None, None)
            .unwrap();
        StoryService::new(&self.ctx())
            .set_state(&self.story, "verifying", None, None, None)
            .unwrap();
        self.candidate()
    }

    /// Writes the journal an earlier attempt's gate left for `candidate`.
    fn earlier_journal(&self, candidate: &VerificationCandidate) -> std::path::PathBuf {
        let path = journal_path(&self.env, candidate);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            format!(
                "{}\n",
                serde_json::json!({
                    "kind": "run",
                    "generation": candidate.verifying_generation.unwrap().get(),
                    "attempt_id": "attempt-before",
                    "at": self.env.now(),
                })
            ),
        )
        .unwrap();
        path
    }

    fn status_at(&self, activity: &VerificationActivity, now: &str) -> status::VerifierStatus {
        activity
            .status(&self.ctx().clock(crate::service::Clock::Fixed(now.into())))
            .unwrap()
    }
}

#[test]
fn admission_retires_the_earlier_journal_before_it_publishes_the_owner() {
    for reservation in [None, Some(ReservationReason::Cleanup)] {
        let board = Board::new();
        let candidate = board.candidate();
        let path = board.earlier_journal(&candidate);
        let activity = VerificationActivity::new();
        let started_at = board.env.now();

        let guard = activity
            .admit(
                &board.store,
                &board.env,
                &candidate,
                started_at.clone(),
                reservation,
            )
            .unwrap()
            .expect("an enabled queue admits its candidate");

        assert!(
            !path.exists(),
            "{reservation:?}: the earlier journal stayed"
        );
        if reservation.is_none() {
            let status = board.status_at(&activity, &started_at);
            assert_eq!(status.evidence_error, None, "{status:?}");
            assert_eq!(status.warning, None, "{status:?}");
            assert_eq!(
                status.last_evidence_at.as_deref(),
                Some(started_at.as_str())
            );
            assert_eq!(
                status.active.map(|active| active.attempt_id),
                Some(guard.active.attempt_id.clone())
            );
        }
    }
}

#[test]
fn a_refused_admission_keeps_the_last_evidence() {
    let board = Board::new();
    let candidate = board.candidate();
    let path = board.earlier_journal(&candidate);
    let before = std::fs::read_to_string(&path).unwrap();
    let activity = VerificationActivity::new();
    activity
        .control(&board.store, board.project, VerificationAction::Stop)
        .unwrap();

    let admitted = activity
        .try_acquire(&board.store, &board.env, &candidate, board.env.now())
        .unwrap();

    assert!(admitted.is_none(), "a stopped queue admits nothing");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

#[test]
fn a_journal_that_cannot_be_retired_is_reported_and_admission_proceeds() {
    let board = Board::new();
    let candidate = board.candidate();
    let path = journal_path(&board.env, &candidate);
    // A directory refuses unlink on every platform (EPERM or EISDIR), and
    // neither error is NotFound.
    std::fs::create_dir_all(&path).unwrap();
    let journal = scratch_dir();
    let logs = journal.path().join("logs");
    let activity = VerificationActivity::new();
    let started_at = board.env.now();

    let guard = {
        let _journal = enter(Some(LogContext {
            directory: logs.clone(),
            label: "project=fixture retirement".into(),
        }));
        activity
            .try_acquire(&board.store, &board.env, &candidate, started_at.clone())
            .unwrap()
    };

    assert!(guard.is_some(), "a status concern never refuses admission");
    let records = crate::daemon::activity::day_files(&logs)
        .unwrap()
        .into_iter()
        .map(|day| std::fs::read_to_string(day).unwrap())
        .collect::<String>();
    let record = records
        .lines()
        .find(|line| line.contains("could not retire"))
        .unwrap_or_else(|| panic!("no retirement failure was journaled: {records}"));
    assert!(record.contains("\"ERROR\""), "{record}");
    assert!(record.contains(&path.display().to_string()), "{record}");
    let error = board
        .status_at(&activity, &started_at)
        .evidence_error
        .unwrap_or_default();
    assert!(
        error.contains(&path.display().to_string()),
        "status still names the journal it cannot read: {error}"
    );
}

#[test]
fn a_generation_transfer_retires_the_earlier_journal() {
    let board = Board::new();
    let first = board.candidate();
    let activity = VerificationActivity::new();
    let mut guard = activity
        .try_acquire(&board.store, &board.env, &first, board.env.now())
        .unwrap()
        .unwrap();
    let path = journal_path(&board.env, &first);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        format!(
            "{}\n",
            serde_json::json!({
                "kind": "run",
                "generation": first.verifying_generation.unwrap().get(),
                "attempt_id": guard.active.attempt_id,
                "at": board.env.now(),
            })
        ),
    )
    .unwrap();
    let resubmitted = board.resubmit();
    let resumed_at = board.env.now();

    guard.replace(&board.env, &resubmitted, resumed_at.clone());

    assert!(
        !path.exists(),
        "the transferred owner kept the earlier journal"
    );
    let status = board.status_at(&activity, &resumed_at);
    assert_eq!(status.evidence_error, None, "{status:?}");
    assert_eq!(status.warning, None, "{status:?}");
    assert_eq!(
        status.active.and_then(|active| active.generation),
        resubmitted.verifying_generation
    );
}
