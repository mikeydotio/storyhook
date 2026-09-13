//! Generation-bound context transfers; external operations never hold a store lock.
use super::{Ctx, QueryService, project_prefix, resolve_story};
use crate::domain::StoryCleanupLease;
use crate::error::AppError;
use crate::store::{
    Continuation, ContinuationPhase, ContinuationStatus, ReadOps, Store, StoreError, WriteOps,
};
use serde_json::{Value, json};
mod json;
mod runtime;
pub use json::parse_document;
pub use runtime::PythonRuntime;
mod administrative;
mod request;

/// Provider observations and delivery effects, replaceable only at the process boundary.
pub trait ContinuationRuntime {
    /// Execute one bounded provider operation against a complete persisted record.
    fn call(&self, operation: &str, input: &Value) -> Result<Value, AppError>;
}
/// A continuation service bound to a selected project and runtime adapter.
pub struct ContinuationService<'a, S: Store> {
    ctx: &'a Ctx<'a, S>,
    runtime: &'a dyn ContinuationRuntime,
}
impl<'a, S: Store> ContinuationService<'a, S> {
    /// Bind the service without observing or changing provider state.
    pub fn new(ctx: &'a Ctx<'a, S>, runtime: &'a dyn ContinuationRuntime) -> Self {
        Self { ctx, runtime }
    }
    /// Read current request evidence and the exact story sequence for a receiving review.
    pub fn status(&self, id: &str) -> Result<Value, AppError> {
        self.ctx.store().read(|tx|{
            let prefix=project_prefix(tx,self.ctx.project())?;
            let (_,row)=resolve_story(tx,self.ctx.project(),&prefix,id)?;
            let requests:Vec<_>=tx.continuations(self.ctx.project())?.into_iter().filter(|r|r.story_id==id).collect();
            Ok(json!({"result":"ok","story_id":id,"snapshot_seq":row.head_global_seq.get(),"requests":requests}))
        }).map_err(Into::into)
    }
    /// Record only a native receipt bound to this provider session and compaction attempt.
    pub fn receipt(
        &self,
        id: &str,
        request: &str,
        input: &Value,
    ) -> Result<Continuation, AppError> {
        if input["event"] != "post-compact" || input["origin"]["session_id"] != input["session_id"]
        {
            return Err(AppError::Validation(
                "invalid native compaction receipt".into(),
            ));
        }
        let previous = self
            .ctx
            .store()
            .read(|tx| find(tx, self.ctx, id, request))?;
        let mut observation = serde_json::to_value(&previous)?;
        observation["receipt"] = input.clone();
        let observed = self.runtime.call("observe", &observation)?;
        if observed["ok"] != true
            || !matches!(
                observed["phase"].as_str(),
                Some("compacted" | "idle" | "busy")
            )
        {
            return Err(AppError::Validation(
                "native compaction receipt could not be validated against the current session"
                    .into(),
            ));
        }
        self.ctx.store().write(|tx|{
            let mut record=find(tx,self.ctx,id,request)?;
            if record.capture["provider"]!=input["provider"]||record.capture["session_id"]!=input["session_id"]{return Err(StoreError::Validation("compaction receipt belongs to another provider session".into()));}
            if record.capture.get("compaction_receipt")==Some(input){return Ok(record);}
            if !record.status.outstanding(){return Err(StoreError::Validation("no outstanding continuation for this compaction receipt".into()));}
            record.detail="native compaction receipt recorded; receiving review acknowledgement remains required".into();
            record.capture["compaction_receipt"]=input.clone();
            save(tx,&mut record,&self.ctx.now())?;
            Ok(record)
        }).map_err(Into::into)
    }
    /// Acknowledge the receiving root's explicit review evidence after current runtime validation.
    pub fn ack(
        &self,
        id: &str,
        request: &str,
        reviewed_seq: i64,
        head: &str,
        provider: &str,
        session_id: &str,
    ) -> Result<Continuation, AppError> {
        let record = self
            .ctx
            .store()
            .read(|tx| find(tx, self.ctx, id, request))?;
        let observation = self
            .runtime
            .call("observe", &serde_json::to_value(&record)?)?;
        if observation["ok"] != true
            || !matches!(observation["phase"].as_str(), Some("idle" | "busy"))
        {
            return Err(AppError::Validation(format!(
                "cannot validate receiving session: {}",
                observation["detail"]
            )));
        }
        if let Some(fresh) = observation.get("capture") {
            if fresh["session_id"] != session_id
                || fresh["provider"] != provider
                || fresh["head"] != head
            {
                return Err(AppError::Validation(
                    "receiving identity or reviewed HEAD differs from current runtime".into(),
                ));
            }
        } else {
            return Err(AppError::Validation(
                "receiving acknowledgement requires fresh runtime capture".into(),
            ));
        }
        self.ctx
            .store()
            .write(|tx| {
                let mut latest = find(tx, self.ctx, id, request)?;
                require_eligible(tx, self.ctx, id)?;
                if latest.capture["provider"] != provider
                    || latest.capture["session_id"] != session_id
                {
                    return Err(StoreError::Validation(
                        "acknowledgement belongs to another receiving session".into(),
                    ));
                }
                let prefix = project_prefix(tx, self.ctx.project())?;
                let (_, row) = resolve_story(tx, self.ctx.project(), &prefix, id)?;
                if row.head_global_seq.get() != reviewed_seq {
                    return Err(StoreError::Validation(
                        "story changed after review; reread current comments and relationships"
                            .into(),
                    ));
                }
                if latest.status == ContinuationStatus::Acknowledged
                    && latest.reviewed_seq == Some(reviewed_seq)
                    && latest.reviewed_head.as_deref() == Some(head)
                {
                    return Ok(latest);
                }
                if latest.phase != ContinuationPhase::NativeContinuation
                    && latest.phase != ContinuationPhase::Resume
                    && latest.phase != ContinuationPhase::Complete
                {
                    return Err(StoreError::Validation(
                        "continuation prompt has not reached a receiving session".into(),
                    ));
                }
                if !matches!(
                    latest.status,
                    ContinuationStatus::AwaitingAck
                        | ContinuationStatus::Attempting
                        | ContinuationStatus::NeedsAttention
                        | ContinuationStatus::Acknowledged
                ) {
                    return Err(StoreError::Validation(
                        "request is not awaiting receiving acknowledgement".into(),
                    ));
                }
                latest.status = ContinuationStatus::Acknowledged;
                latest.phase = ContinuationPhase::Complete;
                latest.reviewed_seq = Some(reviewed_seq);
                latest.reviewed_head = Some(head.into());
                latest.detail =
                    "receiving root acknowledged current story review and Git HEAD".into();
                save(tx, &mut latest, &self.ctx.now())?;
                Ok(latest)
            })
            .map_err(Into::into)
    }
    /// Retry only when fresh observation proves that no prior delivery remains ambiguous.
    pub fn retry(&self, id: &str, request: &str) -> Result<Continuation, AppError> {
        let record = self
            .ctx
            .store()
            .read(|tx| find(tx, self.ctx, id, request))?;
        let observed = self
            .runtime
            .call("observe", &serde_json::to_value(&record)?)?;
        if observed["ok"] != true || !matches!(observed["phase"].as_str(), Some("idle" | "absent"))
        {
            return Err(AppError::Validation(
                "continuation retry requires proven idle or absent ownership".into(),
            ));
        }
        if record.phase != ContinuationPhase::Observe && observed["phase"] != "absent" {
            return Err(AppError::Validation(
                "prior delivery remains ambiguous; await its matching receipt or acknowledgement"
                    .into(),
            ));
        }
        self.ctx
            .store()
            .write(|tx| {
                let mut latest = find(tx, self.ctx, id, request)?;
                require_eligible(tx, self.ctx, id)?;
                if latest.revision != record.revision
                    || latest.status != ContinuationStatus::NeedsAttention
                {
                    return Err(StoreError::Validation(
                        "continuation changed or does not need recovery".into(),
                    ));
                }
                latest.status = ContinuationStatus::Pending;
                latest.phase = ContinuationPhase::Observe;
                latest.detail =
                    "explicit retry admitted after fresh ownership and eligibility checks".into();
                save(tx, &mut latest, &self.ctx.now())?;
                Ok(latest)
            })
            .map_err(Into::into)
    }
}
fn validate_request(id: &str, input: &Value) -> Result<(), AppError> {
    let obj = input
        .as_object()
        .ok_or_else(|| AppError::Validation("handoff request must be an object".into()))?;
    if obj
        .keys()
        .any(|k| !matches!(k.as_str(), "handoff" | "origin" | "provider"))
        || !matches!(input["provider"].as_str(), Some("codex" | "claude"))
        || !input["origin"].is_object()
    {
        return Err(AppError::Validation(
            "invalid continuation request wrapper".into(),
        ));
    }
    let h = &input["handoff"];
    let valid = h.as_object().is_some_and(|o| {
        o.len() == 5
            && o.keys().all(|k| {
                matches!(
                    k.as_str(),
                    "type" | "version" | "story_id" | "kind" | "evidence"
                )
            })
    });
    if !valid
        || h["type"] != "storyhook.session-handoff"
        || h["version"].as_u64() != Some(1)
        || h["story_id"] != id
        || !matches!(h["kind"].as_str(), Some("context" | "obviation-review"))
        || !h["evidence"].is_object()
    {
        return Err(AppError::Validation(
            "invalid or foreign storyhook.session-handoff v1 envelope".into(),
        ));
    }
    for key in if h["kind"] == "context" {
        vec!["context", "outstanding_work"]
    } else {
        vec!["context", "original_state"]
    } {
        if h["evidence"][key]
            .as_str()
            .is_none_or(|v| v.trim().is_empty())
        {
            return Err(AppError::Validation(format!(
                "handoff evidence requires nonempty {key}"
            )));
        }
    }
    Ok(())
}
fn validate_capture(id: &str, provider: &Value, capture: &Value) -> Result<(), AppError> {
    let lease: StoryCleanupLease = serde_json::from_value(capture["lease"].clone())
        .map_err(|e| AppError::Validation(format!("continuation cleanup lease: {e}")))?;
    if lease.version != 1
        || lease.story_id != id
        || !lease.worktree_path.is_absolute()
        || !lease.repository_path.is_absolute()
        || !lease.tmux.socket_path.is_absolute()
        || lease.branch.is_empty()
        || capture["provider"] != *provider
        || capture["autonomy"] != true
    {
        return Err(AppError::Validation(
            "invalid autonomous continuation ownership capture".into(),
        ));
    }
    for key in [
        "head",
        "fingerprint",
        "session_id",
        "turn_id",
        "mode",
        "socket",
        "pane",
        "started",
    ] {
        if capture[key].as_str().is_none_or(|v| v.is_empty()) {
            return Err(AppError::Validation(format!(
                "runtime capture requires {key}"
            )));
        }
    }
    if capture["pid"].as_u64().is_none_or(|pid| pid == 0) {
        return Err(AppError::Validation(
            "runtime capture requires provider PID".into(),
        ));
    }
    Ok(())
}
fn same_generation(a: &Value, b: &Value) -> bool {
    ["provider", "session_id", "turn_id"]
        .iter()
        .all(|k| a[*k] == b[*k])
}
pub(crate) fn require_eligible(
    tx: &impl ReadOps,
    ctx: &Ctx<'_, impl Store>,
    id: &str,
) -> Result<(), StoreError> {
    let eligibility = QueryService::new(tx, ctx.project(), &ctx.now()).session_eligibility(id)?;
    if !eligibility.eligible {
        return Err(StoreError::Validation(format!(
            "continuation refused: story is {:?}",
            eligibility.reason
        )));
    }
    Ok(())
}
pub(crate) fn find(
    tx: &impl ReadOps,
    ctx: &Ctx<'_, impl Store>,
    id: &str,
    request: &str,
) -> Result<Continuation, StoreError> {
    tx.continuations(ctx.project())?
        .into_iter()
        .find(|r| r.id == request && r.story_id == id)
        .ok_or_else(|| {
            StoreError::NotFound(format!("continuation `{request}` for `{id}` not found"))
        })
}
pub(crate) fn save(
    tx: &mut impl WriteOps,
    record: &mut Continuation,
    now: &str,
) -> Result<(), StoreError> {
    let expected = record.revision;
    record.revision += 1;
    record.updated_at = now.into();
    if !tx.update_continuation(record, expected)? {
        return Err(StoreError::Validation(
            "continuation changed concurrently; reread current status".into(),
        ));
    }
    Ok(())
}

