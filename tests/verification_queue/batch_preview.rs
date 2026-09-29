//! The shadow batch preview (SH-830): computed at each dequeue over a real
//! repository, shown while the gate runs, recorded with the gate's verdict,
//! and never a change to what the verifier verifies, lands or writes.

use super::*;
use storyhook::daemon::verification::status::VerifierStatus;
use storyhook::daemon::verification::{VerificationCancellation, batch_preview_log};
use storyhook::service::batch_preview::{BatchPreview, ExclusionReason, PreviewOutcome};
use storyhook::service::trial_merge::{PrivateTrialMerger, TrialMerge, TrialMerger};
use storyhook::store::{EngineAgent, EngineRunRecord, EngineRunState, EngineScope};

/// Fixed identity and dates for fixture commits, so two boards built alike
/// have the same commits and their store writes can be compared exactly.
const COMMIT_ENV: [(&str, &str); 6] = [
    ("GIT_AUTHOR_NAME", "t"),
    ("GIT_AUTHOR_EMAIL", "t@t"),
    ("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z"),
    ("GIT_COMMITTER_NAME", "t"),
    ("GIT_COMMITTER_EMAIL", "t@t"),
    ("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z"),
];

pub(super) fn git(root: &Path, args: &[&str]) -> String {
    let output = storyhook::env::git_env::command(root)
        .args(args)
        .envs(COMMIT_ENV)
        .output()
        .expect("fixture: running git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// A registered checkout with `origin/dev` at a base commit and one leased,
/// submitted story per `(file, body)`, each on its own branch off the base.
pub(super) struct Board {
    pub(super) fixture: ServiceFixture,
    pub(super) root: PathBuf,
    pub(super) stories: Vec<String>,
}

impl Board {
    pub(super) fn new(stories: &[(&str, &str)]) -> Self {
        Self::with_base(stories, &[], None)
    }

    /// [`Self::new`] whose base also holds `files` and, when given,
    /// `pointer_tail` appended to its committed `.storyhook.toml` (SH-834).
    pub(super) fn with_base(
        stories: &[(&str, &str)],
        files: &[(&str, &str)],
        pointer_tail: Option<&str>,
    ) -> Self {
        let fixture = ServiceFixture::new();
        let root = fixture.github_checkout("https://github.com/acme/widgets");
        git(&root, &["config", "commit.gpgsign", "false"]);
        storyhook_test_support::approve_fixture_identity(&root, "t", "t@t");
        for file in ["a", "b", "c"] {
            std::fs::write(root.join(file), format!("{file} base\n")).unwrap();
        }
        git(&root, &["add", "a", "b", "c"]);
        for (file, body) in files {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
            git(&root, &["add", file]);
        }
        if let Some(tail) = pointer_tail {
            let pointer = root.join(".storyhook.toml");
            let mut text = std::fs::read_to_string(&pointer).unwrap();
            text.push_str(tail);
            std::fs::write(&pointer, text).unwrap();
            git(&root, &["add", ".storyhook.toml"]);
        }
        git(&root, &["commit", "-qm", "base"]);
        let base = git(&root, &["rev-parse", "HEAD"]);
        git(&root, &["update-ref", "refs/remotes/origin/dev", &base]);
        let mut ids = Vec::new();
        for (index, (file, body)) in stories.iter().enumerate() {
            let url = format!("https://github.com/acme/widgets/pull/{}", index + 1);
            let (id, lease) =
                leased_submission(&fixture, &root, &format!("story {index}"), Some(&url));
            git(&root, &["checkout", "-q", "-b", &lease.branch, &base]);
            std::fs::write(root.join(file), body).unwrap();
            // Staged by name, so a story may add a file the base lacks.
            git(&root, &["add", file]);
            git(&root, &["commit", "-qam", &id]);
            git(&root, &["checkout", "-q", "--detach", &base]);
            ids.push(id);
        }
        std::fs::create_dir_all(fixture.env().daemon_state_dir()).unwrap();
        Self {
            fixture,
            root,
            stories: ids,
        }
    }

    /// The environment the tick runs with: the fixture's, on the fixture's
    /// fixed clock, so two runs write the same timestamps.
    pub(super) fn env(&self) -> Environment {
        self.fixture
            .env()
            .clone()
            .clock(Clock::Fixed(FIXTURE_NOW.into()))
    }

    /// One verifier attempt with `actuator`.
    fn tick(&self, actuator: &Probe<'_>) -> TickResult {
        let env = self.env();
        tick_with_activity(
            self.fixture.store(),
            &env,
            actuator,
            &actuator.activity,
            &InFlight::new(env.clone()),
            self.fixture.project(),
        )
        .unwrap()
    }

    pub(super) fn live_run(&self, lanes: u32) {
        let slug = self
            .fixture
            .store()
            .read(|tx| tx.project(self.fixture.project()))
            .unwrap()
            .unwrap()
            .slug;
        let run = EngineRunRecord {
            id: "preview-run".into(),
            project_slug: slug,
            scope: EngineScope::Project,
            lanes,
            agent: EngineAgent::Claude,
            model: None,
            effort: None,
            speed: None,
            state: EngineRunState::Paused,
            consecutive_hard_stops: 0,
            recent_quarantines: Vec::new(),
            stop_reason: None,
            acknowledged_at: None,
            created_at: FIXTURE_NOW.into(),
            updated_at: FIXTURE_NOW.into(),
        };
        self.fixture
            .store()
            .write(|tx| tx.create_engine_run(&run))
            .unwrap();
    }

    /// Every store write, as data: the project's change feed with the
    /// fixture's own scratch path taken out, plus the verifier's durable
    /// records outside the event log.
    fn writes(&self) -> String {
        let project = self.fixture.project();
        let (feed, incident, intents, recovery) = self
            .fixture
            .store()
            .read(|tx| {
                Ok((
                    tx.events_since(project, GlobalSeq::new(0), u32::MAX)?,
                    tx.verification_incident(project)?,
                    tx.landing_intents()?,
                    tx.verification_recovery(project)?,
                ))
            })
            .unwrap();
        let feed: Vec<String> = feed.iter().map(|event| format!("{event:?}")).collect();
        format!("{feed:#?}\n{incident:?}\n{intents:?}\n{recovery:?}")
            .replace(&self.root.display().to_string(), "<root>")
            .replace(&self.fixture.cwd().display().to_string(), "<cwd>")
    }

    pub(super) fn records(&self) -> Vec<serde_json::Value> {
        let path = batch_preview_log(&self.env(), &self.slug());
        match std::fs::read_to_string(&path) {
            Ok(text) => text
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => panic!("reading {}: {error}", path.display()),
        }
    }

    pub(super) fn slug(&self) -> String {
        self.fixture
            .store()
            .read(|tx| tx.project(self.fixture.project()))
            .unwrap()
            .unwrap()
            .slug
    }

    fn status(&self, activity: &VerificationActivity) -> VerifierStatus {
        activity.status(&self.fixture.ctx()).unwrap()
    }
}

/// How the probe's trial merges behave.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Merges {
    /// No preview: the default every other actuator keeps.
    Off,
    /// The production merger over the board's repository.
    Real,
    /// Opening private object storage fails.
    Unopenable,
    /// The merger panics on its first merge.
    Panicking,
}

/// An actuator that submits the leased branch as it is, answers every gate
/// with `outcome`, reads status from inside the gate, and previews through
/// `merges`.
struct Probe<'a> {
    board: &'a Board,
    activity: VerificationActivity,
    merges: Merges,
    outcome: VerificationOutcome,
    verified: Mutex<Vec<String>>,
    seen: Mutex<Vec<Option<BatchPreview>>>,
}

