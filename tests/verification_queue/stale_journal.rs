//! A new verifier attempt never reads an earlier attempt's progress journal
//! as its own evidence (SH-776).
//!
//! The journal is one file per story, and an attempt writes its `run` line
//! when its execution is durably admitted. Each test here lets a first attempt journal the
//! way `run_verify_pr` does, then starts a second attempt through the
//! production tick and reads status the way `story verifier status` does,
//! before the second actuator writes anything. Its daemon-owned header must
//! already identify the new execution, never the earlier attempt.

use super::*;
use storyhook::daemon::verification::status::VerifierStatus;
use storyhook::daemon::verification::{ActiveVerification, LandingOutcome};

/// One status read, taken from inside the fake's blocking call while the tick
/// still owns the project's slot.
#[derive(Debug)]
struct Probe {
    call: &'static str,
    /// The first journal record and durable execution identity observed at the call.
    journal: Option<serde_json::Value>,
    execution_id: Option<String>,
    status: VerifierStatus,
}

/// A fake gate that journals as the real one does: `verify` reads status
/// first, then writes the owning attempt's `run` line, then reads again.
struct JournalingGate<'a> {
    fixture: &'a ServiceFixture,
    activity: &'a VerificationActivity,
    outcomes: Mutex<VecDeque<VerificationOutcome>>,
    landings: Mutex<VecDeque<LandingOutcome>>,
    /// When set, the gate also appends a run line from the earliest attempt
    /// it saw, the way a foreign writer would, and reads status again.
    foreign_append: bool,
    /// Every attempt that wrote a `run` line, in order.
    journaled: Mutex<Vec<ActiveVerification>>,
    probes: Mutex<Vec<Probe>>,
}

impl<'a> JournalingGate<'a> {
    fn new(
        fixture: &'a ServiceFixture,
        activity: &'a VerificationActivity,
        outcomes: impl IntoIterator<Item = VerificationOutcome>,
    ) -> Self {
        Self {
            fixture,
            activity,
            outcomes: Mutex::new(outcomes.into_iter().collect()),
            landings: Mutex::new(VecDeque::new()),
            foreign_append: false,
            journaled: Mutex::new(Vec::new()),
            probes: Mutex::new(Vec::new()),
        }
    }

    fn probe(&self, call: &'static str, candidate: &VerificationCandidate) {
        let ctx = self
            .fixture
            .ctx()
            .clock(Clock::Fixed(self.fixture.env().now()));
        let status = self.activity.status(&ctx).unwrap();
        let journal = std::fs::read_to_string(journal_path(self.fixture.env(), candidate))
            .ok()
            .map(|text| {
                serde_json::from_str(text.lines().next().expect("journal header")).unwrap()
            });
        let owner = self.activity.active_for(candidate.project).unwrap();
        let execution_id = self
            .fixture
            .store()
            .read(|tx| {
                Ok(tx
                    .gate_attempts(candidate.project)?
                    .into_iter()
                    .find(|attempt| attempt.id == owner.attempt_id)
                    .and_then(|attempt| {
                        attempt
                            .executions
                            .last()
                            .map(|execution| execution.id.clone())
                    }))
            })
            .unwrap();
        self.probes.lock().unwrap().push(Probe {
            call,
            journal,
            execution_id,
            status,
        });
    }

    fn journal(&self, candidate: &VerificationCandidate, attempt: &ActiveVerification) {
        let path = journal_path(self.fixture.env(), candidate);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let execution = self
            .fixture
            .store()
            .read(|tx| {
                Ok(tx
                    .gate_attempts(candidate.project)?
                    .into_iter()
                    .find(|record| record.id == attempt.attempt_id)
                    .and_then(|record| {
                        record
                            .executions
                            .last()
                            .map(|execution| execution.id.clone())
                    }))
            })
            .unwrap()
            .expect("run belongs to a durable execution");
        let run = serde_json::json!({
            "kind": "run",
            "execution_id": execution,
            "generation": attempt.generation.unwrap().get(),
            "attempt_id": attempt.attempt_id,
            "at": self.fixture.env().now(),
        });
        use std::io::Write;
        // Production progress is append-only; do not truncate the daemon's
        // header beneath its independently running cost observer.
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(file, "{run}").unwrap();
    }

