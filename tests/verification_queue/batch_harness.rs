//! The scripted batching actuator and board shared by the batching (SH-831,
//! SH-832) and bisection (SH-833) tests: real trial merges and assembly over
//! the board's repository, a scripted GitHub side, and a record of every call.

use super::batch_preview::{Board, git};
use super::*;
use std::collections::{BTreeMap, BTreeSet};
use storyhook::daemon::verification::{
    BatchActuator, BatchPublication, BatchRetirement, LandingOutcome, MemberBranch, MemberOwner,
    MemberPrune, VerificationCancellation,
};
use storyhook::service::trial_merge::{PrivateTrialMerger, TrialMerger};
use storyhook::store::{BatchPullRequest, VerificationBatch};

pub(super) const BATCH_PR: &str = "https://github.com/acme/widgets/pull/900";

/// Room for the batch observer's periodic authority check (every recovery
/// wake on a bus nobody publishes to) on a loaded machine.
pub(super) const OBSERVER_PATIENCE: Duration = Duration::from_secs(120);

/// Three stories that merge cleanly with one another.
pub(super) const CLEAN: [(&str, &str); 3] = [("a", "head\n"), ("b", "second\n"), ("c", "third\n")];

/// How the scripted batch gate behaves.
#[derive(Clone)]
pub(super) enum Gate {
    /// It answers this outcome.
    Answer(Box<VerificationOutcome>),
    /// It takes the story at this index out of `verifying`, then waits to be
    /// cancelled.
    MemberLeaves(usize),
    /// It stops the verifier, then waits to be cancelled.
    OperatorStops,
    /// It certifies the batch pull request at the tip it was published at.
    CertifiesTip,
    /// It judges the gated commit (SH-833): red, with the real merge tree,
    /// when a culprit's head (story indices) is in it, else certified.
    Culprits(Vec<usize>),
    /// It does `action`, then answers as `then` does, without waiting to be
    /// cancelled.
    Then { action: Action, then: Box<Gate> },
}

/// What a scripted gate does to the board before it answers (SH-833).
#[derive(Clone, Copy, Debug)]
pub(super) enum Action {
    /// Takes the story at this index out of `verifying`, as its agent does.
    Leaves(usize),
    /// Labels the story at this index human-only.
    Holds(usize),
    /// Makes a project recovery of the board's project active.
    Recovers,
}

/// A gate that answers `outcome`.
pub(super) fn answer(outcome: VerificationOutcome) -> Gate {
    Gate::Answer(Box::new(outcome))
}

/// An actuator that batches: real trial merges and assembly over the board's
/// repository, a scripted GitHub side, and a record of every call.
pub(super) struct Batcher<'a> {
    pub(super) board: &'a Board,
    pub(super) activity: VerificationActivity,
    pub(super) batching: bool,
    pub(super) gate: Gate,
    pub(super) refuse: BTreeSet<String>,
    pub(super) moved: BTreeSet<String>,
    pub(super) calls: Mutex<Vec<String>>,
    pub(super) publications: Mutex<Vec<BatchPublication>>,
    /// What `land` answers.
    pub(super) landing: LandingOutcome,
    /// What `recover_landing` answers; none means no recovery is expected.
    pub(super) recovery: Option<LandingOutcome>,
    /// A story `land` labels human-only as the merge completes.
    pub(super) held_at_landing: Option<String>,
    /// Whether the base requires signed commits.
    pub(super) signed_base: bool,
    /// The dashboard's `/data` and the CLI's status text, read while the
    /// batch gate ran and while the batch landed.
    pub(super) observed: Mutex<Vec<(serde_json::Value, String)>>,
    /// Gates answered instead of `gate`, by gate call (0-based) (SH-833).
    pub(super) faults: Mutex<BTreeMap<usize, Gate>>,
    /// Gate calls so far.
    pub(super) gates: Mutex<usize>,
    /// Prefix lengths whose tree carries a qualifying receipt: a commit is
    /// certified when it merges exactly that many of the board's first
    /// stories.
    pub(super) receipts: BTreeSet<usize>,
    /// Members whose agent is absent at their first paste.
    pub(super) absent: BTreeSet<String>,
    /// Members pasted to so far.
    pub(super) pasted: Mutex<BTreeSet<String>>,
    /// Status read at each probe gate: (gate call, dashboard, text).
    pub(super) probe_status: Mutex<Vec<(usize, serde_json::Value, String)>>,
}