impl<'a> Probe<'a> {
    fn new(board: &'a Board, merges: Merges, outcome: VerificationOutcome) -> Self {
        Self {
            board,
            activity: VerificationActivity::new(),
            merges,
            outcome,
            verified: Mutex::new(Vec::new()),
            seen: Mutex::new(Vec::new()),
        }
    }
}

struct PanickingMerger;

impl TrialMerger for PanickingMerger {
    fn resolve(&mut self, rev: &str) -> Result<Option<String>, AppError> {
        Ok(Some(if rev.starts_with("refs/") {
            "0".repeat(40)
        } else {
            rev.to_string()
        }))
    }
    fn merge(&mut self, _onto: &str, _head: &str) -> Result<TrialMerge, AppError> {
        panic!("simulated trial merge panic")
    }
    fn commit(&mut self, _onto: &str, _head: &str, _tree: &str) -> Result<String, AppError> {
        unreachable!()
    }
}

impl VerificationActuator for Probe<'_> {
    fn submit(
        &self,
        candidate: &VerificationCandidate,
    ) -> Result<SubmittedPullRequest, SubmissionFailure> {
        let link = candidate.pull_request.clone().expect("a linked PR");
        let branch = &candidate.cleanup_lease.as_ref().expect("a lease").branch;
        Ok(SubmittedPullRequest {
            url: link.url,
            number: link.number,
            base: "dev".into(),
            head_oid: git(&self.board.root, &["rev-parse", branch]),
            adopted: true,
        })
    }

    fn verify(
        &self,
        candidate: &VerificationCandidate,
        _pull_request: &PrLink,
    ) -> VerificationOutcome {
        self.verified
            .lock()
            .unwrap()
            .push(candidate.story_id.clone());
        let status = self.board.status(&self.activity);
        self.seen.lock().unwrap().push(status.batch_preview);
        self.outcome.clone()
    }

    fn land(
        &self,
        _candidate: &VerificationCandidate,
        _intent: &storyhook::store::LandingIntent,
    ) -> storyhook::daemon::verification::LandingOutcome {
        storyhook::daemon::verification::LandingOutcome::Merged {
            detail: "test merge confirmed".into(),
        }
    }

    fn recover_landing(
        &self,
        _candidate: &VerificationCandidate,
        _intent: &storyhook::store::LandingIntent,
    ) -> storyhook::daemon::verification::LandingOutcome {
        panic!("this test leaves no landing authority unresolved")
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
        match self.merges {
            Merges::Off => None,
            Merges::Real => Some(PrivateTrialMerger::open(repository).map(|merger| {
                Box::new(
                    merger
                        .with_deadline(deadline)
                        .with_cancellation(cancellation.clone()),
                ) as Box<dyn TrialMerger>
            })),
            Merges::Unopenable => Some(Err(AppError::Storage(
                "simulated: no private object storage".into(),
            ))),
            Merges::Panicking => Some(Ok(Box::new(PanickingMerger))),
        }
    }
}