    /// The first attempt that journaled: the one a later attempt must not read.
    fn first_attempt(&self) -> ActiveVerification {
        self.journaled.lock().unwrap()[0].clone()
    }

    fn take_probes(&self) -> Vec<Probe> {
        std::mem::take(&mut *self.probes.lock().unwrap())
    }
}

impl VerificationActuator for JournalingGate<'_> {
    fn submit(
        &self,
        candidate: &VerificationCandidate,
    ) -> Result<storyhook::domain::landing::SubmissionOutcome, SubmissionFailure> {
        adopt_linked(candidate)
    }

    fn verify(
        &self,
        candidate: &VerificationCandidate,
        _pull_request: &PrLink,
    ) -> VerificationOutcome {
        self.probe("before run line", candidate);
        let owner = self
            .activity
            .active_for(candidate.project)
            .expect("a gate runs only while its attempt owns the project");
        self.journal(candidate, &owner);
        self.journaled.lock().unwrap().push(owner.clone());
        self.probe("after run line", candidate);
        if self.foreign_append {
            self.journal(candidate, &self.first_attempt());
            self.probe("foreign run line", candidate);
            self.journal(candidate, &owner);
        }
        self.outcomes
            .lock()
            .unwrap()
            .pop_front()
            .expect("every verification attempt must have a fixture outcome")
    }

    fn land(
        &self,
        _candidate: &VerificationCandidate,
        _intent: &storyhook::store::LandingIntent,
    ) -> LandingOutcome {
        self.landings
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(LandingOutcome::Merged {
                detail: "test merge confirmed".into(),
            })
    }

    fn recover_landing(
        &self,
        candidate: &VerificationCandidate,
        _intent: &storyhook::store::LandingIntent,
    ) -> LandingOutcome {
        self.probe("recover landing", candidate);
        LandingOutcome::Merged {
            detail: "test merge recovered".into(),
        }
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
}

/// Runs one production tick for the fixture's project.
fn tick(fixture: &ServiceFixture, gate: &JournalingGate<'_>) -> TickResult {
    std::fs::create_dir_all(fixture.env().daemon_state_dir()).unwrap();
    tick_with_activity(
        fixture.store(),
        fixture.env(),
        gate,
        gate.activity,
        &InFlight::new(fixture.env().clone()),
        fixture.project(),
    )
    .unwrap()
}

/// Asserts that `probe` saw a new attempt exactly as a first attempt looks
/// before its actuator: a fresh owned header (or none for landing recovery),
/// no evidence error, and no warning.
fn assert_first_attempt_state(probe: &Probe, earlier: &ActiveVerification) {
    let call = probe.call;
    let status = &probe.status;
    assert_eq!(status.evidence_error, None, "{call}: {status:?}");
    assert_eq!(status.warning, None, "{call}: {status:?}");
    let active = status
        .active
        .as_ref()
        .unwrap_or_else(|| panic!("{call}: no owner in {status:?}"));
    assert_ne!(active.attempt_id, earlier.attempt_id, "{call}");
    if call == "recover landing" {
        assert!(
            probe.journal.is_none(),
            "recovery must not reuse gate evidence"
        );
    } else {
        let header = probe
            .journal
            .as_ref()
            .expect("daemon publishes an execution header");
        assert_eq!(header["kind"], "run");
        assert_eq!(header["attempt_id"], active.attempt_id);
        assert_eq!(header["generation"], active.generation.unwrap().get());
        let execution = probe
            .execution_id
            .as_ref()
            .expect("durably admitted execution");
        assert!(!execution.is_empty());
        assert_eq!(header["execution_id"], *execution);
    }
}

