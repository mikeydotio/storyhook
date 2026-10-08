//! Readmission is one atomic durable effect. It grants no stale gate receipt.
use super::*;
use crate::domain::StoryEvent;

/// Durable head constraint for a retained generation. It is checked again at
/// the private pinned-input callback before any ordinary gate can execute.
pub(in crate::service::project_recovery) fn expected_head(
    tx: &impl ReadOps,
    project: ProjectId,
    story: StoryNo,
    generation: Option<GlobalSeq>,
) -> Result<Option<String>, StoreError> {
    let mut expected = None;
    for view in super::super::references::views_naming(tx, project, story)? {
        if !view.state.shared.as_ref().is_some_and(|shared| {
            shared
                .readmissions
                .iter()
                .any(|receipt| receipt.story == story && Some(receipt.generation) == generation)
        }) {
            continue;
        }
        let observation = view
            .observations
            .iter()
            .find(|o| o.story == story && Some(o.generation) == generation)
            .ok_or_else(|| invalid("readmission lacks original submission evidence"))?;
        let evidence = validate_observation(&view.state, &view.record, observation)?;
        let head = evidence
            .attribution
            .inputs
            .head
            .as_ref()
            .filter(|head| crate::service::project_fault::is_pinned_oid(head))
            .ok_or_else(|| invalid("readmission has no pinned original head"))?;
        if expected.as_ref().is_some_and(|previous| previous != head) {
            return Err(invalid(
                "readmission owners disagree about the retained head",
            ));
        }
        expected = Some(head.clone());
    }
    Ok(expected)
}

pub(in crate::service::project_recovery) fn check_input(
    tx: &impl ReadOps,
    candidate: &VerificationCandidate,
    head: &str,
) -> Result<(), StoreError> {
    let prefix = crate::service::project_prefix(tx, candidate.project)?;
    let story = StoryNo::parse_id(&prefix, &candidate.story_id)?;
    if let Some(expected) =
        expected_head(tx, candidate.project, story, candidate.verifying_generation)?
        && expected != head
    {
        return Err(invalid(&format!(
            "retained submission head changed: expected {expected}, observed {head}; hold without running a gate or adopting the changed head"
        )));
    }
    for view in super::super::references::views_naming(tx, candidate.project, story)? {
        let Some(receipt) = view.state.shared.as_ref().and_then(|shared| {
            shared.readmissions.iter().find(|receipt| {
                receipt.story == story && Some(receipt.generation) == candidate.verifying_generation
            })
        }) else {
            continue;
        };
        let subject = view
            .state
            .subjects
            .iter()
            .find(|subject| {
                subject.story == story
                    && subject.candidate.verifying_generation == candidate.verifying_generation
            })
            .ok_or_else(|| invalid("readmission subject disappeared"))?;
        let observation = view
            .observations
            .iter()
            .find(|o| o.story == story && Some(o.generation) == candidate.verifying_generation)
            .ok_or_else(|| invalid("readmission observation disappeared"))?;
        let evidence = validate_observation(&view.state, &view.record, observation)?;
        if !same_submission(tx, &subject.candidate, story)?
            || authority::state_revision(tx, candidate.project, story)? != subject.state_revision
            || authority::label_revision(tx, candidate.project, story)? != subject.label_revision
            || super::super::resume::awaiting_revision(tx, candidate.project, story)?
                != Some(receipt.event)
            || !crate::service::automations::permits_generation(
                tx,
                candidate.project,
                candidate.verifying_generation,
            )?
        {
            return Err(invalid(
                "retained readmission authority changed before its fresh gate",
            ));
        }
        evidence.native.verify()?;
    }
    Ok(())
}

