//! Drives every reserved story reset to completion on the daemon's own store.
//!
//! A reset is durable intent (SH-886). Requests from the dashboard and the CLI
//! only reserve it; this runtime executes it on the daemon's store, so its
//! writes queue on the store's in-process mutex instead of competing through
//! SQLite's busy timeout, and it resumes every unfinished reset at startup and
//! on a periodic sweep, so no reset waits for a person to retry it.
use crate::api::dispatch::DispatchRegistry;
use crate::daemon::bus::{Change, ChangeBus};
use crate::daemon::lifecycle::{CurrentRequest, InFlight};
use crate::daemon::verification::VerificationActivity;
use crate::env::Environment;
use crate::error::AppError;
use crate::service::Ctx;
use crate::service::story_reset::StoryResetService;
use crate::store::patience::Shutdown;
use crate::store::{ProjectId, ReadOps, Store, StoryNo, StoryReset};
use std::collections::{BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// How often the runtime looks for unfinished resets to resume.
pub(crate) const SWEEP_INTERVAL: Duration = Duration::from_secs(30);

/// How long the runtime loop sleeps between checks for new work and `stop`.
const POLL: Duration = Duration::from_secs(1);

/// Resets driven at once. More wait their turn; none is refused.
const MAX_WORKERS: usize = 4;

/// One reset the runtime drives.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Job {
    project: ProjectId,
    story: StoryNo,
    story_id: String,
    token: String,
    automatic: bool,
}

#[derive(Debug, Default)]
struct Queue {
    waiting: VecDeque<Job>,
    /// Tokens waiting or running, so no reset is driven twice at once.
    active: BTreeSet<String>,
    running: usize,
}

/// The daemon's reset runtime: a queue of reserved resets and the signal
/// that stops patient waits when the daemon stands down.
#[derive(Debug, Default)]
pub struct ResetRuntime {
    queue: Mutex<Queue>,
    wake: Condvar,
    shutdown: Shutdown,
}

impl ResetRuntime {
    /// An empty runtime.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn queue(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues `reset` to be driven to completion; a reset already waiting or
    /// running is not queued twice.
    pub fn request(&self, reset: &StoryReset) {
        self.enqueue(reset, false);
    }

    fn enqueue(&self, reset: &StoryReset, automatic: bool) {
        if reset.completed {
            return;
        }
        let mut queue = self.queue();
        if queue.active.insert(reset.token.clone()) {
            queue.waiting.push_back(Job {
                project: reset.project,
                story: reset.story,
                story_id: reset.story_id.clone(),
                token: reset.token.clone(),
                automatic,
            });
            self.wake.notify_all();
        }
    }

    /// Whether the runtime is driving, or about to drive, `token`.
    #[must_use]
    pub fn is_active(&self, token: &str) -> bool {
        self.queue().active.contains(token)
    }

    /// Stops every patient wait; the next daemon resumes the unfinished work.
    pub fn shutdown(&self) {
        self.shutdown.request();
        self.wake.notify_all();
    }

    /// The next job to start, when a worker slot is free.
    fn next(&self) -> Option<Job> {
        let mut queue = self.queue();
        if queue.running >= MAX_WORKERS {
            return None;
        }
        let job = queue.waiting.pop_front()?;
        queue.running += 1;
        Some(job)
    }

    /// Waits for new work, a finished worker, or the poll interval.
    fn idle(&self) {
        let queue = self.queue();
        drop(
            self.wake
                .wait_timeout(queue, POLL)
                .unwrap_or_else(PoisonError::into_inner),
        );
    }
}

/// Releases a job's slot however its worker ends, panics included, so the
/// next sweep can resume it.
struct Slot<'a> {
    runtime: &'a ResetRuntime,
    token: String,
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        let mut queue = self.runtime.queue();
        queue.active.remove(&self.token);
        queue.running = queue.running.saturating_sub(1);
        self.runtime.wake.notify_all();
    }
}

