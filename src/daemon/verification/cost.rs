//! Daemon-owned cost observation, independent of progress publication and authority.
//!
//! Lock order: ownership registry, cost traces, Store. The sampler never takes
//! the ownership registry. Budget observations never write the progress journal.
use super::*;
use crate::service::gate_cost;
use crate::store::{
    GateAttempt, GateExecution, GateInputs, GateInterval, GateSubmission, StoreError,
};
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::OpenOptionsExt;
use std::sync::mpsc;

#[cfg(test)]
mod tests;

/// Maximum checkpoint interval; the observer also checks every finalization.
const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(1);

/// In-process clock and the last successfully committed evidence revision.
pub(super) struct Trace {
    record: GateAttempt,
    start: Instant,
    ended: Option<Duration>,
    env: Environment,
    cancellation: Cancellation,
    error: Option<String>,
    phase: Option<(usize, Instant)>,
    breach_published: bool,
}

/// Shared by the owning guard and its project's scoped cost observer.
pub(super) type Traces = Arc<Mutex<BTreeMap<String, Trace>>>;

/// Constructs durable identity in the admission transaction; failure admits nothing.
pub(super) fn admission(
    tx: &mut impl WriteOps,
    env: &Environment,
    candidate: &VerificationCandidate,
    id: &str,
    at: &str,
) -> Result<GateAttempt, StoreError> {
    let mut record = GateAttempt::new(id.into(), submission(candidate), at);
    record.journal_path = Some(journal_path(env, candidate).display().to_string());
    let previous = tx
        .gate_attempts(candidate.project)?
        .into_iter()
        .rev()
        .find(|old| old.submission.story_id == candidate.story_id);
    let queue_start = match &previous {
        Some(old) if old.submission == record.submission => old.finished_at.clone(),
        _ => candidate.verifying_since.clone(),
    };
    record.intervals.push(GateInterval {
        id: "queue".into(),
        path: "admission".into(),
        phase: "queue".into(),
        started_at: queue_start.clone(),
        ended_at: Some(at.into()),
        milliseconds: queue_start
            .as_deref()
            .and_then(|start| gate_cost::utc_milliseconds(start, at)),
        estimated: true,
        started_monotonic_ns: None,
    });
    record.previous_attempt = previous.map(|old| old.id);
    tx.insert_gate_attempt(&record)?;
    Ok(record)
}

/// Submission identity from the queue, never reconstructed from producer claims.
pub(super) fn submission(candidate: &VerificationCandidate) -> GateSubmission {
    GateSubmission {
        project: candidate.project,
        story_id: candidate.story_id.clone(),
        generation: candidate.verifying_generation,
        submitted_at: candidate.verifying_since.clone(),
    }
}

/// Publishes a monotonic origin only after its durable admission exists.
pub(super) fn register(
    traces: &Traces,
    record: GateAttempt,
    start: Instant,
    env: &Environment,
    cancellation: Cancellation,
) {
    let id = record.id.clone();
    traces
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(
            record.id.clone(),
            Trace {
                record,
                start,
                ended: None,
                env: env.clone(),
                cancellation,
                error: None,
                phase: None,
                breach_published: false,
            },
        );
    phase(traces, &id, Some("workspace"));
}

/// Freezes admission duration before the owning guard releases its slot.
pub(super) fn end(traces: &Traces, id: &str) {
    if let Some(trace) = traces
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get_mut(id)
        && trace.ended.is_none()
    {
        change_phase(trace, None);
        trace.ended = Some(trace.start.elapsed());
        trace.record.finished_at = Some(trace.env.now());
    }
}

/// Transitions an admission interval at its actual lifecycle boundary.
pub(super) fn phase(traces: &Traces, id: &str, phase: Option<&str>) {
    if let Some(trace) = traces
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get_mut(id)
    {
        change_phase(trace, phase);
    }
}

fn change_phase(trace: &mut Trace, phase: Option<&str>) {
    let now = trace.env.now();
    if let Some((index, start)) = trace.phase.take() {
        let interval = &mut trace.record.intervals[index];
        interval.ended_at = Some(now.clone());
        interval.milliseconds = u64::try_from(start.elapsed().as_millis()).ok();
    }
    if let Some(phase) = phase {
        let index = trace.record.intervals.len();
        trace.record.intervals.push(GateInterval {
            id: uuid::Uuid::new_v4().to_string(),
            path: "admission".into(),
            phase: phase.into(),
            started_at: Some(now),
            ended_at: None,
            milliseconds: None,
            estimated: false,
            started_monotonic_ns: None,
        });
        trace.phase = Some((index, Instant::now()));
    }
}