pub(crate) struct SubmissionEvidence {
    head: String,
    worktree: std::path::PathBuf,
    branch: String,
    clean: bool,
}
pub(crate) fn current_submission(path: &std::path::Path) -> Result<SubmissionEvidence, AppError> {
    let git = |args: &[&str]| -> Result<String, AppError> {
        let mut command = crate::env::git_env::command(path);
        command.args(args);
        let output = crate::process::run_captured(command, std::time::Duration::from_secs(5))
            .map_err(|e| {
                AppError::Validation(format!(
                    "continuation submission Git evidence: {}",
                    e.detail()
                ))
            })?;
        if !output.status.success() {
            return Err(AppError::Validation(format!(
                "cannot read continuation submission Git evidence: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().into())
    };
    Ok(SubmissionEvidence {
        head: git(&["rev-parse", "--verify", "HEAD"])?,
        worktree: std::path::PathBuf::from(git(&["rev-parse", "--show-toplevel"])?)
            .canonicalize()?,
        branch: git(&["symbolic-ref", "--quiet", "--short", "HEAD"])?,
        clean: git(&["status", "--porcelain", "--untracked-files=all"])?.is_empty(),
    })
}
pub(crate) fn check_submission(
    tx: &impl ReadOps,
    project: crate::store::ProjectId,
    story: crate::store::StoryNo,
    sequence: i64,
    evidence: Option<&Result<SubmissionEvidence, AppError>>,
) -> Result<(), StoreError> {
    let records = tx.continuations(project)?;
    if let Some(request) = records
        .iter()
        .rev()
        .find(|r| r.story_no == story && r.status.outstanding())
    {
        return Err(StoreError::Validation(format!(
            "continuation {} remains unresolved ({:?}); read status and acknowledge the receiving review before verifying",
            request.id, request.status
        )));
    }
    if let Some(request) = records.iter().rev().find(|r| {
        r.story_no == story
            && r.handoff["kind"] == "context"
            && r.status == ContinuationStatus::Acknowledged
    }) {
        if request.reviewed_seq != Some(sequence) {
            return Err(StoreError::Validation("continuation submission review is stale; reread current comments and refresh acknowledgement".into()));
        }
        let evidence = match evidence {
            Some(Ok(evidence)) => evidence,
            Some(Err(error)) => return Err(StoreError::Validation(error.to_string())),
            None => {
                return Err(StoreError::Validation(
                    "continuation submission is missing current Git evidence".into(),
                ));
            }
        };
        if !evidence.clean {
            return Err(StoreError::Validation("continuation submission requires a clean worktree; commit tracked, staged and untracked work before verifying".into()));
        }
        if request.reviewed_head.as_ref() != Some(&evidence.head) {
            return Err(StoreError::Validation("continuation submitted HEAD differs from the reviewed HEAD; rerun impacted tests and refresh acknowledgement".into()));
        }
        if request.capture["lease"]["worktree_path"].as_str() != evidence.worktree.to_str()
            || request.capture["lease"]["branch"] != evidence.branch
        {
            return Err(StoreError::Validation(
                "continuation submission must originate in the retained worktree and branch".into(),
            ));
        }
    }
    Ok(())
}
