//! Complete managed worker flow. Only remote observations/transport and the
//! gate workload are fixtures; real assembly, ownership CAS, central accounting,
//! certificate construction, original completion and private settlement execute.
use super::*;
use crate::daemon::{
    bus::ChangeBus,
    lifecycle::InFlight,
    verification::{
        self, LandingOutcome, NotifyDelivery, ResumePlan, SubmissionFailure, VerificationActivity,
        VerificationActuator, VerificationOutcome, integration_worker,
    },
};
use crate::domain::landing::SubmissionOutcome;
use crate::service::integration_recovery::publication::worker_fixture::{Fault, Remote};
use crate::store::VerificationFailureDisposition;
use std::{
    os::fd::AsRawFd,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Pass,
    GateFails,
    ChangedInputs,
    CancelGate,
    MergeUncertain,
}
#[derive(Default)]
struct Observed {
    gate_calls: usize,
    merge_calls: usize,
    input_reads: usize,
    merged: bool,
    roots: Vec<std::path::PathBuf>,
    slots_during_effects: usize,
    cancellation_witnessed: bool,
    descendant_settled_with_slot: bool,
}
struct Native<'a> {
    fixture: &'a OwnedFixture,
    remote: &'a Remote,
    mode: Mode,
    state: Arc<Mutex<Observed>>,
    activity: VerificationActivity,
}
impl integration_worker::native::NativeOperations for Native<'_> {
    fn inspect(
        &self,
        c: &VerificationCandidate,
        h: &str,
        _: &Environment,
        d: Instant,
        x: &Cancellation,
    ) -> Result<BoundInspection, AppError> {
        assert_eq!(c, &self.fixture.candidate);
        assert_eq!(h, self.fixture.native.head);
        let Inspection::Proposed(proposal) =
            inspect(&c.checkout, &self.fixture.native.base, h, d, x.clone())?
        else {
            return Err(AppError::Validation(
                "real worker fixture inspection was not proposed".into(),
            ));
        };
        Ok(BoundInspection::Proposed(BoundIntegrationProposal {
            proposal,
            deadline: d,
            cancellation: x.clone(),
            submission: SubmissionObservation {
                checkout: c.checkout.clone(),
                repository: "github.com/acme/widgets".into(),
                pull_request: c.pull_request.as_ref().unwrap().url.clone(),
                base_branch: "dev".into(),
                base: self.fixture.native.base.clone(),
                head: h.into(),
            },
        }))
    }
    fn clean(
        &self,
        _: &VerificationCandidate,
        _: &str,
        _: &Environment,
        _: Instant,
        _: &Cancellation,
    ) -> Result<NativeCleanIntegration, AppError> {
        panic!("conflict fixture became clean")
    }
    fn publish<S: Store>(
        &self,
        s: &IntegrationOwnerService<'_, S>,
        c: &mut PublicationClaim,
        p: &BoundIntegrationProposal,
        d: Instant,
        x: &Cancellation,
    ) -> Result<NativePublication, AppError> {
        self.remote.publish(s, c, p, d, x)
    }
    fn gate_inputs<S: Store>(
        &self,
        _: &IntegrationOwnerService<'_, S>,
        c: &IntegrationGateClaim,
        d: Instant,
        x: &Cancellation,
    ) -> Result<NativeIntegrationGateInputs, AppError> {
        let mut state = self.state.lock().unwrap();
        state.input_reads += 1;
        let mut evidence = IntegrationGateInputsEvidence {
            version: 1,
            owner: c.id().into(),
            attempt: c.attempt().into(),
            publication: c.publication().clone(),
            current_base: c.assembly().plan.base.clone(),
            base_branch: c.publication().original.base_branch.clone(),
            tree: c.publication().tree.clone(),
            policy: c.assembly().plan.policy.clone(),
            parents: c.publication().parents.clone(),
        };
        if self.mode == Mode::ChangedInputs && state.input_reads == 2 {
            evidence.current_base = "f".repeat(40);
        }
        Ok(gate_inputs::fixture_gate_inputs(evidence, d, x.clone()))
    }
    fn landed<S: Store>(
        &self,
        _: &IntegrationOwnerService<'_, S>,
        q: IntegrationLandingObservation,
        d: Instant,
        x: &Cancellation,
    ) -> Result<NativeIntegrationLanded, AppError> {
        let mut state = self.state.lock().unwrap();
        assert!(self.activity.active_for(q.candidate().project).is_some());
        state.slots_during_effects += 1;
        if !state.merged {
            return Err(AppError::Validation(
                "remote fixture has no native merged fact".into(),
            ));
        }
        let p = q.publication();
        let evidence = IntegrationLandedEvidence {
            version: 1,
            owner: q.id().into(),
            intent_id: q.intent().id.clone(),
            repository: p.original.repository.clone(),
            original_pr: p.original.pull_request.clone(),
            original_head: p.original.head.clone(),
            managed_pr: p.pull_request.clone(),
            managed_head: p.commit.clone(),
            merge_commit: p.commit.clone(),
            merge_tree: p.tree.clone(),
            base_branch: p.original.base_branch.clone(),
            observed_base: p.commit.clone(),
            observed_base_tree: p.tree.clone(),
        };
        let native = landed_observation::fixture_landed(q, evidence, d, x.clone())?;
        state.roots.push(native.observation_path().to_path_buf());
        Ok(native)
    }
    fn branch(
        &self,
        e: &Environment,
        a: &AssemblyEvidence,
        _: Instant,
        _: &Cancellation,
    ) -> RetainedBranchObservation {
        assert!(
            self.activity
                .active_for(self.fixture.candidate.project)
                .is_some()
        );
        let remote = self.remote.snapshot();
        assert_eq!(remote.branch_head.as_deref(), Some(a.commit.as_str()));
        RetainedBranchObservation {
            version: 1,
            owner: a.owner.clone(),
            assembly_epoch: a.epoch,
            repository: a.submission.repository.clone(),
            reference: format!("refs/heads/{}", a.branch),
            expected_head: a.commit.clone(),
            observed_at: e.now(),
            outcome: RetainedBranchOutcome::RetainedExact,
        }
    }
}
struct Actuator<'a> {
    store: &'a SqliteStore,
    env: Environment,
    activity: VerificationActivity,
    state: Arc<Mutex<Observed>>,
    mode: Mode,
}
impl VerificationActuator for Actuator<'_> {
    fn submit(&self, _: &VerificationCandidate) -> Result<SubmissionOutcome, SubmissionFailure> {
        panic!("managed worker resubmitted author")
    }
    fn verify(&self, _: &VerificationCandidate, _: &crate::store::PrLink) -> VerificationOutcome {
        panic!("managed gate used ordinary adapter")
    }
    fn land(&self, _: &VerificationCandidate, _: &crate::store::LandingIntent) -> LandingOutcome {
        panic!("managed merge used ordinary adapter")
    }
    fn recover_landing(
        &self,
        _: &VerificationCandidate,
        _: &crate::store::LandingIntent,
    ) -> LandingOutcome {
        panic!("managed merge used legacy recovery")
    }
    fn notify(&self, _: &VerificationCandidate, _: &str) -> Result<NotifyDelivery, AppError> {
        panic!("managed flow notified author")
    }
    fn redispatch(&self, _: &VerificationCandidate, _: &ResumePlan) -> Result<(), AppError> {
        panic!("managed flow redispatched author")
    }
    fn reap(&self, _: &VerificationCandidate) -> Result<(), AppError> {
        panic!("managed flow reclaimed author resources")
    }
    fn verify_integration(
        &self,
        c: &VerificationCandidate,
        managed: &crate::store::PrLink,
        x: &Cancellation,
    ) -> VerificationOutcome {
        let active = self.activity.active_for(c.project).unwrap();
        let record = self
            .store
            .read(|tx| tx.integration_recoveries(c.project))
            .unwrap()
            .pop()
            .unwrap();
        let owner: IntegrationOwner = serde_json::from_value(record.state).unwrap();
        assert_eq!(
            managed.url,
            owner.publication.as_ref().unwrap().pull_request
        );
        assert_ne!(managed.url, c.pull_request.as_ref().unwrap().url);
        let assembly = owner.assembly.unwrap();
        let admission = self
            .store
            .read(|tx| tx.gate_attempts(c.project))
            .unwrap()
            .into_iter()
            .find(|a| a.id == active.attempt_id)
            .unwrap();
        let execution = admission.executions.last().unwrap();
        assert!(execution.purpose.is_gate());
        assert_eq!(execution.submissions, vec![admission.submission.clone()]);
        self.state.lock().unwrap().gate_calls += 1;
        let ready = self.env.home().join("managed-fixture-gate-ready");
        let lease = self.env.home().join("managed-fixture-child-lease");
        let custody = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&lease)
            .unwrap();
        let child_seen = AtomicBool::new(false);
        let lease_witnessed = AtomicBool::new(false);
        let script = r#"import fcntl,json,os,subprocess,sys,time