/// Refuses disposition after an evidence failure, independently of gate verdict.
pub(super) fn check(owner: &VerificationGuard) -> Result<(), AppError> {
    let traces = owner
        .registry
        .costs
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    match traces.get(&owner.active.attempt_id) {
        Some(trace) => fault(trace),
        None => Err(AppError::Storage(format!(
            "missing admitted gate evidence {}",
            owner.active.attempt_id
        ))),
    }
}

fn fail(trace: &mut Trace, error: AppError) -> AppError {
    let detail = format!(
        "durable gate evidence failed for {}: {error}",
        trace.record.id
    );
    let detail = trace.error.get_or_insert(detail).clone();
    trace.cancellation.cancel();
    AppError::Storage(detail)
}

/// Current physical execution, supplied to the shell as an additional journal binding.
pub(super) fn execution_id(activity: &VerificationActivity, id: &str) -> Option<String> {
    activity
        .costs
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(id)
        .and_then(|trace| trace.record.executions.last())
        .filter(|execution| execution.finished_at.is_none())
        .map(|execution| execution.id.clone())
}

fn milliseconds(duration: Duration) -> Result<u64, AppError> {
    u64::try_from(duration.as_millis())
        .map_err(|_| AppError::Storage("gate cost elapsed overflow".into()))
}

fn fault(trace: &Trace) -> Result<(), AppError> {
    match &trace.error {
        Some(error) => Err(AppError::Storage(error.clone())),
        None => Ok(()),
    }
}

fn flush(store: &impl Store, trace: &mut Trace) -> Result<(), AppError> {
    fault(trace)?;
    let elapsed = trace.ended.unwrap_or_else(|| trace.start.elapsed());
    let now = trace
        .record
        .finished_at
        .clone()
        .unwrap_or_else(|| trace.env.now());
    trace.record.elapsed.observe(milliseconds(elapsed)?, &now);
    read_journals(&mut trace.record)?;
    if trace.ended.is_some()
        && let Some(path) = trace.record.journal_path.as_deref()
    {
        trace.record.journal_path = archive(std::path::Path::new(path), &trace.record.id)?
            .map(|path| path.display().to_string());
    }
    persist(store, &mut trace.record)
}

fn persist(store: &impl Store, record: &mut GateAttempt) -> Result<(), AppError> {
    let expected = record.revision;
    let mut next = record.clone();
    next.revision = expected
        .checked_add(1)
        .ok_or_else(|| AppError::Storage("gate evidence revision overflow".into()))?;
    if !store.write(|tx| tx.update_gate_attempt(&next, expected))? {
        return Err(AppError::Storage(format!(
            "gate cost revision conflict for {}",
            record.id
        )));
    }
    *record = next;
    Ok(())
}

fn read_journals(record: &mut GateAttempt) -> Result<(), AppError> {
    // Only unfinished executions can gain data. Completed raw logs remain available.
    let journals: Vec<_> = record
        .executions
        .iter()
        .filter(|e| e.finished_at.is_none())
        .map(|e| (e.id.clone(), e.journal_path.clone(), e.journal_offset))
        .collect();
    for (id, path, offset) in journals {
        let mut file = match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(AppError::Storage(format!(
                    "reading gate evidence {path}: {error}"
                )));
            }
        };
        let metadata = file
            .metadata()
            .map_err(|e| AppError::Storage(format!("gate evidence metadata {path}: {e}")))?;
        if !metadata.is_file() || metadata.len() < offset {
            return Err(AppError::Storage(format!(
                "gate evidence {path} is not a regular append-only journal for execution {id}"
            )));
        }
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| AppError::Storage(format!("seeking gate evidence {path}: {e}")))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|e| AppError::Storage(format!("reading gate evidence {path}: {e}")))?;
        // A concurrently written UTF-8 suffix is not malformed complete evidence.
        if let Some(last) = bytes.iter().rposition(|byte| *byte == b'\n') {
            let text = std::str::from_utf8(&bytes[..=last]).map_err(|e| {
                AppError::Storage(format!("non-UTF-8 complete gate evidence {path}: {e}"))
            })?;
            gate_cost::journal::ingest(record, &id, text);
        }
    }
    Ok(())
}

pub(super) fn sample(
    store: &impl Store,
    activity: &VerificationActivity,
    project: ProjectId,
) -> Result<(), AppError> {
    let mut traces = activity
        .costs
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let mut breached = false;
    for trace in traces
        .values_mut()
        .filter(|t| t.record.submission.project == project)
    {
        if let Err(error) = flush(store, trace) {
            return Err(fail(trace, error));
        }
        if trace.record.elapsed.breached_at.is_some() && !trace.breach_published {
            trace.breach_published = true;
            breached = true;
        }
    }
    traces.retain(|_, t| t.record.submission.project != project || t.ended.is_none());
    drop(traces);
    if breached {
        activity.publish_project(store, project)?;
    }
    Ok(())
}

