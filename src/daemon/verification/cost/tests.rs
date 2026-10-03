use super::*;
use crate::service::NewStoryInput;
use crate::store::SqliteStore;
use storyhook_test_support::{ServiceFixture, load_grace};

struct Board {
    _fixture: ServiceFixture,
    store: SqliteStore,
    env: Environment,
    candidate: VerificationCandidate,
    activity: VerificationActivity,
}

impl Board {
    fn new() -> Self {
        let fixture = ServiceFixture::new();
        let store = SqliteStore::open(fixture.store().path()).unwrap();
        let project = ProjectId::new(fixture.project().get());
        let env = Environment::at(fixture.cwd());
        let ctx = Ctx::new(&store, project, env.home().to_path_buf(), env.clone()).no_hooks(true);
        let story = StoryService::new(&ctx)
            .create(&NewStoryInput {
                title: "Cost observation".into(),
                ..Default::default()
            })
            .unwrap();
        StoryService::new(&ctx)
            .set_state(&story.id, "verifying", None, None, None)
            .unwrap();
        let candidate = VerificationQueue::new(&store).next().unwrap().unwrap();
        Self {
            _fixture: fixture,
            store,
            env,
            candidate,
            activity: VerificationActivity::new(),
        }
    }

    fn admit(&self) -> VerificationGuard {
        self.activity
            .try_acquire(&self.store, &self.env, &self.candidate, self.env.now())
            .unwrap()
            .unwrap()
    }

    fn rows(&self) -> Vec<GateAttempt> {
        self.store
            .read(|tx| tx.gate_attempts(self.candidate.project))
            .unwrap()
    }

    fn age(&self, id: &str, seconds: u64) {
        self.activity
            .costs
            .lock()
            .unwrap()
            .get_mut(id)
            .unwrap()
            .start = Instant::now()
            .checked_sub(Duration::from_secs(seconds))
            .unwrap();
    }
}

#[test]
fn admission_is_durable_before_preparation_and_breach_does_not_cancel() {
    let board = Board::new();
    let guard = board.admit();
    let first = board.rows();
    assert_eq!(first.len(), 1, "admission must precede all preparation");
    assert_eq!(first[0].id, guard.active.attempt_id);
    board.age(&guard.active.attempt_id, 900);
    sample(&board.store, &board.activity, board.candidate.project).unwrap();
    let rows = board.rows();
    assert_eq!(rows[0].budget_status(), "process-budget-breach");
    assert!(rows[0].executions.is_empty());
    assert!(!guard.is_cancelled());
    assert!(
        !journal_path(&board.env, &board.candidate).exists(),
        "sampling must not emit progress"
    );
    drop(guard);
    sample(&board.store, &board.activity, board.candidate.project).unwrap();
    assert!(board.rows()[0].finished_at.is_some());
}

#[test]
fn a_real_child_observes_durable_breach_without_status_polling_or_heartbeat() {
    let board = Board::new();
    observe(&board.store, &board.env, &board.activity, board.candidate.project, || {
        let guard = board.admit();
        // The child waits for SQLite, not for progress publication. Its named
        // failure proves the real append/import/archive path, including UTF-8.
        let script = r#"
import json, sqlite3, sys, time
db, attempt, journal, patience = sys.argv[1:]
original = open(journal, 'rb').read()
deadline = time.monotonic() + float(patience)
while True:
    with sqlite3.connect(db) as connection:
        row = connection.execute('SELECT payload FROM gate_attempts WHERE id=?', (attempt,)).fetchone()
    if row and json.loads(row[0])['elapsed']['breached_at'] is not None:
        break
    if time.monotonic() >= deadline:
        raise AssertionError('observer never persisted breach while child was alive')
    time.sleep(0.02)
assert open(journal, 'rb').read() == original, 'sampler wrote a progress heartbeat'
with open(journal, 'a') as output:
    output.write(json.dumps({'kind':'case','outcome':'fail','path':'unit','name':'parser::literal "é"','target':'parser'}) + '\n')
"#;
        execute(&board.store, &board.env, &guard, &board.candidate, GateInputs::default(), vec![submission(&board.candidate)], || {
            board.age(&guard.active.attempt_id, 900);
            let patience = load_grace::graced_now(Duration::from_secs(30)).as_secs_f64().to_string();
            let output = Command::new("python3").arg("-c").arg(script)
                .arg(board.store.path()).arg(&guard.active.attempt_id)
                .arg(journal_path(&board.env, &board.candidate)).arg(patience).output().unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            assert!(!guard.is_cancelled(), "a budget breach must not stop owned work");
            VerificationOutcome::Cancelled
        }, |outcome| Ok(Some(outcome.clone())))?;
        Ok(())
    }).unwrap();
    let rows = board.rows();
    let execution = &rows[0].executions[0];
    assert_eq!(rows[0].budget_status(), "process-budget-breach");
    assert_eq!(
        execution.failed_cases[0].name.as_deref(),
        Some("parser::literal \"é\"")
    );
    assert!(execution.journal_bound);
    assert!(execution.finished_at.is_some());
    assert!(std::path::Path::new(&execution.journal_path).exists());
    assert!(
        journal_path(&board.env, &board.candidate).exists(),
        "existing progress readers retain their source"
    );
    assert!(board.activity.costs.lock().unwrap().is_empty());
}