fn certified() -> VerificationOutcome {
    VerificationOutcome::Certified {
        head: "a".repeat(40),
        tree: "b".repeat(40),
        detail: "gate passed".into(),
        gate: "make test".into(),
    }
}

fn outcomes() -> [VerificationOutcome; 3] {
    [
        certified(),
        VerificationOutcome::TestsFailed {
            tree: "c".repeat(40),
            log: "/tmp/gate.log".into(),
            detail: "1 failed".into(),
            gate: "make test".into(),
        },
        VerificationOutcome::Conflict {
            detail: "both modified a".into(),
        },
    ]
}

/// The head, a clean story, a story that conflicts with the head, and a
/// story that clashes with nothing.
const STORIES: [(&str, &str); 4] = [
    ("a", "head\n"),
    ("b", "clean\n"),
    ("a", "clashes with the head\n"),
    ("c", "also clean\n"),
];

#[test]
fn the_preview_changes_nothing_the_verifier_verifies_or_writes() {
    for outcome in outcomes() {
        let mut runs = Vec::new();
        for merges in [
            Merges::Off,
            Merges::Real,
            Merges::Unopenable,
            Merges::Panicking,
        ] {
            let board = Board::new(&STORIES);
            let probe = Probe::new(&board, merges, outcome.clone());
            let result = board.tick(&probe);
            let verified = probe.verified.lock().unwrap().clone();
            let seen = probe.seen.lock().unwrap().clone();
            assert_eq!(
                seen.iter().all(Option::is_some),
                merges != Merges::Off,
                "a preview is shown exactly when one is computed ({outcome:?})"
            );
            assert_eq!(
                board.records().len(),
                usize::from(merges != Merges::Off),
                "one record per previewed gate"
            );
            runs.push((result, verified, board.writes()));
        }
        let (off, on) = runs.split_first().unwrap();
        for run in on {
            assert_eq!(run.0, off.0, "the tick result ({outcome:?})");
            assert_eq!(run.1, off.1, "the verified story ({outcome:?})");
            assert_eq!(run.2, off.2, "every store write ({outcome:?})");
        }
        assert_eq!(off.1.len(), 1, "one gate per tick ({outcome:?})");
    }
}