struct Finish(Option<mpsc::Sender<()>>);
impl Drop for Finish {
    fn drop(&mut self) {
        self.0.take();
    }
}

/// Observes an entire verifier cycle, including admission, waits and cleanup.
pub(super) fn observe<T>(
    store: &impl Store,
    env: &Environment,
    activity: &VerificationActivity,
    project: ProjectId,
    run: impl FnOnce() -> Result<T, AppError>,
) -> Result<T, AppError> {
    if !activity
        .costs
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .values()
        .any(|trace| trace.record.submission.project == project)
    {
        restart(store, project, &env.now())?;
    }
    sample(store, activity, project)?;
    let (finish, receiver) = mpsc::channel::<()>();
    std::thread::scope(|scope| {
        let finish = Finish(Some(finish));
        let observer = scope.spawn(move || {
            loop {
                let ended = !matches!(
                    receiver.recv_timeout(CHECKPOINT_INTERVAL),
                    Err(mpsc::RecvTimeoutError::Timeout)
                );
                sample(store, activity, project)?;
                if ended {
                    return Ok::<_, AppError>(());
                }
            }
        });
        let result = run();
        drop(finish);
        let observed = observer
            .join()
            .map_err(|_| AppError::Storage("gate cost observer panicked".into()))?;
        // A failed final write stays unfinished in Store for restart accounting.
        // Its dead in-memory owner must not poison every later recovery cycle.
        activity
            .costs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|_, trace| trace.record.submission.project != project || trace.ended.is_none());
        observed?;
        result
    })
}