impl<'a> Batcher<'a> {
    pub(super) fn new(board: &'a Board, gate: Gate) -> Self {
        Self {
            board,
            activity: VerificationActivity::new(),
            batching: true,
            gate,
            refuse: BTreeSet::new(),
            moved: BTreeSet::new(),
            calls: Mutex::new(Vec::new()),
            publications: Mutex::new(Vec::new()),
            landing: LandingOutcome::Merged {
                detail: "test merge confirmed".into(),
            },
            recovery: None,
            held_at_landing: None,
            signed_base: false,
            observed: Mutex::new(Vec::new()),
            faults: Mutex::new(BTreeMap::new()),
            gates: Mutex::new(0),
            receipts: BTreeSet::new(),
            absent: BTreeSet::new(),
            pasted: Mutex::new(BTreeSet::new()),
            probe_status: Mutex::new(Vec::new()),
        }
    }

    /// Answers the `call`-th gate (0-based) with `gate` instead.
    pub(super) fn fault(&self, call: usize, gate: Gate) {
        self.faults.lock().unwrap().insert(call, gate);
    }

    /// The head commit of the story at `index`.
    pub(super) fn story_head(&self, index: usize) -> String {
        git(
            &self.board.root,
            &[
                "rev-parse",
                &format!("worktree-{}", self.board.stories[index]),
            ],
        )
    }

    /// Whether `commit` contains the head of the story at `index`.
    pub(super) fn contains(&self, index: usize, commit: &str) -> bool {
        storyhook::env::git_env::command(&self.board.root)
            .args([
                "merge-base",
                "--is-ancestor",
                &self.story_head(index),
                commit,
            ])
            .status()
            .unwrap()
            .success()
    }

    /// Judges the commit `pull_request` gates: red iff a culprit's head is
    /// in it, with the tree its merge onto `origin/dev` makes.
    fn judge(&self, pull_request: &PrLink, culprits: &[usize]) -> VerificationOutcome {
        let root = &self.board.root;
        let published = self
            .publications
            .lock()
            .unwrap()
            .iter()
            .enumerate()
            .find(|(index, _)| pull_request.number == 900 + *index as u64)
            .map(|(_, publication)| publication.tip.clone());
        let commit = published.unwrap_or_else(|| {
            // A story's own pull request: pull/<index + 1>.
            self.story_head(pull_request.number as usize - 1)
        });
        let tree = git(root, &["merge-tree", "--write-tree", "origin/dev", &commit]);
        let red = culprits
            .iter()
            .any(|culprit| self.contains(*culprit, &commit));
        if red {
            VerificationOutcome::TestsFailed {
                tree,
                log: format!("/tmp/gate-{}.log", pull_request.number),
                detail: format!("fixture: pull request {} is red", pull_request.number),
                gate: "make test".into(),
            }
        } else {
            VerificationOutcome::Certified {
                head: commit,
                tree,
                detail: "fixture: green".into(),
                gate: "make test".into(),
            }
        }
    }