/// Everything a worker needs from the daemon.
pub(crate) struct Daemon<'a, S: Store> {
    /// The daemon's own store.
    pub(crate) store: &'a S,
    /// The daemon's environment.
    pub(crate) env: &'a Environment,
    /// Announces finished resets to dashboards.
    pub(crate) bus: &'a ChangeBus,
    /// HTTP dispatches a reset waits for.
    pub(crate) dispatch: &'a DispatchRegistry,
    /// The verifier a reset cancels for its story.
    pub(crate) activity: &'a VerificationActivity,
    /// Keeps a draining daemon alive while a reset attempt runs.
    pub(crate) inflight: &'a InFlight,
}

/// Runs the runtime until `stop`: resumes unfinished resets at once and on
/// every sweep, and drives queued resets on scoped worker threads.
pub(crate) fn run<'scope, 'env, S: Store>(
    scope: &'scope std::thread::Scope<'scope, 'env>,
    daemon: &'env Daemon<'env, S>,
    runtime: &'env ResetRuntime,
    stop: &'env AtomicBool,
) {
    let mut swept: Option<Instant> = None;
    while !stop.load(Ordering::Relaxed) && !runtime.shutdown.requested() {
        if swept.is_none_or(|at| at.elapsed() >= SWEEP_INTERVAL) {
            adopt_legacy(daemon, runtime);
            resume_unfinished(daemon.store, runtime);
            swept = Some(Instant::now());
        }
        while let Some(job) = runtime.next() {
            scope.spawn(move || {
                crate::daemon::qos::WorkClass::Housekeeping.enter();
                drive(daemon, runtime, job);
            });
        }
        runtime.idle();
    }
}

/// Adopts each reservation `story reset` left before this upgrade, so that
/// request also finishes without anyone retrying it (council C1).
fn adopt_legacy<S: Store>(daemon: &Daemon<'_, S>, runtime: &ResetRuntime) {
    let legacy = daemon.store.read(|tx| {
        let mut found = Vec::new();
        for project in tx.projects()? {
            if !tx.automations_enabled(project.id)? {
                continue;
            }
            for (story, encoded) in tx.story_resets(project.id)? {
                found.push((project.id, story.to_id(&project.prefix), encoded));
            }
        }
        Ok(found)
    });
    let legacy = match legacy {
        Ok(legacy) => legacy,
        Err(error) => {
            return crate::daemon::activity::emit(
                "ERROR",
                "reset",
                "event",
                "runtime",
                &format!("listing pre-upgrade story resets to adopt: {error}"),
            );
        }
    };
    for (project, id, encoded) in legacy {
        let Ok(Some(_automation)) =
            crate::service::automations::enter(daemon.store, daemon.env, project)
        else {
            continue;
        };
        if daemon
            .store
            .read(|tx| Ok(tx.settings(project)?.automations_after.is_some()))
            .unwrap_or(true)
        {
            continue;
        }
        // A record that cannot say it was forced is treated as not forced.
        let force = serde_json::from_str::<crate::service::reset::ResetReservation>(&encoded)
            .is_ok_and(|reservation| reservation.force);
        let ctx = Ctx::new(
            daemon.store,
            project,
            daemon.env.home().to_path_buf(),
            daemon.env.clone(),
        )
        .no_hooks(true);
        match StoryResetService::new(&ctx).adopt_legacy(&id, force) {
            Ok(reset) => runtime.enqueue(&reset, true),
            Err(error) => crate::daemon::activity::emit(
                "ERROR",
                "reset",
                "event",
                &id,
                &format!("adopting its pre-upgrade story reset: {error}"),
            ),
        }
    }
}

/// Queues every unfinished reset the store holds.
fn resume_unfinished<S: Store>(store: &S, runtime: &ResetRuntime) {
    match store.read(|tx| {
        let mut resets = Vec::new();
        for reset in tx.unfinished_story_resets()? {
            if tx.automations_enabled(reset.project)?
                && reset.origin.automation_generation
                    == tx.settings(reset.project)?.automations_after
            {
                resets.push(reset);
            }
        }
        Ok(resets)
    }) {
        Ok(resets) => resets.iter().for_each(|reset| runtime.enqueue(reset, true)),
        Err(error) => crate::daemon::activity::emit(
            "ERROR",
            "reset",
            "event",
            "runtime",
            &format!("listing unfinished resets to resume: {error}"),
        ),
    }
}

