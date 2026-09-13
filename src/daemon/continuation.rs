//! Continuation monitoring never writes live-provider terminal input.
use super::bus::{Change, ChangeBus};
use crate::env::Environment;
use crate::error::AppError;
use crate::service::{
    Ctx,
    continuation::{ContinuationRuntime, PythonRuntime, require_eligible, save},
};
use crate::store::{Continuation, ContinuationPhase, ContinuationStatus, ReadOps, Store};
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Bounded observation interval for an unacknowledged delivery; late evidence remains admissible.
pub const DELIVERY_TIMEOUT: Duration = Duration::from_secs(45);
fn outstanding(store: &impl Store) -> Result<Vec<Continuation>, AppError> {
    Ok(store.read(|tx| {
        let mut requests = Vec::new();
        for project in tx.projects()? {
            requests.extend(tx.continuations(project.id)?.into_iter().filter(|r| {
                matches!(
                    r.status,
                    ContinuationStatus::Pending
                        | ContinuationStatus::Attempting
                        | ContinuationStatus::AwaitingAck
                )
            }));
        }
        Ok(requests)
    })?)
}
/// Recover the external-effect gap without replaying an uncertain live delivery.
pub fn recover(store: &impl Store, env: &Environment) -> Result<(), AppError> {
    for mut record in outstanding(store)?
        .into_iter()
        .filter(|r| r.status == ContinuationStatus::Attempting)
    {
        record.status = ContinuationStatus::NeedsAttention;
        record.detail="daemon stopped during continuation delivery; no automatic replay; matching receipt or acknowledgement can resolve this uncertainty".into();
        persist(store, env, &mut record)?;
    }
    Ok(())
}
fn persist(
    store: &impl Store,
    env: &Environment,
    record: &mut Continuation,
) -> Result<(), AppError> {
    let now = env.now();
    store.write(|tx| save(tx, record, &now))?;
    Ok(())
}
fn expired(record: &Continuation, now: &str) -> bool {
    match (
        chrono::DateTime::parse_from_rfc3339(&record.updated_at),
        chrono::DateTime::parse_from_rfc3339(now),
    ) {
        (Ok(start), Ok(end)) => {
            end.signed_duration_since(start).num_seconds() >= DELIVERY_TIMEOUT.as_secs() as i64
        }
        _ => true,
    }
}
/// Observe all active handoffs once, resuming only providers positively proven absent.
/// The runtime seam substitutes external observations while retaining production orchestration.
pub fn process_one(
    store: &impl Store,
    env: &Environment,
    runtime: &dyn ContinuationRuntime,
) -> Result<bool, AppError> {
    let requests = outstanding(store)?;
    let mut changed = false;
    for mut record in requests {
        if record.status == ContinuationStatus::Attempting {
            continue;
        }
        let ctx = Ctx::new(store, record.project_id, env.home(), env.clone()).no_hooks(true);
        if let Err(error) = store.read(|tx| require_eligible(tx, &ctx, &record.story_id)) {
            record.status = ContinuationStatus::NeedsAttention;
            record.detail = format!("continuation eligibility changed: {error}; holds preserved");
            persist(store, env, &mut record)?;
            changed = true;
            continue;
        }
        let observed = runtime.call("observe", &serde_json::to_value(&record)?);
        let phase = observed
            .as_ref()
            .ok()
            .filter(|v| v["ok"] == true)
            .and_then(|v| v["phase"].as_str());
        match phase {
            Some("idle" | "busy" | "compacted") => {
                if expired(&record, &ctx.now()) {
                    record.status = ContinuationStatus::NeedsAttention;
                    record.detail="receiving review was not acknowledged before the delivery observation timeout; live session preserved, no input replay".into();
                    persist(store, env, &mut record)?;
                    changed = true;
                }
            }
            Some("absent") => {
                let expected = record.revision;
                record.status = ContinuationStatus::Attempting;
                record.phase = ContinuationPhase::Resume;
                record.attempts += 1;
                store.write(|tx| {
                    require_eligible(tx, &ctx, &record.story_id)?;
                    save(tx, &mut record, &ctx.now())
                })?;
                debug_assert_eq!(record.revision, expected + 1);
                let answer = runtime.call("resume", &serde_json::to_value(&record)?);
                match answer {
                    Ok(answer) if answer["ok"] == true && answer["phase"] == "submitted" => {
                        if let Some(capture) = answer
                            .get("capture")
                            .filter(|c| valid_resumed_capture(&record.capture, c))
                        {
                            record.capture = capture.clone();
                            record.status = ContinuationStatus::AwaitingAck;
                            record.detail="absent provider resumed in its retained worktree; receiving review acknowledgement required".into();
                        } else {
                            record.status = ContinuationStatus::NeedsAttention;
                            record.detail="resume returned no valid generation-bound receiving capture; no replay".into();
                        }
                    }
                    Ok(answer) => {
                        record.status = ContinuationStatus::NeedsAttention;
                        record.detail = format!(
                            "resume was not acknowledged: {}; no replay",
                            answer["detail"]
                        );
                    }
                    Err(error) => {
                        record.status = ContinuationStatus::NeedsAttention;
                        record.detail =
                            format!("resume failed or is ambiguous: {error}; no replay");
                    }
                }
                persist(store, env, &mut record)?;
                changed = true;
            }
            _ => {
                record.status = ContinuationStatus::NeedsAttention;
                record.detail = match observed {
                    Ok(answer) => format!(
                        "provider observation is uncertain: {}; no replay",
                        answer["detail"]
                    ),
                    Err(error) => format!("provider observation failed: {error}; no replay"),
                };
                persist(store, env, &mut record)?;
                changed = true;
            }
        }
    }
    Ok(changed)
}
fn valid_resumed_capture(previous: &Value, next: &Value) -> bool {
    next["lease"] == previous["lease"]
        && next["provider"] == previous["provider"]
        && next["head"] == previous["head"]
        && next["fingerprint"] == previous["fingerprint"]
        && next["session_id"].as_str().is_some_and(|s| !s.is_empty())
        && next["pid"].as_u64().is_some_and(|p| p > 0)
}
/// Event-driven monitor with read-only idle passes and bounded shutdown.
pub(crate) fn poll(store: &impl Store, env: &Environment, bus: &ChangeBus, stop: &AtomicBool) {
    let subscription = bus.subscribe();
    let runtime = PythonRuntime::installed(env.clone());
    if let Err(error) = recover(store, env) {
        eprintln!("storyhook: continuation recovery failed: {error}");
    }
    while !stop.load(Ordering::Relaxed) {
        if let Err(error) = process_one(store, env, &runtime) {
            eprintln!("storyhook: continuation monitoring failed: {error}");
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
