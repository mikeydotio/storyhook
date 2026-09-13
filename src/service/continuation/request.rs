//! Atomic handoff intake and one-shot native feedback ownership.
use super::{
    ContinuationService, administrative, require_eligible, same_generation, validate_capture,
    validate_request,
};
use crate::domain::StoryEvent;
use crate::error::AppError;
use crate::service::{append_and_fold, project_prefix, resolve_story};
use crate::store::{
    BlockAction, Continuation, ContinuationPhase, ContinuationStatus, DeliveryStatus, ExpectedSeq,
    ReadOps, Store, StoreError, WriteOps,
};
use serde_json::{Value, json};

impl<S: Store> ContinuationService<'_, S> {
    /// Capture a strict handoff and atomically persist its story evidence and delivery intent.
    pub fn request(&self, id: &str, input: Value) -> Result<Continuation, AppError> {
        self.request_with_receipt(id, input)
            .map(|(record, _)| record)
    }
    /// Atomically decide whether this request owns the native Stop feedback.
    /// Existing generations return their record with false, including concurrent retries.
    pub fn request_with_receipt(
        &self,
        id: &str,
        input: Value,
    ) -> Result<(Continuation, bool), AppError> {
        validate_request(id, &input)?;
        let answer = self.runtime.call("capture", &input)?;
        let capture = answer
            .get("capture")
            .filter(|_| answer["ok"] == true)
            .ok_or_else(|| {
                AppError::Validation(format!(
                    "continuation capture refused: {}",
                    answer["detail"]
                ))
            })?
            .clone();
        validate_capture(id, &input["provider"], &capture)?;
        self.ctx.write_stories(|tx| {
            let project_id=self.ctx.project();
            let prefix=project_prefix(tx,project_id)?;
            let (number,row)=resolve_story(tx,project_id,&prefix,id)?;
            let project=tx.project(project_id)?.ok_or_else(||StoreError::NotFound("continuation project disappeared".into()))?;
            if capture["lease"]["project_slug"]!=project.slug {
                return Err(StoreError::Validation("continuation lease belongs to another project".into()));
            }
            let records=tx.continuations(project_id)?;
            if let Some(old)=records.iter().find(|r|r.story_id==id&&r.handoff["kind"]==input["handoff"]["kind"]&&same_generation(&r.generation,&capture)) {
                if old.handoff!=input["handoff"] {
                    return Err(StoreError::Validation("conflicting continuation payload for this session generation".into()));
                }
                return Ok((old.clone(),false));
            }
            if input["handoff"]["kind"]=="context" && records.iter().any(|r|r.story_id==id&&r.status.outstanding()) {
                return Err(StoreError::Validation("story already has an unresolved continuation".into()));
            }
            let generic_resume=tx.block_deliveries(project_id)?.iter().any(|delivery| {
                delivery.story==number&&delivery.action==BlockAction::Resume
                    &&delivery.status==DeliveryStatus::Attempting
            });
            if generic_resume && input["handoff"]["kind"]=="context" {
                return Err(StoreError::Validation("an unblock delivery already owns the effect boundary; continuation cannot race its input".into()));
            }
            let mut record=Continuation {
                id:uuid::Uuid::new_v4().to_string(),
                project_id,
                story_no:number,
                story_id:id.into(),
                handoff:input["handoff"].clone(),
                generation:json!({"provider":capture["provider"],"session_id":capture["session_id"],"turn_id":capture["turn_id"]}),
                capture:capture.clone(),
                status:ContinuationStatus::AwaitingAck,
                phase:ContinuationPhase::NativeContinuation,
                revision:0,
                attempts:0,
                created_at:self.ctx.now(),
                updated_at:self.ctx.now(),
                detail:"context handoff recorded; native feedback receipt owned by this request".into(),
                reviewed_seq:None,
                reviewed_head:None,
            };
            if let Some(binding) = engine_binding(tx, &project.slug, id)? {
                record.capture["engine_lane"] = binding;
            }
            if input["handoff"]["kind"]=="obviation-review" {
                administrative::record(tx,self.ctx,&record,&row)?;
                record.status=ContinuationStatus::Acknowledged;
                record.phase=ContinuationPhase::Administrative;
                record.detail="pending human obviation review; no implementation approval granted".into();
            } else {
                require_eligible(tx,self.ctx,id)?;
                let unchanged=records.iter().rev()
                    .filter(|r|r.story_id==id&&r.handoff["kind"]=="context")
                    .take_while(|r|r.capture["head"]==capture["head"]&&r.capture["fingerprint"]==capture["fingerprint"])
                    .count();
                if unchanged>=2 {
                    record.status=ContinuationStatus::NeedsAttention;
                    record.detail="three consecutive context handoffs without changed HEAD or dirty content; automatic continuation stopped".into();
                }
                let states=tx.state_map(project_id)?;
                let comment=StoryEvent::StoryCommentAdded {
                    at:self.ctx.now(),
                    text:format!("CONTEXT HANDOFF {}\n{}\nRuntime evidence: {}",record.id,record.handoff,record.capture),
                };
                append_and_fold(tx,project_id,number,&prefix,&states,ExpectedSeq::Exact(row.head_seq),&[comment],self.ctx.provenance())?;
            }
            tx.insert_continuation(&record)?;
            let feedback=record.status==ContinuationStatus::AwaitingAck&&record.phase==ContinuationPhase::NativeContinuation;
            Ok((record,feedback))
        }).map_err(Into::into)
    }
}

fn engine_binding(
    tx: &impl ReadOps,
    project: &str,
    story: &str,
) -> Result<Option<Value>, StoreError> {
    let mut binding = None;
    for run in tx
        .live_engine_runs()?
        .into_iter()
        .filter(|run| run.project_slug == project)
    {
        for lane in tx.engine_lanes(&run.id)? {
            if lane.story_id.as_deref() == Some(story) {
                if binding.is_some() {
                    return Err(StoreError::Validation(format!(
                        "multiple live engine lanes claim continuation story {story}"
                    )));
                }
                binding = Some(json!({"run_id":run.id,"lane_index":lane.lane_index}));
            }
        }
    }
    Ok(binding)
}
