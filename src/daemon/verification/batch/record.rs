//! A batch's durable record and its GitHub publication (SH-831): recording a
//! new batch, moving it through its phases, and the pull request it is
//! published and gated as. The helpers take the record they act on, so any
//! batch record the attempt writes goes through the same code.

use super::end::{member_list, write};
use super::*;

impl<S: Store> Attempt<'_, S> {
    /// Records the batch the plan assembled, with `tip` as its last merge
    /// commit, and shows it in status.
    pub(super) fn insert(&mut self, id: BatchId, tip: String) -> Result<(), AppError> {
        let batch = self.new_record(id, tip, self.planned_members(), self.plan.excluded.clone());
        store_new(self.store, &batch)?;
        journal(
            "INFO",
            self.head,
            &format!(
                "verification batch {} assembled: {} at {}",
                batch.id,
                member_list(&batch),
                batch.tip
            ),
        );
        if let Some(membership) = &self.membership {
            membership.show(&batch.id, batch.phase);
        }
        self.record = Some(batch);
        Ok(())
    }

    /// Moves the attempt's live record to `phase`, applies `change`, and
    /// shows the new phase in status.
    pub(super) fn advance(
        &mut self,
        phase: BatchPhase,
        change: impl FnOnce(&mut VerificationBatch),
    ) -> Result<(), AppError> {
        let current = self.record.as_ref().ok_or_else(|| {
            AppError::Storage("a batch phase moved before the batch was recorded".into())
        })?;
        let next = advance_record(self.store, self.env, self.head, current, phase, change)?;
        if let Some(membership) = &self.membership {
            membership.show(&next.id, phase);
        }
        self.record = Some(next);
        Ok(())
    }

    /// The plan's members as a record lists them, in queue order.
    pub(super) fn planned_members(&self) -> Vec<BatchMember> {
        self.plan
            .members
            .iter()
            .enumerate()
            .map(|(position, member)| BatchMember {
                story: member.story,
                story_id: member.candidate.story_id.clone(),
                generation: member.generation,
                head_commit: member.commit.clone(),
                pull_request: member.pull_request.clone(),
                position: position as u32,
                branch: member
                    .candidate
                    .cleanup_lease
                    .as_ref()
                    .map(|lease| lease.branch.clone()),
            })
            .collect()
    }

    /// A new, assembled record of this attempt's head and base.
    pub(super) fn new_record(
        &self,
        id: BatchId,
        tip: String,
        members: Vec<BatchMember>,
        excluded: Vec<BatchExclusion>,
    ) -> VerificationBatch {
        let now = self.env.now();
        VerificationBatch {
            branch: id.branch(),
            id,
            project: self.head.project,
            project_slug: self.head.project_slug.clone(),
            head: self.head.story_id.clone(),
            base_branch: self.plan.base_branch.clone(),
            base_commit: self.plan.base_commit.clone(),
            tip,
            pull_request: None,
            phase: BatchPhase::Assembled,
            members,
            excluded,
            gate: None,
            detail: None,
            retired: false,
            revision: 0,
            created_at: now.clone(),
            updated_at: now,
        }
    }
}

/// Writes a new live record, retention included.
pub(super) fn store_new(store: &impl Store, batch: &VerificationBatch) -> Result<(), AppError> {
    let project = batch.project;
    let now = batch.created_at.clone();
    store.write(|tx| {
        // Backstop for B10: a batch still live here was left by a tick
        // that failed before it could end it.
        abandon_live(
            tx,
            project,
            "a newer batch of the project was recorded while this one was still live",
            &now,
        )?;
        tx.insert_verification_batch(batch)?;
        tx.prune_verification_batches(project, RETAINED_BATCHES)?;
        Ok(())
    })?;
    Ok(())
}

/// Moves `current` to `phase` with `change` applied, writes it, and answers
/// the record as written.
pub(super) fn advance_record(
    store: &impl Store,
    env: &Environment,
    head: &VerificationCandidate,
    current: &VerificationBatch,
    phase: BatchPhase,
    change: impl FnOnce(&mut VerificationBatch),
) -> Result<VerificationBatch, AppError> {
    let mut next = current.advance(phase, &env.now())?;
    change(&mut next);
    write(store, &next, current.revision)?;
    journal(
        "INFO",
        head,
        &format!("verification batch {} {}", next.id, phase.as_str()),
    );
    Ok(next)
}

/// The pull request `batch` is published as.
pub(super) fn publication(batch: &VerificationBatch) -> BatchPublication {
    let members: Vec<String> = batch
        .members
        .iter()
        .map(|member| {
            let number = parse_pr_url(&member.pull_request)
                .map(|reference| format!("#{}", reference.number))
                .unwrap_or_else(|_| member.pull_request.clone());
            format!(
                "{}. {} — {number} (head {})",
                member.position + 1,
                member.story_id,
                &member.head_commit[..12]
            )
        })
        .collect();
    BatchPublication {
        branch: batch.branch.clone(),
        tip: batch.tip.clone(),
        base: batch.base_branch.clone(),
        title: format!("Verification batch {}: {}", batch.id, member_list(batch)),
        body: format!(
            "Verification batch `{}` of project `{}`, formed by the storyhook verifier.\n\n\
             Base: `{}` at {}.\n\n\
             Members, merged in queue order as merge commits:\n{}\n\n\
             The verifier gates this pull request's merge tree. Landing a batch is not built \
             yet, so the verifier closes this pull request when its gate ends and verifies \
             each member on its own.",
            batch.id,
            batch.project_slug,
            batch.base_branch,
            batch.base_commit,
            members.join("\n")
        ),
    }
}

/// The link the gate verifies for `batch`'s pull request.
pub(super) fn pull_request_link(
    batch: &VerificationBatch,
    now: String,
) -> Result<PrLink, AppError> {
    let pull_request = batch
        .pull_request
        .clone()
        .ok_or_else(|| AppError::Storage("the batch has no pull request to gate".into()))?;
    let reference = parse_pr_url(&pull_request.url)?;
    Ok(PrLink {
        owner: reference.owner,
        repo: reference.repo,
        number: pull_request.number,
        url: pull_request.url,
        close_on_merge: false,
        status: "open".into(),
        linked_at: now,
        last_checked_at: None,
    })
}