pub(in crate::service::project_recovery) fn eligible(
    tx: &impl ReadOps,
    view: &RecoveryView,
    hold: &crate::service::project_recovery::OwnedDependencyHold,
) -> Result<bool, StoreError> {
    let Some(shared) = &view.state.shared else {
        return Ok(false);
    };
    if super::super::resolution::release(&view.state).is_none()
        || shared
            .readmissions
            .iter()
            .any(|r| r.story == hold.story && r.generation == hold.generation)
    {
        return Ok(false);
    }
    let project = view.record.project;
    let Some(subject) = view.state.subjects.iter().find(|s| {
        s.story == hold.story && s.candidate.verifying_generation == Some(hold.generation)
    }) else {
        return Ok(false);
    };
    let Some(row) = tx.story(project, hold.story)? else {
        return Ok(false);
    };
    if subject.returned
        || row.state != crate::service::verification::VERIFYING_STATE
        || row.awaiting.as_deref() != Some(&hold.awaiting)
        || super::super::resume::awaiting_revision(tx, project, hold.story)? != Some(hold.event)
        || authority::state_revision(tx, project, hold.story)? != subject.state_revision
        || authority::label_revision(tx, project, hold.story)? != subject.label_revision
        || authority::policy_hold(tx, project, &row.snapshot)?.is_some()
        || !crate::service::automations::permits_generation(tx, project, Some(hold.generation))?
        || super::super::resume::resource_hold(tx, project, hold.story)?
        || crate::service::verification::verifying_entry(tx, project, hold.story)?
            .map(|(_, seq)| seq)
            != Some(hold.generation)
        || !same_submission(tx, &subject.candidate, hold.story)?
    {
        return Ok(false);
    }
    let mut snapshot = row.snapshot;
    snapshot.awaiting = None;
    if crate::domain::is_blocked(&snapshot, &crate::service::query::story_map(tx, project)?) {
        return Ok(false);
    }
    // Preserve every later manual block episode, even if it has since been cleared.
    if tx.events_for(project, hold.story)?.iter().any(|e| e.global_seq > hold.event
        && matches!(e.known(), Some(StoryEvent::StoryRelationshipAdded { relation, .. }) if relation == "blocked-by")) {
        return Ok(false);
    }
    let Some(observation) = view
        .observations
        .iter()
        .find(|o| o.story == hold.story && o.generation == hold.generation)
    else {
        return Ok(false);
    };
    let evidence = validate_observation(&view.state, &view.record, observation)?;
    Ok(evidence.native.verify().is_ok()
        && evidence.attribution.components.len() == 1
        && tx
            .attributions(project)?
            .iter()
            .any(|a| a == &evidence.attribution))
}

fn same_submission(
    tx: &impl ReadOps,
    candidate: &VerificationCandidate,
    story: StoryNo,
) -> Result<bool, StoreError> {
    let Some(original) = candidate.pull_request.as_ref().ok() else {
        return Ok(false);
    };
    let links: Vec<_> = tx
        .open_pr_links_for_story(candidate.project, story)?
        .into_iter()
        .filter(|p| p.close_on_merge)
        .collect();
    let [current] = links.as_slice() else {
        return Ok(false);
    };
    Ok(
        tx.checkout_path(candidate.project)?.as_deref() == Some(candidate.checkout.as_path())
            && matches!((crate::domain::pr_url::parse_pr_url(&original.url), crate::domain::pr_url::parse_pr_url(&current.url)), (Ok(a), Ok(b)) if a == b),
    )
}