fn only<'p>(probes: &'p [Probe], call: &str) -> &'p Probe {
    let matching: Vec<_> = probes.iter().filter(|probe| probe.call == call).collect();
    assert_eq!(matching.len(), 1, "{call}: {probes:?}");
    matching[0]
}

#[test]
fn a_resubmission_reads_as_a_first_attempt_until_its_own_run_line() {
    let fixture = ServiceFixture::new();
    fixture.github_checkout("https://github.com/acme/widgets");
    let id = submitted(
        &fixture,
        "returned then resubmitted",
        Priority::High,
        PR_ONE,
    );
    let activity = VerificationActivity::new();
    let mut gate = JournalingGate::new(
        &fixture,
        &activity,
        [
            VerificationOutcome::TestsFailed {
                tree: "abc123".into(),
                log: "red.log".into(),
                detail: "red".into(),
                gate: GateCommand::DEFAULT.into(),
            },
            VerificationOutcome::Certified {
                head: "a".repeat(40),
                tree: "b".repeat(40),
                detail: "landed".into(),
                gate: GateCommand::DEFAULT.into(),
            },
        ],
    );
    assert_eq!(tick(&fixture, &gate), TickResult::Returned);
    let earlier = gate.first_attempt();
    gate.take_probes();
    // An unproved red gate holds its generation. Only an explicit operator
    // withdrawal/resubmission starts the new attempt whose journal we inspect.
    assert_eq!(story_row(&fixture, &id).state, "verifying");
    assert!(
        VerificationQueue::new(fixture.store())
            .next()
            .unwrap()
            .is_none()
    );
    StoryService::new(&fixture.ctx())
        .set_state(&id, "in-progress", None, Some("verifying"), None)
        .unwrap();
    StoryService::new(&fixture.ctx())
        .set_state(&id, "verifying", None, Some("in-progress"), None)
        .unwrap();

    gate.foreign_append = true;
    assert_eq!(tick(&fixture, &gate), TickResult::Completed);

    let probes = gate.take_probes();
    assert_first_attempt_state(only(&probes, "before run line"), &earlier);
    let after = only(&probes, "after run line");
    assert!(after.journal.is_some());
    assert_eq!(after.status.evidence_error, None, "{:?}", after.status);
    assert_eq!(after.status.warning, None, "{:?}", after.status);
    // The detector stays: once the attempt has its own run line, a journal
    // that names another attempt is still unavailable evidence.
    let foreign = only(&probes, "foreign run line");
    let error = foreign.status.evidence_error.as_deref().unwrap_or_default();
    assert!(
        error.contains("does not identify the active generation and attempt"),
        "{:?}",
        foreign.status
    );
    assert!(
        foreign
            .status
            .warning
            .as_deref()
            .is_some_and(|warning| warning.contains("evidence unavailable")),
        "{:?}",
        foreign.status
    );
}

#[test]
fn a_retry_of_the_same_generation_reads_as_a_first_attempt() {
    let fixture = ServiceFixture::new();
    fixture.github_checkout("https://github.com/acme/widgets");
    submitted(&fixture, "retried", Priority::High, PR_ONE);
    let activity = VerificationActivity::new();
    let gate = JournalingGate::new(
        &fixture,
        &activity,
        [
            VerificationOutcome::InfrastructureFailure {
                detail: "head ref did not converge".into(),
                disposition: VerificationFailureDisposition::Retryable,
            },
            VerificationOutcome::Certified {
                head: "a".repeat(40),
                tree: "b".repeat(40),
                detail: "landed".into(),
                gate: GateCommand::DEFAULT.into(),
            },
        ],
    );
    assert_eq!(tick(&fixture, &gate), TickResult::RetryLater);
    let earlier = gate.first_attempt();
    gate.take_probes();

    assert_eq!(tick(&fixture, &gate), TickResult::Completed);

    let probes = gate.take_probes();
    let before = only(&probes, "before run line");
    assert_first_attempt_state(before, &earlier);
    assert_eq!(
        before.status.active.as_ref().unwrap().generation,
        earlier.generation,
        "a retry keeps its generation"
    );
}

