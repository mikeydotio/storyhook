//! One daemon worker drains durable block effects without holding a database lock.
//!
//! An idle pass is a read. A write transaction is opened only to change a
//! delivery's status, never to find out whether there is one to change: every
//! store fault point fires inside every commit, an empty transaction included,
//! so a housekeeping write on a fresh daemon kills any armed daemon before it
//! accepts its first connection (SH-693) — and, armed or not, holds
//! `BEGIN IMMEDIATE` against every client once per [`IDLE_POLL`] for nothing.
use super::bus::{Change, ChangeBus};
use crate::api::dispatch::{DispatchAgent, resolve_dispatch_script};
use crate::domain::{StoryEvent, SuperState, is_blocked};
use crate::env::Environment;
use crate::env::spawn_env::apply_dispatch_allowlist;
use crate::error::AppError;
use crate::process::{CaptureError, TerminationPolicy, run_captured_quiescent};
use crate::service::workspace_lock::WorkspaceLock;
use crate::service::{Ctx, block_delivery::UNBLOCK_PROMPT};
use crate::store::{
    BlockAction, BlockDelivery, DeliveryStatus, ExpectedSeq, ReadOps, Store, WriteOps,
};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// What an operator can do when a Resume may not have reached its agent.
///
/// Appended to every Unreached or Uncertain Resume, in the stored detail and in
/// the outcome comment, so a lost resume is never only a status nobody reads
/// (SH-772: MT-32 sat idle 37 hours behind one).
const RESUME_REMEDY: &str = "The agent may not know the block was lifted. If a session is \
     still working this story, tell the agent the block was lifted (paste the unblock prompt into \
     its tmux window); if no session is, dispatch the story again.";

/// `delivery` as it is recorded: an undelivered Resume carries [`RESUME_REMEDY`].
fn recorded(delivery: &BlockDelivery) -> BlockDelivery {
    let mut recorded = delivery.clone();
    if recorded.action == BlockAction::Resume
        && matches!(
            recorded.status,
            DeliveryStatus::Unreached | DeliveryStatus::Uncertain
        )
        && !recorded.detail.contains(RESUME_REMEDY)
    {
        recorded.detail = format!("{} {RESUME_REMEDY}", recorded.detail);
    }
    recorded
}

fn finish(
    tx: &mut impl WriteOps,
    ctx: &Ctx<'_, impl Store>,
    delivery: &BlockDelivery,
    expected: DeliveryStatus,
) -> Result<(), AppError> {
    let delivery = &recorded(delivery);
    if !tx.update_block_delivery(delivery, expected)? {
        return Ok(());
    }
    let Some(row) = tx.story(delivery.project, delivery.story)? else {
        return Ok(());
    };
    let prefix = crate::service::project_prefix(tx, delivery.project)?;
    let states = tx.state_map(delivery.project)?;
    crate::service::append_and_fold(
        tx,
        delivery.project,
        delivery.story,
        &prefix,
        &states,
        ExpectedSeq::Exact(row.head_seq),
        &[StoryEvent::StoryCommentAdded {
            at: ctx.now(),
            text: format!(
                "AGENT BLOCK DELIVERY #{} — {} {}

{}",
                delivery.id,
                delivery.action.as_str(),
                delivery.status.as_str(),
                crate::text_lint::quote_evidence(&delivery.detail)
            ),
        }],
        ctx.provenance(),
    )?;
    Ok(())
}

/// Every delivery, across every project, currently in `status`.
///
/// A read, deliberately. The worker's passes are almost always idle, so the
/// question "is there anything to do?" must not cost a write transaction: the
/// callers open one only for the rows this returns, and [`finish`]'s
/// compare-and-swap on the expected status protects that write against a row
/// that moved between the read and the lock.
fn deliveries_in(
    store: &impl Store,
    status: DeliveryStatus,
) -> Result<Vec<BlockDelivery>, AppError> {
    Ok(store.read(|tx| {
        let mut found = Vec::new();
        for project in tx.projects()? {
            found.extend(
                tx.block_deliveries(project.id)?
                    .into_iter()
                    .filter(|d| d.status == status),
            );
        }
        Ok(found)
    })?)
}