root,head,tree,journal,attempt,execution,ready,lease,mode=sys.argv[1:]
with open(journal) as f: run=json.loads(f.readline())
assert run['attempt_id']==attempt and run['execution_id']==execution
actual=subprocess.check_output(['/usr/bin/git','--git-dir='+root,'rev-parse',head+'^{tree}'],env={'PATH':'/usr/bin:/bin','GIT_CONFIG_NOSYSTEM':'1','GIT_CONFIG_GLOBAL':'/dev/null','GIT_NO_REPLACE_OBJECTS':'1'}).decode().strip()
assert actual==tree
if mode=='cancel':
    if os.fork()==0:
        with open(lease,'r+') as owned:
            fcntl.flock(owned,fcntl.LOCK_EX)
            with open(ready+'.tmp','x') as f: f.write('descendant holds its native file lease')
            os.rename(ready+'.tmp',ready)
            while True: time.sleep(.02)
    while True: time.sleep(.02)
with open(journal,'a') as f:
    f.write(json.dumps({'kind':'case','path':'fixture','name':'managed native tree','outcome':'fail' if mode=='fail' else 'pass'})+'\n')
    f.write(json.dumps({'kind':'item','path':'fixture/managed-native-tree','status':'fail' if mode=='fail' else 'pass'})+'\n')
