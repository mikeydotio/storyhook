//! One batch in progress: locking and submitting members, assembling and
//! recording the batch, publishing it and running its gate (SH-831).

use super::end::{member_list, write};
use super::*;

/// One batch in progress.
pub(super) struct Attempt<'a, S: Store> {
    pub(super) store: &'a S,
    pub(super) env: &'a Environment,
    pub(super) queue: &'a VerificationQueue<'a, S>,
    pub(super) ctx: &'a Ctx<'a, S>,
    pub(super) batching: &'a dyn BatchActuator,
    pub(super) head: &'a VerificationCandidate,
    pub(super) plan: Plan,
    pub(super) cancellation: &'a Cancellation,
    /// Head and members whose authority the observer checks.
    pub(super) tracked: &'a Mutex<Vec<VerificationCandidate>>,
    /// The slot's listing of the batch's members while the steps run.
    pub(super) membership: Option<BatchMembership<'a>>,
    /// The durable record, once written.
    pub(super) record: Option<VerificationBatch>,
    /// Why the batch ended before a record was written.
    pub(super) dissolved: Option<String>,
    /// Why a recorded batch could not reach or finish its gate.
    pub(super) failure: Option<String>,
    /// The batch gate's outcome and duration in seconds.
    pub(super) gate: Option<(VerificationOutcome, u64)>,
}