/// Report interrupted delivery attempts without replaying external side effects.
///
/// Read first, write only for the deliveries actually interrupted: a fresh
/// daemon with nothing to recover opens no write transaction at all (SH-693).
pub fn recover(store: &impl Store, env: &Environment) -> Result<(), AppError> {
    let mut failures = Vec::new();
    for delivery in deliveries_in(store, DeliveryStatus::Attempting)? {
        if let Err(error) = recover_one(store, env, &delivery) {
            failures.push(format!("delivery #{}: {error}", delivery.id));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(AppError::Storage(format!(
            "block delivery recovery remains pending: {}",
            failures.join("; ")
        )))
    }
}

fn recover_one(
    store: &impl Store,
    env: &Environment,
    delivery: &BlockDelivery,
) -> Result<(), AppError> {
    let (project, checkout) = store.read(|tx| {
        let project = tx
            .project(delivery.project)?
            .ok_or_else(|| AppError::Storage("attempted delivery project disappeared".into()))?;
        let checkout = tx.checkout_path(delivery.project)?.ok_or_else(|| {
            AppError::Storage("attempted delivery lost its linked checkout".into())
        })?;
        Ok((project, checkout))
    })?;
    let id = delivery.story.to_id(&project.prefix);
    let Some(_workspace) = WorkspaceLock::try_acquire(&checkout, &id)? else {
        // An old helper may still hold an inherited descriptor after daemon death.
        return Ok(());
    };
    store.write(|tx| {
        let Some(current) = tx.project(delivery.project)? else { return Ok(()); };
        if current.uuid != project.uuid || current.slug != project.slug || current.prefix != project.prefix
            || tx.checkout_path(delivery.project)?.as_ref() != Some(&checkout) {
            return Ok(());
        }
        let mut recovered = delivery.clone();
        recovered.status = DeliveryStatus::Uncertain;
        recovered.detail = "daemon stopped before acknowledgement; the agent may have been reached; no automatic replay".into();
        let ctx = Ctx::new(store, delivery.project, &checkout, env.clone()).no_hooks(true);
        finish(tx, &ctx, &recovered, DeliveryStatus::Attempting)?;
        Ok(())
    })?;
    Ok(())
}

/// The session a Resume must name, when its own block episode acknowledged one.
///
/// The episode is every row of the story after its previous Resume. Its latest
/// Interrupt counts only when it was Delivered with a target: that exact
/// session is then the only one the Resume may reach (`--expected-target`, the
/// SH-718 binding). Otherwise (unreached, uncertain, superseded, or no
/// Interrupt at all) the Resume goes to the story's current registered session
/// (`--registered-session`), under the same pending authority and workspace
/// lock an Interrupt binds with (SH-772, decision D5). An earlier episode's
/// session names a lifetime that may be long gone, so it never binds.
fn acknowledged_session(history: &[BlockDelivery], resume: &BlockDelivery) -> Option<String> {
    history
        .iter()
        .rev()
        .filter(|earlier| earlier.story == resume.story && earlier.id < resume.id)
        .take_while(|earlier| earlier.action != BlockAction::Resume)
        .find(|earlier| earlier.action == BlockAction::Interrupt)
        .filter(|interrupt| interrupt.status == DeliveryStatus::Delivered)
        .and_then(|interrupt| interrupt.target.clone())
}

/// Deliver one pending effect using the real helper protocol; returns whether work existed.
/// The script parameter permits isolated provider fixtures without changing production flow.
pub fn process_one(
    store: &impl Store,
    env: &Environment,
    script: Option<&Path>,
) -> Result<bool, AppError> {
    // Busy recovery is retried on every pass; another workspace may still progress.
    let recovery = recover(store, env);
    let mut pending = deliveries_in(store, DeliveryStatus::Pending)?;
    pending.sort_by_key(|delivery| delivery.id);
    for delivery in pending {
        // A busy workspace does not own other stories' queue progress.
        if process_candidate(store, env, script, &delivery)? {
            if let Err(error) = recovery {
                eprintln!("storyhook: {error}");
            }
            return Ok(true);
        }
    }
    recovery.map(|()| false)
}

fn process_candidate(
    store: &impl Store,
    env: &Environment,
    script: Option<&Path>,
    observed_delivery: &BlockDelivery,
) -> Result<bool, AppError> {
    let Some((observed_project, observed_checkout)) = store.read(|tx| {
        let Some(project) = tx.project(observed_delivery.project)? else {
            return Ok(None);
        };
        let checkout = tx.checkout_path(project.id)?;
        Ok(Some((project, checkout)))
    })?
    else {
        return Ok(false);
    };
    let observed_id = observed_delivery.story.to_id(&observed_project.prefix);
    // Git discovery and the nonblocking OS lock occur before the SQL writer.
    let (workspace, lock_failure) = if let Some(checkout) = observed_checkout.as_deref() {
        match WorkspaceLock::try_acquire(checkout, &observed_id) {
            Ok(Some(lock)) => (Some(lock), None),
            Ok(None) => return Ok(false),
            Err(error) => (None, Some(error.to_string())),
        }
    } else {
        (None, None)
    };
    let work = store.write(|tx| {
        let history = tx.block_deliveries(observed_delivery.project)?;
        let Some(mut delivery) = history
            .iter()
            .find(|delivery| {
                delivery.id == observed_delivery.id && delivery.status == DeliveryStatus::Pending
            })
            .cloned()
        else {
            return Ok(None);
        };
        let Some(project) = tx.project(delivery.project)? else {
            return Ok(None);
        };
        let checkout = tx.checkout_path(delivery.project)?;
        if project.uuid != observed_project.uuid
            || project.slug != observed_project.slug
            || project.prefix != observed_project.prefix
            || checkout != observed_checkout
        {
            return Ok(None);
        }
        if let Some(error) = lock_failure.as_ref() {
            delivery.status = DeliveryStatus::Unreached;
            delivery.detail =
                format!("no agent reached: workspace exclusion could not be acquired: {error}");
            let ctx = Ctx::new(store, project.id, env.home(), env.clone()).no_hooks(true);
            finish(tx, &ctx, &delivery, DeliveryStatus::Pending)?;
            return Ok(Some((
                delivery,
                project.slug,
                observed_id.clone(),
                checkout,
            )));
        }
        let stories = crate::service::query::story_map(tx, delivery.project)?;
        let id = delivery.story.to_id(&project.prefix);
        let current_episode = !history
            .iter()
            .any(|later| later.story == delivery.story && later.id > delivery.id);
        let applicable = current_episode
            && stories.get(&id).is_some_and(|s| {
                s.superstate == SuperState::Open
                    && match delivery.action {
                        BlockAction::Interrupt => {
                            is_blocked(s, &stories)
                                && matches!(
                                    s.state.as_str(),
                                    "in-progress" | "blocked" | "verifying"
                                )
                        }
                        BlockAction::Resume => s.state == "in-progress" && !is_blocked(s, &stories),
                    }
            });
        if delivery.action == BlockAction::Resume {
            delivery.target = acknowledged_session(&history, &delivery);
        }
        let ctx = Ctx::new(store, project.id, env.home(), env.clone()).no_hooks(true);
        if delivery.action == BlockAction::Resume
            && tx.continuations(delivery.project)?.iter().any(|request| {
                request.story_no == delivery.story
                    && request.handoff["kind"] == "context"
                    && request.status.outstanding()
            })
        {
            delivery.status = DeliveryStatus::Superseded;
            delivery.detail =
                "outstanding context continuation owns resumption; no generic terminal input sent"
                    .into();
            finish(tx, &ctx, &delivery, DeliveryStatus::Pending)?;
            return Ok(Some((delivery, project.slug, id, checkout)));
        }

        if !applicable || checkout.is_none() {
            delivery.status = if !applicable {
                DeliveryStatus::Superseded
            } else {
                DeliveryStatus::Unreached
            };
            delivery.detail = if !applicable {
                "current story state no longer permits this delivery"
            } else {
                "no agent reached: project has no linked checkout"
            }
            .into();
            finish(tx, &ctx, &delivery, DeliveryStatus::Pending)?;
            return Ok(Some((delivery, project.slug, id, checkout)));
        }
        delivery.status = DeliveryStatus::Attempting;
        if !tx.update_block_delivery(&delivery, DeliveryStatus::Pending)? {
            return Ok(None);
        };
        Ok(Some((delivery, project.slug, id, checkout)))
    })?;
    let Some((mut delivery, slug, id, checkout)) = work else {
        return Ok(false);
    };
    if delivery.status != DeliveryStatus::Attempting {
        return Ok(true);
    }
    let resolved;
    let script = match script {
        Some(script) => script,
        None => match resolve_dispatch_script(DispatchAgent::Codex).or_else(|codex| {
            resolve_dispatch_script(DispatchAgent::Claude)
                .map_err(|claude| format!("Codex helper: {codex}; Claude helper: {claude}"))
        }) {
            Ok(path) => {
                resolved = path;
                &resolved
            }
            Err(error) => {
                delivery.status = DeliveryStatus::Unreached;
                delivery.detail = format!("no agent reached: {error}");
                let ctx = Ctx::new(store, delivery.project, env.home(), env.clone()).no_hooks(true);
                store.write(|tx| {
                    finish(tx, &ctx, &delivery, DeliveryStatus::Attempting)?;
                    Ok(())
                })?;
                return Ok(true);
            }
        },
    };
    let story_binary = std::env::current_exe().map_err(|error| {
        AppError::Storage(format!(
            "locating the daemon executable for block delivery: {error}"
        ))
    })?;
    let mut command = Command::new("bash");
    apply_dispatch_allowlist(&mut command);
    command
        .arg(script)
        .args(["--project", &slug, "notify", &id])
        .current_dir(checkout.expect("checked before claiming delivery"))
        .env_remove("STORY_AGENT")
        .envs(env.child_vars())
        // An ambient CLI can replace this daemon while its helper queries the store.
        .env("STORY_BIN", story_binary)
        .stdin(Stdio::null());
    match delivery.action {
        BlockAction::Interrupt => {
            command.arg("--interrupt");
        }
        BlockAction::Resume => {
            command.arg(UNBLOCK_PROMPT);
            match delivery.target.as_deref() {
                Some(target) => command.arg("--expected-target").arg(target),
                None => command.arg("--registered-session"),
            };
        }
    }
    workspace
        .as_ref()
        .expect("a claimed delivery owns its workspace")
        .dispatch_command(&mut command);
    let result = run_captured_quiescent(
        command,
        Duration::from_secs(45),
        TerminationPolicy::TerminateThenKill {
            grace: Duration::from_secs(10),
        },
    );
    // Only a Resume sent to the registered session learns its target from the
    // helper; an acknowledgement that names none proves nothing about who was
    // reached, so it stays Uncertain.
    let names_its_target = delivery.action == BlockAction::Interrupt || delivery.target.is_none();
    delivery.status = DeliveryStatus::Uncertain;
    match result {
        Err(error) => {
            if matches!(error, CaptureError::Unsettled(_)) {
                delivery.status = DeliveryStatus::Attempting;
            }
            delivery.detail = format!("agent delivery was not acknowledged: {}", error.detail())
        }
        Ok(output) => match serde_json::from_slice::<serde_json::Value>(&output.stdout) {
            Ok(answer) => {
                let success = answer.get("ok").and_then(|v| v.as_bool()) == Some(true)
                    && output.status.success();
                let target = answer
                    .get("target")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty());
                if success && (!names_its_target || target.is_some()) {
                    delivery.status = DeliveryStatus::Delivered;
                    if let Some(target) = target {
                        delivery.target = Some(target.into());
                    }
                } else if answer.get("ok").and_then(|v| v.as_bool()) == Some(false)
                    && matches!(
                        answer.get("reason").and_then(|v| v.as_str()),
                        Some(
                            "pane-query-failed"
                                | "pane-unavailable"
                                | "pane-provider-unknown"
                                | "pane-dead"
                                | "pane-changed"
                                | "target-changed"
                                | "composer-busy"
                        )
                    )
                {
                    delivery.status = DeliveryStatus::Unreached;
                }
                delivery.detail = answer
                    .get("display")
                    .and_then(|v| v.as_str())
                    .unwrap_or("helper returned no delivery diagnostic")
                    .into();
                if success && delivery.status == DeliveryStatus::Uncertain {
                    delivery.detail = format!(
                        "the helper acknowledged the resume without naming the session it \
                         reached: {}",
                        delivery.detail
                    );
                }
            }
            Err(error) => {
                delivery.detail = format!(
                    "invalid helper acknowledgement: {error}; stderr: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                )
            }
        },
    }
    let ctx = Ctx::new(store, delivery.project, env.home(), env.clone()).no_hooks(true);
    store.write(|tx| {
        finish(tx, &ctx, &delivery, DeliveryStatus::Attempting)?;
        Ok(())
    })?;
    Ok(true)
}

/// How long an idle pass waits for a change-bus wakeup before scanning again.
///
/// `pub` because `tests/fault_injection.rs` derives its idle window from it
/// (SH-394: a bound is derived from the cadence it is meant to cover, never
/// picked): this is the shortest cadence of every poller the daemon runs, so a
/// window measured in multiples of it covers each poller's start-up pass and
/// its steady state.
pub const IDLE_POLL: Duration = Duration::from_secs(1);

/// Drain ordered intents on committed changes, with fixed recovery and bounded shutdown.
pub(crate) fn poll(store: &impl Store, env: &Environment, bus: &ChangeBus, stop: &AtomicBool) {
    let subscription = bus.subscribe();
    while !stop.load(Ordering::Relaxed) {
        let outcome = process_one(store, env, None);
        match outcome {
            Ok(true) => continue,
            Err(error) => {
                eprintln!("storyhook: block delivery failed: {error}");
            }
            Ok(false) => {}
        }
        let deadline = Instant::now() + IDLE_POLL;
        while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
            if matches!(
                subscription.recv(Duration::from_millis(100)),
                Some(Change::Project(_) | Change::Catalog | Change::Resync)
            ) {
                break;
            }
        }
    }
}