pub(in crate::service::project_recovery) fn reconcile<S: Store>(
    tx: &mut impl WriteOps,
    ctx: &crate::service::Ctx<'_, S>,
    view: &mut RecoveryView,
    now: &str,
) -> Result<bool, StoreError> {
    let Some(release) = super::super::resolution::release(&view.state) else {
        return Ok(false);
    };
    let holds = view
        .state
        .decision
        .as_ref()
        .map(|d| d.dependency_holds.clone())
        .unwrap_or_default();
    let mut changed = false;
    for hold in holds {
        if !eligible(tx, view, &hold)? {
            continue;
        }
        let observation = view
            .observations
            .iter()
            .find(|o| o.story == hold.story && o.generation == hold.generation)
            .expect("eligible observation");
        let evidence = validate_observation(&view.state, &view.record, observation)?;
        let mut attribution = evidence.attribution.clone();
        // A shared proof covers one component. Unknown or unrelated failures keep
        // their independent hold; source recovery must not erase those diagnostics.
        if attribution.components.len() != 1 {
            continue;
        }
        attribution.revision += 1;
        attribution.held = false;
        attribution.retired = Some(format!(
            "shared recovery {} released component {}",
            view.record.id, evidence.component
        ));
        if !tx.update_attribution(&attribution, evidence.attribution.revision)? {
            return Err(invalid("shared attribution changed before readmission"));
        }
        let project = view.record.project;
        let row = tx
            .story(project, hold.story)?
            .ok_or_else(|| invalid("readmission subject disappeared"))?;
        let events = [
            StoryEvent::StoryAwaitingCleared { at: now.into() },
            StoryEvent::StoryCommentAdded {
                at: now.into(),
                text: format!(
                    "SHARED RECOVERY {} READMISSION — {}. Retained generation {} is eligible for a fresh current-base merge and central gate. The old gate grants no certification; the verifier must inspect the current PR head. No author resubmission is required.",
                    view.record.id,
                    release.cause.clause(),
                    hold.generation.get()
                ),
            },
        ];
        crate::service::append_and_fold(
            tx,
            project,
            hold.story,
            &crate::service::project_prefix(tx, project)?,
            &tx.state_map(project)?,
            crate::store::ExpectedSeq::Exact(row.head_seq),
            &events,
            ctx.provenance(),
        )?;
        let event = super::super::resume::awaiting_revision(tx, project, hold.story)?
            .ok_or_else(|| invalid("readmission event missing"))?;
        view.state
            .shared
            .as_mut()
            .expect("shared view")
            .readmissions
            .push(SharedReadmission {
                story: hold.story,
                generation: hold.generation,
                attribution: evidence.attribution.id,
                component: evidence.component,
                event,
            });
        changed = true;
    }
    Ok(changed)
}

pub(in crate::service::project_recovery) fn validate(
    tx: &impl ReadOps,
    view: &RecoveryView,
) -> Result<(), StoreError> {
    let Some(shared) = &view.state.shared else {
        return Ok(());
    };
    let mut seen = std::collections::BTreeSet::new();
    for receipt in &shared.readmissions {
        let Some(release) = super::super::resolution::release(&view.state) else {
            return Err(invalid("readmission lacks release authority"));
        };
        let hold = view.state.decision.as_ref().and_then(|d| {
            d.dependency_holds
                .iter()
                .find(|h| h.story == receipt.story && h.generation == receipt.generation)
        });
        if !seen.insert((receipt.story, receipt.generation))
            || hold.is_none_or(|h| receipt.event <= h.event)
            || receipt.event <= release.anchor
            || !tx
                .events_for(view.record.project, receipt.story)?
                .iter()
                .any(|e| {
                    e.global_seq == receipt.event
                        && matches!(e.known(), Some(StoryEvent::StoryAwaitingCleared { .. }))
                })
        {
            return Err(invalid(
                "readmission has inconsistent hold or release event",
            ));
        }
        let observation = view
            .observations
            .iter()
            .find(|o| o.story == receipt.story && o.generation == receipt.generation)
            .ok_or_else(|| invalid("readmission lacks native observation"))?;
        let evidence = validate_observation(&view.state, &view.record, observation)?;
        if receipt.attribution != evidence.attribution.id
            || receipt.component != evidence.component
            || evidence.attribution.components.len() != 1
        {
            return Err(invalid(
                "readmission does not match its single proved component",
            ));
        }
        let mut expected = evidence.attribution;
        expected.revision += 1;
        expected.held = false;
        expected.retired = Some(format!(
            "shared recovery {} released component {}",
            view.record.id, receipt.component
        ));
        if !tx.attributions(view.record.project)?.contains(&expected) {
            return Err(invalid("readmission attribution release receipt changed"));
        }
    }
    Ok(())
}