assert mode!='fail','fixture requested a real failing gate'
"#;
        let mut command = std::process::Command::new("python3");
        command
            .arg("-c")
            .arg(script)
            .arg(&assembly.workspace.path)
            .arg(&assembly.commit)
            .arg(&assembly.tree)
            .arg(&execution.journal_path)
            .arg(&active.attempt_id)
            .arg(&execution.id)
            .arg(&ready)
            .arg(&lease)
            .arg(if self.mode == Mode::CancelGate {
                "cancel"
            } else if self.mode == Mode::GateFails {
                "fail"
            } else {
                "pass"
            });
        let deadline = Instant::now()
            + storyhook_test_support::load_grace::graced_now(Duration::from_secs(30));
        let captured = crate::process::run_captured_query_quiescent(
            command,
            deadline,
            &|| {
                if self.mode == Mode::CancelGate
                    && ready.exists()
                    && !child_seen.load(Ordering::SeqCst)
                {
                    // Readiness was published only after the descendant held
                    // this real native lease. Its process still owns effects.
                    let locked =
                        unsafe { libc::flock(custody.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
                    lease_witnessed.store(
                        locked == -1
                            && std::io::Error::last_os_error().raw_os_error()
                                == Some(libc::EWOULDBLOCK),
                        Ordering::SeqCst,
                    );
                    if locked == 0 {
                        unsafe { libc::flock(custody.as_raw_fd(), libc::LOCK_UN) };
                    }
                    child_seen.store(true, Ordering::SeqCst);
                    x.cancel();
                }
                x.is_cancelled()
            },
            1024 * 1024,
            &[1],
        );
        if self.mode == Mode::CancelGate {
            assert!(
                child_seen.load(Ordering::SeqCst),
                "gate never reached owned descendant readiness"
            );
            assert!(
                lease_witnessed.load(Ordering::SeqCst),
                "readiness did not prove live descendant custody"
            );
            assert!(x.is_cancelled(), "original gate token was not cancelled");
            assert!(
                self.activity.active_for(c.project).is_some(),
                "slot released before child settlement"
            );
            assert_eq!(
                unsafe { libc::flock(custody.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
                0,
                "owned descendant retained effects after capture returned"
            );
            let mut state = self.state.lock().unwrap();
            state.cancellation_witnessed = true;
            state.descendant_settled_with_slot = true;
            return VerificationOutcome::Cancelled;
        }
        match captured {
            Ok(out) if out.status.success() => VerificationOutcome::Certified {
                head: assembly.commit,
                tree: assembly.tree,
                detail: "actual owned native tree fixture passed".into(),
                gate: "fixture native tree".into(),
            },
            Ok(out) => VerificationOutcome::TestsFailed {
                tree: assembly.tree,
                log: String::from_utf8_lossy(&out.stderr).into_owned(),
                detail: "actual owned fixture failed".into(),
                gate: "fixture native tree".into(),
            },
            Err(error) => VerificationOutcome::InfrastructureFailure {
                detail: error.detail(),
                disposition: VerificationFailureDisposition::Permanent,
            },
        }
    }
    fn land_integration(&self, c: &IntegrationLandingClaim) -> LandingOutcome {
        assert!(self.activity.active_for(c.candidate().project).is_some());
        assert!(c.take_request().unwrap());
        assert!(!c.take_request().unwrap());
        let mut state = self.state.lock().unwrap();
        state.merge_calls += 1;
        state.slots_during_effects += 1;
        if self.mode == Mode::MergeUncertain {
            return LandingOutcome::Uncertain {
                detail: "remote reply lost; no merged fact".into(),
            };
        }
        state.merged = true;
        LandingOutcome::Merged {
            detail: "fixture remote accepted managed PR".into(),
        }
    }
}

fn run(
    mode: Mode,
    fault: Fault,
) -> (
    OwnedFixture,
    Result<verification::TickResult, AppError>,
    Arc<Mutex<Observed>>,
    Remote,
) {
    let f = OwnedFixture::new(false);
    let subject = super::pending::settled_subject(&f);
    let ctx = f.ctx();
    let proof = f.proof();
    let remote = Remote::new(proof.submission().clone(), 99, fault).unwrap();
    proof.settle().unwrap();
    let activity = VerificationActivity::new();
    let state = Arc::new(Mutex::new(Observed::default()));
    let actuator = Actuator {
        store: &f.store,
        env: ctx.env().clone(),
        activity: activity.clone(),
        state: state.clone(),
        mode,
    };
    let native = Native {
        fixture: &f,
        remote: &remote,
        mode,
        state: state.clone(),
        activity: activity.clone(),
    };
    let result = integration_worker::run_observed_fixture(
        &f.store,
        ctx.env(),
        &ChangeBus::new(),
        &actuator,
        &activity,
        &InFlight::new(ctx.env().clone()),
        &subject,
        &native,
    );
    assert!(
        activity.active_for(f.candidate.project).is_none(),
        "central slot escaped worker return"
    );
    drop(native);
    drop(actuator);
    drop(ctx);
    (f, result, state, remote)
}

#[test]
fn managed_worker_full_flow_keeps_real_accounting_original_identity_and_settles_owned_resources() {
    let (f, result, state, remote) = run(Mode::Pass, Fault::None);
    assert_eq!(result.unwrap(), verification::TickResult::Completed);
    let state = state.lock().unwrap();
    assert_eq!(state.gate_calls, 1);
    assert_eq!(state.merge_calls, 1);
    assert_eq!(state.input_reads, 2);
    assert_eq!(state.slots_during_effects, 2);
    assert!(state.roots.iter().all(|p| !p.exists()));
    let row = f
        .store
        .read(|tx| tx.integration_recoveries(f.candidate.project))
        .unwrap()
        .pop()
        .unwrap();
    let owner: IntegrationOwner = serde_json::from_value(row.state).unwrap();
    assert!(!row.active);
    assert_eq!(owner.phase, IntegrationPhase::Landed);
    assert!(!owner.workspace.exists());
    assert_eq!(
        owner.candidate.verifying_generation,
        f.candidate.verifying_generation
    );
    assert_eq!(owner.submission.head, f.native.head);
    let attempts = f
        .store
        .read(|tx| tx.gate_attempts(f.candidate.project))
        .unwrap();
    let actual = attempts
        .iter()
        .find(|a| a.id == owner.gate_attempt.clone().unwrap())
        .unwrap();
    assert!(actual.finished_at.is_some());
    assert_eq!(actual.executions.len(), 1);
    let physical = &actual.executions[0];
    assert!(physical.journal_bound);
    assert!(physical.finished_at.is_some());
    assert!(!physical.estimated);
    assert!(physical.purpose.is_gate());
    assert!(
        physical.diagnostics.is_empty(),
        "{:?}",
        physical.diagnostics
    );
    assert_eq!(physical.legs.len(), 1);
    assert_eq!(physical.legs[0].path, "fixture/managed-native-tree");
    assert_eq!(physical.legs[0].status, "pass");
    assert_eq!(
        physical.journal_offset,
        std::fs::metadata(&physical.journal_path).unwrap().len()
    );
    assert_eq!(physical.submissions, vec![actual.submission.clone()]);
    let prefix = f
        .store
        .read(|tx| crate::service::project_prefix(tx, f.candidate.project))
        .unwrap();
    let story = crate::store::StoryNo::parse_id(&prefix, &f.candidate.story_id).unwrap();
    let events = f
        .store
        .read(|tx| tx.events_for(f.candidate.project, story))
        .unwrap();
    assert!(!events.iter().any(|e| matches!(
        e.known(),
        Some(crate::domain::StoryEvent::StoryPrMerged { .. })
    )));
    let remote = remote.snapshot();
    assert_eq!(remote.push_calls, 1);
    assert_eq!(remote.create_calls, 1);
    assert!(remote.branch_head.is_some(), "managed branch was deleted");
}

#[test]
fn managed_worker_failed_or_changed_gate_never_claims_merge_or_original_completion() {
    for mode in [Mode::GateFails, Mode::ChangedInputs, Mode::CancelGate] {
        let (f, result, state, _) = run(mode, Fault::None);
        let error = result.unwrap_err().to_string();
        let observed = state.lock().unwrap();
        assert_eq!(
            observed.gate_calls, 1,
            "did not reach physical gate: {error}"
        );
        assert_eq!(observed.merge_calls, 0);
        assert_eq!(
            observed.input_reads,
            if mode == Mode::ChangedInputs { 2 } else { 1 }
        );
        if mode == Mode::ChangedInputs {
            assert!(
                error.contains("inputs changed before certification"),
                "{error}"
            );
        }
        if mode == Mode::CancelGate {
            assert!(observed.cancellation_witnessed && observed.descendant_settled_with_slot);
        }
        drop(observed);
        let attempts = f
            .store
            .read(|tx| tx.gate_attempts(f.candidate.project))
            .unwrap();
        let physical = attempts
            .iter()
            .flat_map(|a| &a.executions)
            .find(|e| e.id != "original-native-conflict")
            .expect("real managed execution missing");
        assert!(physical.journal_bound && physical.finished_at.is_some() && !physical.estimated);
        assert_eq!(
            physical.verdict.as_deref(),
            Some(match mode {
                Mode::GateFails => "tests-failed",
                Mode::ChangedInputs => "certified",
                Mode::CancelGate => "interrupted",
                _ => unreachable!(),
            })
        );
        assert!(
            physical.diagnostics.is_empty(),
            "{:?}",
            physical.diagnostics
        );
        if mode == Mode::GateFails {
            assert_eq!(physical.failed_cases.len(), 1);
            assert_eq!(physical.failed_cases[0].path, "fixture");
            assert_eq!(
                physical.failed_cases[0].name.as_deref(),
                Some("managed native tree")
            );
            assert_eq!(physical.legs[0].status, "fail");
        }
        let row = f
            .store
            .read(|tx| tx.integration_recoveries(f.candidate.project))
            .unwrap()
            .pop()
            .unwrap();
        assert!(row.active);
        let owner: IntegrationOwner = serde_json::from_value(row.state).unwrap();
        assert!(owner.hold.is_some());
        assert!(owner.workspace.exists());
        assert!(owner.landed.is_none());
        assert!(f.store.read(|tx| tx.landing_intents()).unwrap().is_empty());
    }
}

#[test]
fn managed_worker_unknown_remote_effects_retain_identity_without_republication_or_merge_replay() {
    for (mode, fault) in [
        (Mode::Pass, Fault::PushReplyLost),
        (Mode::Pass, Fault::CreateReplyLost),
        (Mode::MergeUncertain, Fault::None),
    ] {
        let (f, result, state, remote) = run(mode, fault);
        assert!(result.is_err());
        let snapshot = remote.snapshot();
        assert_eq!(snapshot.push_calls, 1);
        assert!(snapshot.create_calls <= 1);
        assert!(state.lock().unwrap().merge_calls <= 1);
        assert!(
            f.store
                .read(|tx| crate::service::integration_recovery::pending_subjects(
                    tx,
                    f.candidate.project
                ))
                .unwrap()
                .is_empty()
        );
        let row = f
            .store
            .read(|tx| tx.integration_recoveries(f.candidate.project))
            .unwrap()
            .pop()
            .unwrap();
        assert!(row.active);
        let owner: IntegrationOwner = serde_json::from_value(row.state).unwrap();
        assert!(owner.workspace.exists());
        assert!(owner.landed.is_none());
        assert!(owner.hold.is_some());
        let reopened = SqliteStore::open(f.store.path()).unwrap();
        assert_eq!(
            reopened
                .read(|tx| tx.integration_recoveries(f.candidate.project))
                .unwrap()[0]
                .id,
            row.id
        );
        // A cached original queue subject is not authority to repeat a
        // publication or merge after restart. Exercise the real admission and
        // orchestration again against the reopened durable store.
        let subject = PendingIntegration {
            candidate: owner.candidate.clone(),
            attribution: owner.attribution.id.clone(),
            component: owner.component.clone(),
            retained_head: owner.submission.head.clone(),
        };
        let env = f.ctx().env().clone();
        let activity = VerificationActivity::new();
        let actuator = Actuator {
            store: &reopened,
            env: env.clone(),
            activity: activity.clone(),
            state: state.clone(),
            mode,
        };
        let native = Native {
            fixture: &f,
            remote: &remote,
            mode,
            state: state.clone(),
            activity: activity.clone(),
        };
        let effects = {
            let observed = state.lock().unwrap();
            (observed.gate_calls, observed.merge_calls)
        };
        let retried = integration_worker::run_observed_fixture(
            &reopened,
            &env,
            &ChangeBus::new(),
            &actuator,
            &activity,
            &InFlight::new(env.clone()),
            &subject,
            &native,
        );
        assert!(!matches!(retried, Ok(verification::TickResult::Completed)));
        assert!(activity.active_for(f.candidate.project).is_none());
        let observed = state.lock().unwrap();
        assert_eq!((observed.gate_calls, observed.merge_calls), effects);
        assert_eq!(
            remote.snapshot(),
            snapshot,
            "cached original subject replayed a remote effect after restart"
        );
    }
}