#[test]
fn a_landing_recovery_reads_as_a_first_attempt() {
    let fixture = ServiceFixture::new();
    fixture.github_checkout("https://github.com/acme/widgets");
    submitted(&fixture, "landing uncertain", Priority::High, PR_ONE);
    let activity = VerificationActivity::new();
    let gate = JournalingGate::new(
        &fixture,
        &activity,
        [VerificationOutcome::Certified {
            head: "a".repeat(40),
            tree: "b".repeat(40),
            detail: "certified".into(),
            gate: GateCommand::DEFAULT.into(),
        }],
    );
    gate.landings
        .lock()
        .unwrap()
        .push_back(LandingOutcome::Uncertain {
            detail: "merge request sent; no answer".into(),
        });
    assert_eq!(tick(&fixture, &gate), TickResult::RetryLater);
    let earlier = gate.first_attempt();
    gate.take_probes();

    assert_eq!(tick(&fixture, &gate), TickResult::Completed);

    let probes = gate.take_probes();
    assert_first_attempt_state(only(&probes, "recover landing"), &earlier);
}

#[test]
fn an_unproved_conflict_never_hands_over_and_explicit_resubmission_has_a_fresh_journal() {
    let fixture = ServiceFixture::new();
    fixture.github_checkout("https://github.com/acme/widgets");
    let held = submitted(&fixture, "reconciled", Priority::High, PR_ONE);
    let activity = VerificationActivity::new();
    let gate = JournalingGate::new(
        &fixture,
        &activity,
        [
            VerificationOutcome::Conflict {
                detail: "both modified src/lib.rs".into(),
            },
            VerificationOutcome::Certified {
                head: "a".repeat(40),
                tree: "b".repeat(40),
                detail: "landed after reconciliation".into(),
                gate: GateCommand::DEFAULT.into(),
            },
        ],
    );
    std::fs::create_dir_all(fixture.env().daemon_state_dir()).unwrap();

    let result = tick_with_reconciliation(
        fixture.store(),
        fixture.env(),
        &gate,
        &activity,
        &InFlight::new(fixture.env().clone()),
        fixture.project(),
        |_| panic!("unproved conflict must not wait for or assign implementer repair"),
    )
    .unwrap();

    assert_eq!(result, TickResult::Returned);
    assert_eq!(story_row(&fixture, &held).state, "verifying");
    assert!(activity.active_for(fixture.project()).is_none());
    assert!(
        VerificationQueue::new(fixture.store())
            .next()
            .unwrap()
            .is_none()
    );
    let status = activity.status(&fixture.ctx()).unwrap();
    assert_eq!(status.attribution_holds.len(), 1);
    assert_eq!(status.attribution_holds[0].story_id, held);
    // The operator can later withdraw and resubmit; it is a separate tick,
    // and must not borrow the held attempt's progress journal.
    StoryService::new(&fixture.ctx())
        .set_state(&held, "in-progress", None, Some("verifying"), None)
        .unwrap();
    StoryService::new(&fixture.ctx())
        .set_state(&held, "verifying", None, Some("in-progress"), None)
        .unwrap();
    assert_eq!(tick(&fixture, &gate), TickResult::Completed);
    let earlier = gate.first_attempt();
    let probes = gate.take_probes();
    let before: Vec<_> = probes
        .iter()
        .filter(|probe| probe.call == "before run line")
        .collect();
    assert_eq!(before.len(), 2, "{probes:?}");
    assert_first_attempt_state(before[1], &earlier);
    assert_ne!(
        before[1].status.active.as_ref().unwrap().generation,
        earlier.generation,
        "the hand-over owns the resubmitted generation"
    );
}