#[test]
fn retry_links_prior_admission_without_erasing_cost_or_reusing_execution_identity() {
    let board = Board::new();
    for generation in 0..2 {
        observe(
            &board.store,
            &board.env,
            &board.activity,
            board.candidate.project,
            || {
                let guard = board.admit();
                board.age(&guard.active.attempt_id, 900);
                for tree in ["a", "b"] {
                    execute(
                        &board.store,
                        &board.env,
                        &guard,
                        &board.candidate,
                        GateInputs {
                            tree: Some(tree.repeat(40)),
                            ..Default::default()
                        },
                        vec![submission(&board.candidate)],
                        || VerificationOutcome::Cancelled,
                        |outcome| Ok(Some(outcome.clone())),
                    )?;
                }
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(board.rows().len(), generation + 1);
    }
    let rows = board.rows();
    assert_eq!(
        rows[1].previous_attempt.as_deref(),
        Some(rows[0].id.as_str())
    );
    assert_eq!(rows[0].submission, rows[1].submission);
    let ids: BTreeSet<_> = rows
        .iter()
        .flat_map(|a| a.executions.iter().map(|e| &e.id))
        .collect();
    assert_eq!(ids.len(), 4);
    assert!(
        rows.iter()
            .all(|a| a.elapsed.milliseconds >= 900_000 && a.executions.len() == 2)
    );
}

#[test]
fn restart_keeps_unknown_completion_and_crosses_budget_from_utc_checkpoint() {
    let board = Board::new();
    let guard = board.admit();
    assert_eq!(board.rows().len(), 1);
    let id = guard.active.attempt_id.clone();
    let at = "2026-10-03T00:00:00Z";
    // Simulate process loss: the live guard's in-memory end never reaches Store.
    drop(guard);
    let mut record = board.rows().remove(0);
    record.elapsed.checkpoint_at = at.into();
    record.executions.push(GateExecution::new(
        "lost".into(),
        at,
        "/missing/lost-journal".into(),
    ));
    record.revision += 1;
    board
        .store
        .write(|tx| tx.update_gate_attempt(&record, 0))
        .unwrap();
    restart(
        &board.store,
        board.candidate.project,
        "2026-10-03T00:15:00Z",
    )
    .unwrap();
    let rows = board.rows();
    assert_eq!(rows[0].id, id);
    assert_eq!(rows[0].budget_status(), "process-budget-breach");
    assert!(rows[0].elapsed.estimated);
    assert_eq!(rows[0].verdict.as_deref(), Some("interrupted"));
    assert_eq!(rows[0].executions[0].milliseconds, None);
    restart(
        &board.store,
        board.candidate.project,
        "2026-10-03T00:20:00Z",
    )
    .unwrap();
    assert_eq!(
        board.rows(),
        rows,
        "recovery must not count the same restart twice"
    );
}

#[test]
fn failed_admission_write_publishes_no_owner_or_cost_row() {
    use crate::store::fault::{FaultAction, FaultPoint, arm};
    let board = Board::new();
    let failure = arm(
        FaultPoint::BeforeCommit,
        FaultAction::Fail("cost admission unavailable".into()),
    );
    let result =
        board
            .activity
            .try_acquire(&board.store, &board.env, &board.candidate, board.env.now());
    drop(failure);
    assert!(
        result
            .err()
            .unwrap()
            .to_string()
            .contains("cost admission unavailable")
    );
    assert!(board.rows().is_empty());
    assert!(board.activity.active_for(board.candidate.project).is_none());
}

#[test]
fn a_sampling_failure_cancels_owned_work_and_a_later_cycle_recovers_the_row() {
    use crate::store::fault::{FaultAction, FaultPoint, arm};
    let board = Board::new();
    let error = observe(
        &board.store,
        &board.env,
        &board.activity,
        board.candidate.project,
        || {
            let guard = board.admit();
            let failure = arm(
                FaultPoint::BeforeCommit,
                FaultAction::Fail("cost checkpoint unavailable".into()),
            );
            let error = sample(&board.store, &board.activity, board.candidate.project).unwrap_err();
            drop(failure);
            assert!(error.to_string().contains("cost checkpoint unavailable"));
            assert!(guard.is_cancelled());
            assert!(
                check(&guard).is_err(),
                "no disposition after losing required evidence"
            );
            Ok(())
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("cost checkpoint unavailable"));
    assert!(board.rows()[0].finished_at.is_none());
    assert!(board.activity.costs.lock().unwrap().is_empty());
    observe(
        &board.store,
        &board.env,
        &board.activity,
        board.candidate.project,
        || Ok(()),
    )
    .unwrap();
    assert_eq!(board.rows()[0].verdict.as_deref(), Some("interrupted"));
}

#[test]
fn generation_transfer_archives_preparation_and_preserves_repair_and_cleanup_cost() {
    let mut board = Board::new();
    let mut guard = board.admit();
    let path = journal_path(&board.env, &board.candidate);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let preparation = "preparation evidence retained verbatim\n";
    std::fs::write(&path, preparation).unwrap();
    guard
        .reserve(ReservationReason::Reconcile, board.env.now())
        .retire();
    board.age(&guard.active.attempt_id, 900);
    sample(&board.store, &board.activity, board.candidate.project).unwrap();
    let ctx = Ctx::new(
        &board.store,
        board.candidate.project,
        board.env.home().to_path_buf(),
        board.env.clone(),
    )
    .no_hooks(true);
    StoryService::new(&ctx)
        .set_state(&board.candidate.story_id, "in-progress", None, None, None)
        .unwrap();
    StoryService::new(&ctx)
        .set_state(&board.candidate.story_id, "verifying", None, None, None)
        .unwrap();
    board.candidate = VerificationQueue::new(&board.store)
        .next()
        .unwrap()
        .unwrap();
    guard
        .replace(&board.store, &board.env, &board.candidate, board.env.now())
        .unwrap();
    assert!(!path.exists());
    let rows = board.rows();
    assert_eq!(
        std::fs::read_to_string(rows[0].journal_path.as_ref().unwrap()).unwrap(),
        preparation
    );
    assert!(
        rows[0]
            .intervals
            .iter()
            .any(|span| span.phase == "repair-hold" && span.milliseconds.is_some())
    );
    assert_eq!(rows[0].budget_status(), "process-budget-breach");
    assert_ne!(rows[0].submission.generation, rows[1].submission.generation);
    guard
        .reserve(ReservationReason::Remediation, board.env.now())
        .retire();
    guard
        .reserve(ReservationReason::Cleanup, board.env.now())
        .retire();
    board.age(&guard.active.attempt_id, 900);
    drop(guard);
    sample(&board.store, &board.activity, board.candidate.project).unwrap();
    let rows = board.rows();
    assert_eq!(rows[1].budget_status(), "process-budget-breach");
    for phase in ["diagnosis-delivery", "cleanup"] {
        assert!(
            rows[1]
                .intervals
                .iter()
                .any(|span| span.phase == phase && span.milliseconds.is_some())
        );
    }
}

#[test]
fn completed_certification_and_process_budget_failure_are_independent() {
    let board = Board::new();
    observe(
        &board.store,
        &board.env,
        &board.activity,
        board.candidate.project,
        || {
            let guard = board.admit();
            board.age(&guard.active.attempt_id, 900);
            let certified = VerificationOutcome::Certified {
                head: "a".repeat(40),
                tree: "b".repeat(40),
                detail: "fixture complete".into(),
                gate: "fixture gate".into(),
            };
            let outcome = execute(
                &board.store,
                &board.env,
                &guard,
                &board.candidate,
                GateInputs::default(),
                vec![submission(&board.candidate)],
                || certified.clone(),
                |outcome| Ok(Some(outcome.clone())),
            )?;
            assert_eq!(outcome, certified);
            assert!(!guard.is_cancelled());
            Ok(())
        },
    )
    .unwrap();
    let rows = board.rows();
    assert_eq!(rows[0].verdict.as_deref(), Some("certified"));
    assert_eq!(rows[0].budget_status(), "process-budget-breach");
    assert_eq!(
        rows[0].executions[0].inputs.tree.as_deref(),
        Some("b".repeat(40).as_str())
    );
}

#[test]
fn a_retry_queue_interval_does_not_include_prior_admitted_service() {
    let mut board = Board::new();
    board.candidate.verifying_since = Some("2026-01-01T00:00:00Z".into());
    for _ in 0..2 {
        observe(
            &board.store,
            &board.env,
            &board.activity,
            board.candidate.project,
            || {
                let _guard = board.admit();
                Ok(())
            },
        )
        .unwrap();
    }
    let rows = board.rows();
    let queue = rows[1]
        .intervals
        .iter()
        .find(|span| span.phase == "queue")
        .unwrap();
    assert_eq!(
        queue.started_at, rows[0].finished_at,
        "retry wait starts after prior service, not at the original submission"
    );
}