impl<S: Store> Attempt<'_, S> {
    pub(super) fn steps(&mut self) -> Result<(), AppError> {
        let lockable: Vec<(StoryNo, String)> = self.plan.members[1..]
            .iter()
            .map(|member| (member.story, member.candidate.story_id.clone()))
            .collect();
        let (mut locks, busy) = MemberLocks::acquire(&self.head.checkout, &lockable)?;
        for story_id in busy {
            self.exclude(
                &story_id,
                BatchExclusionReason::WorkspaceBusy,
                "another operation holds its workspace lock".into(),
            );
        }
        progress_item(self.env, self.head, "batch member submission", "running");
        let others: Vec<String> = self.plan.members[1..]
            .iter()
            .map(|member| member.candidate.story_id.clone())
            .collect();
        for story_id in others {
            if self.cancellation.is_cancelled() {
                return Ok(());
            }
            let Some(lock) = locks.get(&story_id) else {
                continue;
            };
            if let Err((reason, detail)) = self.submit(&story_id, lock) {
                locks.release(&story_id);
                self.exclude(&story_id, reason, detail);
            }
        }
        progress_item(self.env, self.head, "batch member submission", "passed");
        if self.plan.members.len() < 2 {
            self.dissolved = Some("fewer than two members remain after submission".into());
            return Ok(());
        }
        if self.cancellation.is_cancelled() {
            return Ok(());
        }
        progress_item(self.env, self.head, "batch assembly", "running");
        let id = BatchId::generate();
        let branch = id.branch();
        let assembly = match assemble(
            &self.plan.repository,
            &branch,
            &self.plan.base_commit,
            &self
                .plan
                .members
                .iter()
                .map(|member| AssemblyMember {
                    story_id: member.candidate.story_id.clone(),
                    branch: member
                        .candidate
                        .cleanup_lease
                        .as_ref()
                        .map_or_else(String::new, |lease| lease.branch.clone()),
                    commit: member.commit.clone(),
                })
                .collect::<Vec<_>>(),
            self.cancellation,
        ) {
            Ok(assembly) => assembly,
            Err(error) => {
                self.dissolved = Some(format!("assembling the batch branch failed: {error}"));
                return Ok(());
            }
        };
        progress_item(self.env, self.head, "batch assembly", "passed");
        self.insert(id, assembly.tip)?;
        let publication = self.publication();
        match self
            .batching
            .publish(self.head, &publication, self.cancellation)
        {
            Ok(pull_request) => self.advance(BatchPhase::Submitted, |batch| {
                batch.pull_request = Some(pull_request);
            })?,
            Err(error) => {
                self.failure = Some(format!("publishing the batch failed: {error}"));
                return Ok(());
            }
        }
        if self.cancellation.is_cancelled() {
            return Ok(());
        }
        self.advance(BatchPhase::Gating, |_| {})?;
        let link = self.pull_request_link()?;
        let gate_started = Instant::now();
        let outcome = self.batching.gate(self.head, &link, self.cancellation);
        self.gate = Some((outcome, gate_started.elapsed().as_secs()));
        Ok(())
    }

    /// Submits one member and records its submission; `Err` excludes it.
    fn submit(
        &mut self,
        story_id: &str,
        lock: &WorkspaceLock,
    ) -> Result<(), (BatchExclusionReason, String)> {
        let member = self
            .plan
            .members
            .iter()
            .find(|member| member.candidate.story_id == story_id)
            .cloned()
            .ok_or((
                BatchExclusionReason::Superseded,
                "it left the batch".to_owned(),
            ))?;
        let receipt = self
            .batching
            .submit_member(&member.candidate, MemberOwner(lock), self.cancellation)
            .map_err(|failure| match failure {
                SubmissionFailure::Refused { display, .. } => {
                    (BatchExclusionReason::SubmissionRefused, display)
                }
                SubmissionFailure::Infrastructure { detail } => {
                    (BatchExclusionReason::SubmissionFailed, detail)
                }
            })?;
        if receipt.head_oid != member.commit {
            return Err((
                BatchExclusionReason::HeadMoved,
                format!(
                    "its pushed head {} is not the commit {} the preview merged",
                    receipt.head_oid, member.commit
                ),
            ));
        }
        if receipt.base != self.plan.base_branch {
            return Err((
                BatchExclusionReason::BaseMismatch,
                format!(
                    "its pull request targets {}, not {}",
                    receipt.base, self.plan.base_branch
                ),
            ));
        }
        if let Ok(linked) = &member.candidate.pull_request
            && linked.number != receipt.number
        {
            return Err((
                BatchExclusionReason::PullRequestMismatch,
                format!(
                    "it links {} but its branch has {} open",
                    linked.url, receipt.url
                ),
            ));
        }
        match self
            .queue
            .record_generation_submitted(self.ctx, &member.candidate, &receipt)
        {
            Ok(GenerationWrite::Applied(_)) => {}
            Ok(GenerationWrite::Superseded) => {
                return Err((
                    BatchExclusionReason::Superseded,
                    "its generation changed before its submission was recorded".into(),
                ));
            }
            Err(error) => {
                return Err((
                    BatchExclusionReason::SubmissionFailed,
                    format!("recording its submission failed: {error}"),
                ));
            }
        }
        let fresh = self
            .queue
            .current_for(&member.candidate)
            .map_err(|error| {
                (
                    BatchExclusionReason::SubmissionFailed,
                    format!("re-reading it after submission failed: {error}"),
                )
            })?
            .filter(|fresh| fresh.verifying_generation == Some(member.generation))
            .ok_or((
                BatchExclusionReason::Superseded,
                "its generation changed after its submission was recorded".to_owned(),
            ))?;
        for tracked in self
            .tracked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter_mut()
            .filter(|tracked| tracked.story_id == story_id)
        {
            *tracked = fresh.clone();
        }
        if let Some(planned) = self
            .plan
            .members
            .iter_mut()
            .find(|planned| planned.candidate.story_id == story_id)
        {
            planned.candidate = fresh;
            planned.pull_request = receipt.url;
        }
        Ok(())
    }

    fn exclude(&mut self, story_id: &str, reason: BatchExclusionReason, detail: String) {
        self.plan
            .members
            .retain(|member| member.candidate.story_id != story_id);
        self.tracked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|tracked| tracked.story_id != story_id);
        if let Some(membership) = &self.membership {
            membership.leave(story_id);
        }
        self.plan.excluded.push(BatchExclusion {
            story_id: story_id.to_owned(),
            reason,
            detail,
        });
    }

    fn insert(&mut self, id: BatchId, tip: String) -> Result<(), AppError> {
        let now = self.env.now();
        let batch = VerificationBatch {
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
            members: self
                .plan
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
                })
                .collect(),
            excluded: self.plan.excluded.clone(),
            gate: None,
            detail: None,
            retired: false,
            revision: 0,
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        let project = self.head.project;
        self.store.write(|tx| {
            // Backstop for B10: a batch still live here was left by a tick
            // that failed before it could end it.
            abandon_live(
                tx,
                project,
                "a newer batch of the project was recorded while this one was still live",
                &now,
            )?;
            tx.insert_verification_batch(&batch)?;
            tx.prune_verification_batches(project, RETAINED_BATCHES)?;
            Ok(())
        })?;
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
        self.record = Some(batch);
        Ok(())
    }

    fn advance(
        &mut self,
        phase: BatchPhase,
        change: impl FnOnce(&mut VerificationBatch),
    ) -> Result<(), AppError> {
        let current = self.record.as_ref().ok_or_else(|| {
            AppError::Storage("a batch phase moved before the batch was recorded".into())
        })?;
        let mut next = current.advance(phase, &self.env.now())?;
        change(&mut next);
        write(self.store, &next, current.revision)?;
        journal(
            "INFO",
            self.head,
            &format!("verification batch {} {}", next.id, phase.as_str()),
        );
        self.record = Some(next);
        Ok(())
    }

    fn publication(&self) -> BatchPublication {
        let batch = self
            .record
            .as_ref()
            .expect("the batch is recorded before it is published");
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

    fn pull_request_link(&self) -> Result<PrLink, AppError> {
        let pull_request = self
            .record
            .as_ref()
            .and_then(|batch| batch.pull_request.clone())
            .ok_or_else(|| AppError::Storage("the batch has no pull request to gate".into()))?;
        let reference = parse_pr_url(&pull_request.url)?;
        Ok(PrLink {
            owner: reference.owner,
            repo: reference.repo,
            number: pull_request.number,
            url: pull_request.url,
            close_on_merge: false,
            status: "open".into(),
            linked_at: self.env.now(),
            last_checked_at: None,
        })
    }
}