/// Drives one reset; whatever the outcome, its slot is released and an
/// unfinished reset is left for the next sweep.
fn drive<S: Store>(daemon: &Daemon<'_, S>, runtime: &ResetRuntime, job: Job) {
    let slot = Slot {
        runtime,
        token: job.token.clone(),
    };
    let _automation = if job.automatic {
        match crate::service::automations::enter(daemon.store, daemon.env, job.project) {
            Ok(Some(permit)) => Some(permit),
            _ => return,
        }
    } else {
        None
    };
    let outcome = guarded(|| attempt(daemon, runtime, &job));
    if let Err(error) = outcome {
        crate::daemon::activity::emit(
            "ERROR",
            "reset",
            "event",
            &job.story_id,
            &format!("reset {} will be resumed: {error}", job.token),
        );
    }
    drop(slot);
    if let Ok(Some(project)) = daemon.store.read(|tx| tx.project(job.project)) {
        daemon.bus.publish(Change::Project(project.slug));
    }
}

/// Runs one attempt, turning a panic into an error that names it.
fn guarded(attempt: impl FnOnce() -> Result<StoryReset, AppError>) -> Result<StoryReset, AppError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(attempt)).unwrap_or_else(|panic| {
        let detail = panic
            .downcast_ref::<&str>()
            .map(|text| (*text).to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "a non-text panic".into());
        Err(AppError::Storage(format!(
            "reset worker panicked: {detail}"
        )))
    })
}

fn attempt<S: Store>(
    daemon: &Daemon<'_, S>,
    runtime: &ResetRuntime,
    job: &Job,
) -> Result<StoryReset, AppError> {
    let entry = daemon.inflight.enter();
    entry.name(CurrentRequest {
        request_id: job.token.clone(),
        command: "story-reset".into(),
        project: None,
        pid: std::process::id(),
        started_at: daemon.env.now(),
        served_deadline_secs: 600,
        cwd: daemon.env.home().to_path_buf(),
    });
    let receipt = daemon
        .store
        .read(|tx| tx.story_reset(job.project, job.story))?
        .filter(|receipt| receipt.token == job.token)
        .ok_or_else(|| AppError::NotFound(format!("reset {} for {}", job.token, job.story_id)))?;
    // The request's own directory and hook policy apply, however late it runs.
    let origin = &receipt.origin;
    let ctx = Ctx::new(
        daemon.store,
        job.project,
        origin
            .cwd
            .clone()
            .unwrap_or_else(|| daemon.env.home().to_path_buf()),
        daemon.env.clone(),
    )
    .no_hooks(!origin.fire_hooks)
    .hook_depth(origin.hook_depth);
    let service = StoryResetService::new(&ctx).with_shutdown(runtime.shutdown.clone());
    service.execute(&job.story_id, &job.token, || {
        quiesce(&ctx, daemon, &receipt)
    })
}

/// Waits for a dispatch already under way and cancels the story's verifier,
/// within the dispatch deadline; the reset proceeds either way.
fn quiesce<S: Store>(
    ctx: &Ctx<'_, S>,
    daemon: &Daemon<'_, S>,
    reset: &StoryReset,
) -> Result<(), AppError> {
    let deadline = Instant::now() + crate::service::engine::DISPATCH_TIMEOUT;
    loop {
        let engine_dispatching = crate::service::engine::card_reset_dispatching(ctx, reset)?;
        if daemon.dispatch.running_handle(&reset.story_id).is_none() && !engine_dispatching {
            break;
        }
        if Instant::now() >= deadline {
            return Err(AppError::Validation(
                "an existing dispatch did not finish within the dispatch deadline".into(),
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    daemon
        .activity
        .cancel_story_and_wait(reset.project, &reset.story_id, deadline)
}

#[cfg(test)]
mod tests;