#[test]
fn status_shows_the_would_be_batch_only_while_its_gate_runs() {
    let board = Board::new(&STORIES);
    board.live_run(2);
    let probe = Probe::new(&board, Merges::Real, certified());

    assert_eq!(board.tick(&probe), TickResult::Completed);

    let seen = probe.seen.lock().unwrap().clone();
    let preview = seen[0].as_ref().expect("a preview while the gate runs");
    let ids = &board.stories;
    assert_eq!(preview.head, ids[0]);
    assert_eq!(preview.outcome, PreviewOutcome::Batch);
    assert_eq!(preview.cap, 2, "the live run's lanes");
    assert_eq!(preview.live_lanes, Some(2));
    assert_eq!(preview.queue_depth, 4);
    let members: Vec<_> = preview.members.iter().map(|m| m.story_id.clone()).collect();
    assert_eq!(members, [ids[0].clone(), ids[1].clone()]);
    let excluded: Vec<_> = preview
        .excluded
        .iter()
        .map(|entry| (entry.story_id.clone(), entry.reason))
        .collect();
    assert_eq!(
        excluded,
        [
            (ids[2].clone(), ExclusionReason::ConflictWithMember),
            (ids[3].clone(), ExclusionReason::Cap),
        ]
    );
    assert!(
        preview.excluded.iter().all(|entry| entry.paths.is_empty()),
        "status carries the compact preview"
    );
    assert!(
        board.status(&probe.activity).batch_preview.is_none(),
        "no preview once the gate is over"
    );
}

#[test]
fn each_gate_leaves_one_record_with_its_verdict_duration_and_whole_preview() {
    let board = Board::new(&STORIES);
    let ids = board.stories.clone();
    StoryService::new(&board.fixture.ctx())
        .set_labels(&ids[3], &["human-only".into()], &[])
        .unwrap();
    let probe = Probe::new(&board, Merges::Real, certified());

    assert_eq!(board.tick(&probe), TickResult::Completed);

    let records = board.records();
    assert_eq!(records.len(), 1, "{records:?}");
    let record = &records[0];
    assert_eq!(record["story_id"], ids[0]);
    assert_eq!(record["verdict"], "certified");
    assert!(record["gate_seconds"].is_u64(), "{record}");
    assert_eq!(record["gate_tree"], "b".repeat(40));
    assert!(
        record["attempt_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );
    let preview = &record["preview"];
    assert_eq!(preview["cap"], 1, "no live run: a batch of one");
    assert_eq!(
        preview["queue_depth"], 3,
        "the human-only story is held out of the queue"
    );
    assert_eq!(preview["members"][0]["story_id"], ids[0]);
    assert_eq!(
        preview["members"][0]["commit"],
        git(&board.root, &["rev-parse", &format!("worktree-{}", ids[0])])
    );
    assert_eq!(
        preview["head_tree"],
        git(
            &board.root,
            &["rev-parse", &format!("worktree-{}^{{tree}}", ids[0])]
        )
    );
    let excluded: Vec<(String, String)> = preview["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            (
                entry["story_id"].as_str().unwrap().to_string(),
                entry["reason"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        excluded,
        [
            (ids[1].clone(), "cap".to_string()),
            (ids[2].clone(), "conflict-with-member".to_string()),
            (ids[3].clone(), "held".to_string()),
        ]
    );
    assert_eq!(preview["excluded"][1]["paths"], serde_json::json!(["a"]));
    assert!(
        preview["excluded"][2]["detail"]
            .as_str()
            .unwrap()
            .contains("human-only")
    );
}

#[test]
fn a_preview_that_cannot_run_is_recorded_as_unavailable() {
    for (merges, cause) in [
        (Merges::Unopenable, "no private object storage"),
        (Merges::Panicking, "panicked"),
    ] {
        let board = Board::new(&STORIES[..2]);
        let probe = Probe::new(&board, merges, certified());
        assert_eq!(board.tick(&probe), TickResult::Completed);
        let records = board.records();
        assert_eq!(records[0]["verdict"], "certified");
        assert_eq!(records[0]["preview"]["outcome"], "unavailable");
        assert!(
            records[0]["preview"]["detail"]
                .as_str()
                .unwrap()
                .contains(cause),
            "{records:?}"
        );
    }
}

#[test]
fn a_status_without_a_preview_omits_it_and_an_older_payload_decodes() {
    let fixture = ServiceFixture::new();
    let status = VerificationActivity::new().status(&fixture.ctx()).unwrap();
    let absent = serde_json::to_value(&status).unwrap();
    assert!(absent.get("batch_preview").is_none(), "{absent}");
    let decoded: VerifierStatus = serde_json::from_value(absent).unwrap();
    assert!(decoded.batch_preview.is_none());
}

#[test]
fn the_verifier_help_topic_names_the_batch_preview() {
    let topic = storyhook::help_topics::get_help_topic("verifier").unwrap();
    assert!(topic.contains("batch_preview"), "{topic}");
}