    /// Answers one gate call with `gate`.
    fn answer_gate(
        &self,
        gate: &Gate,
        pull_request: &PrLink,
        cancellation: &VerificationCancellation,
    ) -> VerificationOutcome {
        match gate {
            Gate::Answer(outcome) => (**outcome).clone(),
            Gate::MemberLeaves(index) => {
                self.leave(*index);
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
            Gate::CertifiesTip => VerificationOutcome::Certified {
                head: self
                    .observed_then(&self.publications)
                    .lock()
                    .unwrap()
                    .last()
                    .unwrap()
                    .tip
                    .clone(),
                tree: "e".repeat(40),
                detail: "batch gate passed".into(),
                gate: "make test".into(),
            },
            Gate::Culprits(culprits) => self.judge(pull_request, culprits),
            Gate::Then { action, then } => {
                match action {
                    Action::Leaves(index) => self.leave(*index),
                    Action::Holds(index) => {
                        StoryService::new(&self.board.fixture.ctx())
                            .set_labels(&self.board.stories[*index], &["human-only".into()], &[])
                            .unwrap();
                    }
                    Action::Recovers => {
                        let recovery = storyhook::store::ProjectRecovery {
                            id: "fixture-recovery".into(),
                            project: self.board.fixture.project(),
                            code: "missing-certification".into(),
                            locus: ".storyhook.toml#verify.gate".into(),
                            revision: 0,
                            active: true,
                            state: serde_json::json!({"version": 1, "phase": "assessment-pending"}),
                        };
                        assert!(
                            self.board
                                .fixture
                                .store()
                                .write(|tx| tx.insert_project_recovery(&recovery))
                                .unwrap()
                        );
                    }
                }
                self.answer_gate(then, pull_request, cancellation)
            }
        }
    }

    /// Takes the story at `index` out of `verifying`, as its agent does.
    fn leave(&self, index: usize) {
        StoryService::new(&self.board.fixture.ctx())
            .set_state(
                &self.board.stories[index],
                "in-progress",
                Some("fixture: the agent takes the story back"),
                None,
                None,
            )
            .unwrap();
    }

    /// Observes status, then answers the batch's published tip holder.
    pub(super) fn observed_then<'m>(
        &self,
        publications: &'m Mutex<Vec<BatchPublication>>,
    ) -> &'m Mutex<Vec<BatchPublication>> {
        self.observe();
        publications
    }

    /// Reads status at a later gate call: a bisection probe.
    fn observe_probe(&self, call: usize) {
        let (data, text) = self.status_now();
        self.probe_status.lock().unwrap().push((call, data, text));
    }

    /// Reads what the dashboard and `story verifier status` show now.
    pub(super) fn observe(&self) {
        let observed = self.status_now();
        self.observed.lock().unwrap().push(observed);
    }

    fn status_now(&self) -> (serde_json::Value, String) {
        let fixture = &self.board.fixture;
        let slug = VerificationQueue::new(fixture.store())
            .ordered_for(fixture.project())
            .unwrap()
            .first()
            .map(|candidate| candidate.project_slug.clone())
            .expect("a queued story names the project");
        let routed = rest::route_with_activity(
            fixture.store(),
            fixture.env(),
            &self.activity,
            rest::RouteRequest::new(
                &Method::Get,
                &format!("/api/repos/{slug}/data"),
                &[Header::from_bytes("Host", "127.0.0.1:3456").unwrap()],
                "",
            ),
            &TrustedHosts::default(),
        );
        let data = serde_json::from_str(routed.reply.text_body().unwrap()).unwrap();
        let text = self.activity.status(&fixture.ctx()).unwrap().render_human();
        (data, text)
    }

    pub(super) fn call(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }

    pub(super) fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    pub(super) fn wait_cancelled(cancellation: &VerificationCancellation) -> VerificationOutcome {
        load_grace::wait_for(
            Patience::new(OBSERVER_PATIENCE),
            Duration::from_millis(20),
            || "the batch gate was never cancelled".into(),
            || cancellation.is_cancelled().then_some(()),
        );
        VerificationOutcome::Cancelled
    }

    pub(super) fn receipt(
        &self,
        candidate: &VerificationCandidate,
        head: String,
    ) -> SubmittedPullRequest {
        let link = candidate.pull_request.clone().expect("a linked PR");
        SubmittedPullRequest {
            url: link.url,
            number: link.number,
            base: "dev".into(),
            head_oid: head,
            adopted: true,
        }
    }

    pub(super) fn branch_head(&self, candidate: &VerificationCandidate) -> String {
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
        intent: &storyhook::store::LandingIntent,
    ) -> LandingOutcome {
        self.call(format!(
            "land {} {}",
            candidate.story_id,
            intent.landing_pull_request()
        ));
        self.observe();
        if let Some(held) = &self.held_at_landing {
            StoryService::new(&self.board.fixture.ctx())
                .set_labels(held, &["human-only".into()], &[])
                .unwrap();
        }
        self.landing.clone()
    }

    fn recover_landing(
        &self,
        candidate: &VerificationCandidate,
        intent: &storyhook::store::LandingIntent,
    ) -> LandingOutcome {
        self.call(format!(
            "recover {} {}",
            candidate.story_id,
            intent.landing_pull_request()
        ));
        self.recovery
            .clone()
            .expect("this test leaves no landing authority unresolved")
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

    fn reap(&self, candidate: &VerificationCandidate) -> Result<(), AppError> {
        self.call(format!("reap {}", candidate.story_id));
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
        let mut publications = self.publications.lock().unwrap();
        // The first publication is BATCH_PR; each later one (a bisection
        // probe) its own pull request.
        let number = 900 + publications.len() as u64;
        publications.push(publication.clone());
        Ok(BatchPullRequest {
            url: format!("https://github.com/acme/widgets/pull/{number}"),
            number,
        })
    }

    fn gate(
        &self,
        _head: &VerificationCandidate,
        pull_request: &PrLink,
        cancellation: &VerificationCancellation,
    ) -> VerificationOutcome {
        self.call(format!("gate {}", pull_request.url));
        let call = {
            let mut gates = self.gates.lock().unwrap();
            *gates += 1;
            *gates - 1
        };
        if call > 0 {
            self.observe_probe(call);
        }
        let gate = self
            .faults
            .lock()
            .unwrap()
            .remove(&call)
            .unwrap_or_else(|| self.gate.clone());
        self.answer_gate(&gate, pull_request, cancellation)
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

    fn base_policy(
        &self,
        _head: &VerificationCandidate,
        base: &str,
        _cancellation: &VerificationCancellation,
    ) -> Result<bool, AppError> {
        self.call(format!("base-policy {base}"));
        Ok(self.signed_base)
    }

    fn reap_member(
        &self,
        member: &VerificationCandidate,
        _owner: MemberOwner<'_>,
        _cancellation: &VerificationCancellation,
    ) -> Result<(), AppError> {
        self.call(format!("reap-member {}", member.story_id));
        Ok(())
    }

    fn certified(
        &self,
        _head: &VerificationCandidate,
        base: &str,
        commit: &str,
        _cancellation: &VerificationCancellation,
    ) -> Result<bool, AppError> {
        assert_eq!(base, git(&self.board.root, &["rev-parse", "origin/dev"]));
        let merged = (0..self.board.stories.len())
            .take_while(|index| self.contains(*index, commit))
            .count();
        self.call(format!("certified {merged}"));
        Ok(self.receipts.contains(&merged))
    }

    fn notify_member(
        &self,
        member: &VerificationCandidate,
        _message: &str,
        _owner: MemberOwner<'_>,
        _cancellation: &VerificationCancellation,
    ) -> Result<NotifyDelivery, AppError> {
        self.call(format!("notify-member {}", member.story_id));
        let first = self.pasted.lock().unwrap().insert(member.story_id.clone());
        if first && self.absent.contains(&member.story_id) {
            return Ok(NotifyDelivery::AgentAbsent {
                reason: "agent-absent".into(),
                detail: "fixture: no agent pane".into(),
            });
        }
        Ok(NotifyDelivery::Delivered)
    }

    fn redispatch_member(
        &self,
        member: &VerificationCandidate,
        _plan: &ResumePlan,
        _owner: MemberOwner<'_>,
        _cancellation: &VerificationCancellation,
    ) -> Result<(), AppError> {
        self.call(format!("redispatch-member {}", member.story_id));
        Ok(())
    }

    fn prune_members(
        &self,
        _head: &VerificationCandidate,
        members: &[MemberBranch],
    ) -> Result<Vec<MemberPrune>, AppError> {
        self.call(format!(
            "prune-members {}",
            members
                .iter()
                .map(|member| member.branch.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        ));
        Ok(members
            .iter()
            .map(|member| MemberPrune {
                branch: member.branch.clone(),
                result: "deleted".into(),
                detail: None,
            })
            .collect())
    }
}

/// A board whose repository has the identity batch merge commits need.
pub(super) fn board(stories: &[(&str, &str)], lanes: Option<u32>) -> Board {
    let board = Board::new(stories);
    git(&board.root, &["config", "user.name", "t"]);
    git(&board.root, &["config", "user.email", "t@t"]);
    if let Some(lanes) = lanes {
        board.live_run(lanes);
    }
    board
}

pub(super) fn tick(board: &Board, batcher: &Batcher<'_>) -> TickResult {
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

pub(super) fn batches(board: &Board) -> Vec<VerificationBatch> {
    board
        .fixture
        .store()
        .read(|tx| tx.verification_batches(board.fixture.project()))
        .unwrap()
}

pub(super) fn queued(board: &Board) -> Vec<(String, Option<GlobalSeq>)> {
    VerificationQueue::new(board.fixture.store())
        .ordered_for(board.fixture.project())
        .unwrap()
        .into_iter()
        .map(|candidate| (candidate.story_id, candidate.verifying_generation))
        .collect()
}

pub(super) fn submitted_comments(board: &Board, id: &str) -> usize {
    story_row(&board.fixture, id)
        .snapshot
        .comments
        .iter()
        .filter(|comment| comment.text.starts_with(VERIFICATION_SUBMITTED_PREFIX))
        .count()
}

pub(super) fn member_ids(batch: &VerificationBatch) -> Vec<String> {
    batch.members.iter().map(|m| m.story_id.clone()).collect()
}

pub(super) fn certified_batch() -> VerificationOutcome {
    VerificationOutcome::Certified {
        head: "d".repeat(40),
        tree: "e".repeat(40),
        detail: "batch gate passed".into(),
        gate: "make test".into(),
    }
}