/// Persists an execution before it starts and its result before disposition.
/// The wrapper never decides certification and never cancels because of elapsed time.
#[allow(clippy::too_many_arguments)]
pub(super) fn execute<T>(
    store: &impl Store,
    env: &Environment,
    owner: &VerificationGuard,
    candidate: &VerificationCandidate,
    inputs: GateInputs,
    submissions: Vec<GateSubmission>,
    run: impl FnOnce() -> T,
    result: impl FnOnce(&T) -> Result<Option<VerificationOutcome>, String>,
) -> Result<T, AppError> {
    match execute_inner(
        store,
        env,
        owner,
        candidate,
        inputs,
        submissions,
        run,
        result,
    ) {
        Ok(answer) => Ok(answer),
        Err(error) => {
            let mut traces = owner
                .registry
                .costs
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            match traces.get_mut(&owner.active.attempt_id) {
                Some(trace) => Err(fail(trace, error)),
                None => Err(error),
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_inner<T>(
    store: &impl Store,
    env: &Environment,
    owner: &VerificationGuard,
    candidate: &VerificationCandidate,
    inputs: GateInputs,
    submissions: Vec<GateSubmission>,
    run: impl FnOnce() -> T,
    result: impl FnOnce(&T) -> Result<Option<VerificationOutcome>, String>,
) -> Result<T, AppError> {
    let id = uuid::Uuid::new_v4().to_string();
    let journal = journal_path(env, candidate);
    let origin = Instant::now();
    {
        let mut traces = owner
            .registry
            .costs
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let trace = traces.get_mut(&owner.active.attempt_id).ok_or_else(|| {
            AppError::Storage(format!(
                "missing admitted gate evidence {}",
                owner.active.attempt_id
            ))
        })?;
        fault(trace)?;
        if trace
            .record
            .executions
            .last()
            .is_some_and(|e| e.finished_at.is_none())
        {
            return Err(AppError::Storage(format!(
                "gate evidence {} already has an unfinished execution",
                trace.record.id
            )));
        }
        change_phase(trace, None);
        let mut execution =
            GateExecution::new(id.clone(), &env.now(), journal.display().to_string());
        execution.inputs = inputs;
        execution.submissions = submissions;
        if let Some(prelude) = archive(&journal, &format!("{id}-prelude"))? {
            execution.logs.push(prelude.display().to_string());
        }
        trace.record.executions.push(execution);
        persist(store, &mut trace.record).map_err(|error| fail(trace, error))?;
        if let Some(parent) = journal.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(
            &journal,
            format!(
                "{}\n",
                serde_json::json!({
                    "kind":"run", "attempt_id":owner.active.attempt_id, "execution_id":id,
                    "generation":candidate.verifying_generation.map(|g| g.get()), "at":env.now(),
                })
            ),
        )?;
    }
    let answer = run();
    let outcome = result(&answer);
    let observed = match &outcome {
        Ok(outcome) => {
            crate::domain::gate_verdict::GateVerdict::of(&Ok(outcome.clone()), owner.is_cancelled())
        }
        Err(_) => crate::domain::gate_verdict::GateVerdict::Error,
    }
    .as_str()
    .to_string();
    let mut traces = owner
        .registry
        .costs
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let trace = traces.get_mut(&owner.active.attempt_id).ok_or_else(|| {
        AppError::Storage("admission evidence disappeared during gate execution".into())
    })?;
    fault(trace)?;
    read_journals(&mut trace.record)?;
    let execution = trace
        .record
        .executions
        .iter_mut()
        .find(|e| e.id == id)
        .expect("execution inserted above");
    if let Some(path) = archive(&journal, &id)? {
        execution.journal_path = path.display().to_string();
        execution.logs.push(execution.journal_path.clone());
    }
    execution.milliseconds = Some(milliseconds(origin.elapsed())?);
    execution.finished_at = Some(env.now());
    execution.verdict = Some(observed.clone());
    trace.record.verdict = Some(observed);
    if let Err(detail) = &outcome {
        execution
            .diagnostics
            .push(format!("observing gate result: {detail}"));
    }
    if let Ok(Some(outcome)) = outcome {
        match outcome {
            VerificationOutcome::Certified {
                head, tree, gate, ..
            } => {
                if execution.inputs.head.is_none() {
                    execution.inputs.head = Some(head);
                }
                if execution.inputs.tree.is_none() {
                    execution.inputs.tree = Some(tree);
                }
                if execution.inputs.contract.is_none() {
                    execution.inputs.contract = Some(serde_json::json!({"command":gate}));
                }
            }
            VerificationOutcome::TestsFailed { tree, log, .. } => {
                if execution.inputs.tree.is_none() {
                    execution.inputs.tree = Some(tree);
                }
                if !execution.logs.contains(&log) {
                    execution.logs.push(log);
                }
            }
            _ => {}
        }
    }
    change_phase(trace, Some("verdict"));
    flush(store, trace).map_err(|error| fail(trace, error))?;
    Ok(answer)
}

fn archive(path: &std::path::Path, id: &str) -> Result<Option<PathBuf>, AppError> {
    let mut source = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(AppError::Storage(format!(
                "opening gate archive source {}: {error}",
                path.display()
            )));
        }
    };
    let metadata = source.metadata()?;
    if !metadata.is_file() {
        return Err(AppError::Storage(format!(
            "gate archive source {} is not a regular file",
            path.display()
        )));
    }
    let destination = path.with_file_name(format!("cost-{id}-{}.ndjson", uuid::Uuid::new_v4()));
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&destination)
        .map_err(|e| {
            AppError::Storage(format!(
                "creating gate archive {}: {e}",
                destination.display()
            ))
        })?;
    let copied = std::io::copy(&mut (&mut source).take(metadata.len()), &mut output)?;
    if copied != metadata.len() {
        return Err(AppError::Storage(format!(
            "gate archive source {} shrank during capture",
            path.display()
        )));
    }
    output.sync_all()?;
    if let Some(parent) = destination.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(Some(destination))
}

/// Settles abandoned observations on restart without granting execution ownership.
pub(super) fn restart(store: &impl Store, project: ProjectId, at: &str) -> Result<(), AppError> {
    let pending = store.read(|tx| {
        Ok(tx
            .gate_attempts(project)?
            .into_iter()
            .filter(|a| a.finished_at.is_none())
            .collect::<Vec<_>>())
    })?;
    for mut record in pending {
        read_journals(&mut record)?;
        for execution in record
            .executions
            .iter_mut()
            .filter(|e| e.finished_at.is_none())
        {
            if let Some(path) =
                archive(std::path::Path::new(&execution.journal_path), &execution.id)?
            {
                execution.journal_path = path.display().to_string();
                execution.logs.push(execution.journal_path.clone());
            }
        }
        if let Some(path) = record.journal_path.as_deref() {
            record.journal_path = archive(std::path::Path::new(path), &record.id)?
                .map(|path| path.display().to_string());
        }
        record.elapsed.restart(at);
        record.finished_at = Some(at.into());
        record.verdict = Some("interrupted".into());
        record.diagnostics.push(
            "daemon restarted; elapsed gap estimated, physical execution completion unknown".into(),
        );
        for execution in record
            .executions
            .iter_mut()
            .filter(|e| e.finished_at.is_none())
        {
            execution.estimated = true;
            execution.verdict = Some("interrupted".into());
            // Missing terminal measurement stays unknown, not a guessed end of work.
        }
        persist(store, &mut record)?;
    }
    Ok(())
}
