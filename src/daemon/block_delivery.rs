//! One daemon worker drains durable block effects without holding a database lock.
use super::bus::{Change, ChangeBus};
use crate::api::dispatch::{DispatchAgent, resolve_dispatch_script};
use crate::domain::{StoryEvent, SuperState, is_blocked};
use crate::env::Environment;
use crate::env::spawn_env::apply_dispatch_allowlist;
use crate::error::AppError;
use crate::process::{TerminationPolicy, run_captured_with_termination};
use crate::service::{Ctx, block_delivery::UNBLOCK_PROMPT};
use crate::store::{
    BlockAction, BlockDelivery, DeliveryStatus, ExpectedSeq, ReadOps, Store, WriteOps,
};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

fn finish(
    tx: &mut impl WriteOps,
    ctx: &Ctx<'_, impl Store>,
    delivery: &BlockDelivery,
    expected: DeliveryStatus,
) -> Result<(), AppError> {
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
                "AGENT BLOCK DELIVERY #{} — {} {}: {}",
                delivery.id,
                delivery.action.as_str(),
                delivery.status.as_str(),
                delivery.detail
            ),
        }],
        ctx.provenance(),
    )?;
    Ok(())
}

/// Report interrupted delivery attempts without replaying external side effects.
pub fn recover(store: &impl Store, env: &Environment) -> Result<(), AppError> {
    store.write(|tx| {
        for project in tx.projects()? {
            let ctx = Ctx::new(store, project.id, env.home(), env.clone()).no_hooks(true);
            for mut delivery in tx.block_deliveries(project.id)? {
                if delivery.status == DeliveryStatus::Attempting {
                    delivery.status = DeliveryStatus::Uncertain;
                    delivery.detail = "daemon stopped before acknowledgement; the agent may have been reached; no automatic replay".into();
                    finish(tx, &ctx, &delivery, DeliveryStatus::Attempting)?;
                }
            }
        }
        Ok(())
    })?;
    Ok(())
}

/// Deliver one pending effect using the real helper protocol; returns whether work existed.
/// The script parameter permits isolated provider fixtures without changing production flow.
pub fn process_one(
    store: &impl Store,
    env: &Environment,
    script: Option<&Path>,
) -> Result<bool, AppError> {
    let work = store.write(|tx| {
        let mut pending = Vec::new();
        for project in tx.projects()? {
            pending.extend(
                tx.block_deliveries(project.id)?
                    .into_iter()
                    .filter(|d| d.status == DeliveryStatus::Pending),
            );
        }
        pending.sort_by_key(|d| d.id);
        let Some(mut delivery) = pending.into_iter().next() else {
            return Ok(None);
        };
        let project = tx
            .project(delivery.project)?
            .ok_or_else(|| AppError::Storage("delivery project disappeared".into()))?;
        let checkout = tx.checkout_path(delivery.project)?;
        let stories = crate::service::query::story_map(tx, delivery.project)?;
        let id = delivery.story.to_id(&project.prefix);
        let applicable = stories.get(&id).is_some_and(|s| {
            s.superstate == SuperState::Open
                && (delivery.action == BlockAction::Interrupt
                    || s.state == "in-progress" && !is_blocked(s, &stories))
        });
        if delivery.action == BlockAction::Resume {
            delivery.target = tx
                .block_deliveries(delivery.project)?
                .into_iter()
                .rev()
                .find(|d| {
                    d.id < delivery.id
                        && d.story == delivery.story
                        && d.action == BlockAction::Interrupt
                })
                .filter(|d| d.status == DeliveryStatus::Delivered)
                .and_then(|d| d.target);
        }
        let ctx = Ctx::new(store, project.id, env.home(), env.clone()).no_hooks(true);
        if !applicable
            || checkout.is_none()
            || delivery.action == BlockAction::Resume && delivery.target.is_none()
        {
            delivery.status = if !applicable {
                DeliveryStatus::Superseded
            } else {
                DeliveryStatus::Unreached
            };
            delivery.detail = if !applicable {
                "current story state no longer permits this delivery"
            } else if checkout.is_none() {
                "no agent reached: project has no linked checkout"
            } else {
                "no agent reached: no acknowledged interrupted session to resume"
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
    let mut command = Command::new("bash");
    apply_dispatch_allowlist(&mut command);
    command
        .arg(script)
        .args(["--project", &slug, "notify", &id])
        .current_dir(checkout.expect("checked before claiming delivery"))
        .env_remove("STORY_AGENT")
        .envs(env.child_vars())
        .stdin(Stdio::null());
    match delivery.action {
        BlockAction::Interrupt => {
            command.arg("--interrupt");
        }
        BlockAction::Resume => {
            command
                .arg(UNBLOCK_PROMPT)
                .arg("--expected-target")
                .arg(delivery.target.as_deref().expect("resume target checked"));
        }
    }
    let result = run_captured_with_termination(
        command,
        Duration::from_secs(45),
        TerminationPolicy::TerminateThenKill {
            grace: Duration::from_secs(10),
        },
    );
    delivery.status = DeliveryStatus::Uncertain;
    match result {
        Err(error) => {
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
                if success && (delivery.action == BlockAction::Resume || target.is_some()) {
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

/// Drain ordered intents on committed changes, with fixed recovery and bounded shutdown.
pub(crate) fn poll(store: &impl Store, env: &Environment, bus: &ChangeBus, stop: &AtomicBool) {
    let subscription = bus.subscribe();
    let mut needs_recovery = true;
    while !stop.load(Ordering::Relaxed) {
        let outcome = if needs_recovery {
            recover(store, env).and_then(|()| {
                needs_recovery = false;
                process_one(store, env, None)
            })
        } else {
            process_one(store, env, None)
        };
        match outcome {
            Ok(true) => continue,
            Err(error) => {
                eprintln!("storyhook: block delivery failed: {error}");
                needs_recovery = true;
            }
            Ok(false) => {}
        }
        let deadline = Instant::now() + Duration::from_secs(1);
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
