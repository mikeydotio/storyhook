//! Administrative obviation evidence is atomic and never implementation approval.
use crate::domain::{StoryEvent, SuperState};
use crate::service::{Ctx, append_and_fold, project_prefix, resolve_story};
use crate::store::{Continuation, ExpectedSeq, Store, StoreError, StoryNo, StoryRow, WriteOps};
use std::collections::BTreeSet;

pub(super) fn record(
    tx: &mut impl WriteOps,
    ctx: &Ctx<'_, impl Store>,
    record: &Continuation,
    row: &StoryRow,
) -> Result<(), StoreError> {
    let evidence = &record.handoff["evidence"];
    if row.superstate != SuperState::Open || evidence["original_state"] != row.state {
        return Err(StoreError::Validation(
            "obviation review state changed; reread before requesting the guarded transition"
                .into(),
        ));
    }
    let candidates = evidence["candidates"]
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or_else(|| {
            StoreError::Validation("obviation review requires nonempty candidate IDs".into())
        })?;
    let prefix = project_prefix(tx, ctx.project())?;
    let mut names = BTreeSet::new();
    let mut rows = Vec::new();
    for candidate in candidates {
        let id = candidate.as_str().ok_or_else(|| {
            StoreError::Validation("candidate must be a canonical story ID".into())
        })?;
        let no = StoryNo::parse_id(&prefix, id)?;
        if no.to_id(&prefix) != id || id == record.story_id || !names.insert(id) {
            return Err(StoreError::Validation(
                "obviation candidates must be unique canonical IDs other than the subject".into(),
            ));
        }
        rows.push((id, resolve_story(tx, ctx.project(), &prefix, id)?));
    }
    let states = tx.state_map(ctx.project())?;
    let blocked = states
        .get("blocked")
        .filter(|s| s.super_state == SuperState::Open)
        .ok_or_else(|| {
            StoreError::Validation("obviation review requires the open blocked state".into())
        })?;
    let mut events = vec![StoryEvent::StoryCommentAdded {
        at: ctx.now(),
        text: format!(
            "POSSIBLE OBVIATION {}\nOriginal state: {}\nEvidence: {}\nHuman determination remains required; no implementation approval granted.",
            record.id, row.state, evidence
        ),
    }];
    for (id, (number, candidate)) in rows {
        if !row
            .snapshot
            .relationships
            .iter()
            .any(|r| r.relation == "obviated-by" && r.other_id == id)
        {
            events.push(StoryEvent::StoryRelationshipAdded {
                at: ctx.now(),
                other_id: id.into(),
                relation: "obviated-by".into(),
            });
        }
        if !candidate
            .snapshot
            .relationships
            .iter()
            .any(|r| r.relation == "obviates" && r.other_id == record.story_id)
        {
            append_and_fold(
                tx,
                ctx.project(),
                number,
                &prefix,
                &states,
                ExpectedSeq::Exact(candidate.head_seq),
                &[StoryEvent::StoryRelationshipAdded {
                    at: ctx.now(),
                    other_id: record.story_id.clone(),
                    relation: "obviates".into(),
                }],
                ctx.provenance(),
            )?;
        }
    }
    let reason = match &row.awaiting {
        Some(existing) => format!("{existing}\nHuman review of possible obviation"),
        None => "Human review of possible obviation".into(),
    };
    events.push(StoryEvent::StoryAwaitingSet {
        at: ctx.now(),
        awaiting: reason,
    });
    let events = crate::service::story::state_transition_events(
        blocked,
        row.awaiting.is_some(),
        &ctx.now(),
        events,
    );
    append_and_fold(
        tx,
        ctx.project(),
        record.story_no,
        &prefix,
        &states,
        ExpectedSeq::Exact(row.head_seq),
        &events,
        ctx.provenance(),
    )?;
    Ok(())
}
