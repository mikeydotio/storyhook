//! One batch in progress: locking and submitting members, assembling and
//! recording the batch, publishing it and running its gate (SH-831).

use super::*;

/// One batch in progress.
pub(super) struct Attempt<'a, S: Store> {
    pub(super) store: &'a S,
    pub(super) env: &'a Environment,
    pub(super) queue: &'a VerificationQueue<'a, S>,
    pub(super) ctx: &'a Ctx<'a, S>,
    pub(super) batching: &'a dyn BatchActuator,
    pub(super) head: &'a VerificationCandidate,
    pub(super) owner: &'a VerificationGuard,
    pub(super) plan: Plan,
    pub(super) cancellation: &'a Cancellation,
    /// Head and members whose authority the observer checks.
    pub(super) tracked: &'a Mutex<Vec<VerificationCandidate>>,
    /// The slot's listing of the batch's members while the steps run.
    pub(super) membership: Option<BatchMembership<'a>>,
    /// The non-head members' workspace locks, held until the batch ends.
    pub(super) locks: Option<MemberLocks>,
    /// The durable record, once written.
    pub(super) record: Option<VerificationBatch>,
    /// Why the batch ended before a record was written.
    pub(super) dissolved: Option<String>,
    /// Why a recorded batch could not reach or finish its gate.
    pub(super) failure: Option<String>,
    /// The batch gate's outcome and duration in seconds.
    pub(super) gate: Option<(VerificationOutcome, u64)>,
    /// The bisection of a red batch, once its record ended (SH-833). From
    /// then on `record` holds only a live probe record, if any.
    pub(super) bisection: Option<super::bisect::Bisecting>,
}

impl<S: Store> Attempt<'_, S> {
    pub(super) fn steps(&mut self) -> Result<(), AppError> {
        // Before anything is submitted: a base that requires signed commits
        // would refuse the batch's unsigned merge commits after the landing
        // attempt began, fencing every member (SH-832 D8).
        let base = self.plan.base_branch.clone();
        match self
            .batching
            .base_policy(self.head, &base, self.cancellation)
        {
            Ok(false) => {}
            Ok(true) => {
                self.dissolved = Some(format!(
                    "{base} requires signed commits, and a batch's merge commits are unsigned"
                ));
                return Ok(());
            }
            Err(error) => {
                self.dissolved = Some(format!(
                    "whether {base} requires signed commits could not be read: {error}"
                ));
                return Ok(());
            }
        }
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
                break;
            }
            let Some(lock) = locks.get(&story_id) else {
                continue;
            };
            if let Err((reason, detail)) = self.submit(&story_id, lock) {
                locks.release(&story_id);
                self.exclude(&story_id, reason, detail);
            }
        }
        // The member locks outlive the steps: a landed member is reaped
        // under its own lock (SH-832 D6).
        self.locks = Some(locks);
        if self.cancellation.is_cancelled() {
            return Ok(());
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
        let Some((assembly, resolution)) = self.assemble(&id, &branch) else {
            return Ok(());
        };
        progress_item(self.env, self.head, "batch assembly", "passed");
        self.insert(id, &assembly, resolution)?;
        let publication = record::publication(
            self.record
                .as_ref()
                .expect("the batch is recorded before it is published"),
        );
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
        let link = record::pull_request_link(
            self.record
                .as_ref()
                .ok_or_else(|| AppError::Storage("the batch has no record to gate".into()))?,
            self.env.now(),
        )?;
        let gate_started = Instant::now();
        let outcome = self.costed_gate(&link, self.plan.members.len())?;
        self.gate = Some((outcome, gate_started.elapsed().as_secs()));
        self.bisect()
    }

    /// Every probe is a separate physical execution inside the same admission.
    pub(super) fn costed_gate(
        &self,
        link: &PrLink,
        prefix: usize,
    ) -> Result<VerificationOutcome, AppError> {
        cost::execute(
            self.store,
            self.env,
            self.owner,
            self.head,
            crate::store::GateExecutionPurpose::Gate,
            crate::store::GateInputs::default(),
            self.plan
                .members
                .iter()
                .take(prefix)
                .map(|member| cost::submission(&member.candidate))
                .collect(),
            |_| self.batching.gate(self.head, link, self.cancellation),
            |outcome| Ok(Some(outcome.clone())),
        )
    }

    /// Assembles the batch branch: the clean members as merge commits in
    /// queue order, then a smoothed last member, if the preview admitted one
    /// (SH-834), with its conflict classified again and united. A smoothed
    /// member the assembly refuses, or cannot resolve, is left out, and the
    /// batch goes on with two or more members. `None` when the batch
    /// dissolved or was stopped.
    fn assemble(
        &mut self,
        id: &BatchId,
        branch: &str,
    ) -> Option<(Assembly, Option<BatchResolution>)> {
        let members: Vec<AssemblyMember> = self
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
            .collect();
        let smoothed = self
            .plan
            .members
            .last()
            .is_some_and(|member| member.smoothed);
        let (clean, last) = if smoothed {
            let (last, clean) = members.split_last().expect("a batch has members");
            (clean, Some(last))
        } else {
            (members.as_slice(), None)
        };
        let mut assembly = match assemble(
            &self.plan.repository,
            branch,
            &self.plan.base_commit,
            clean,
            self.cancellation,
        ) {
            Ok(assembly) => assembly,
            Err(error) => {
                self.dissolved = Some(format!("assembling the batch branch failed: {error}"));
                return None;
            }
        };
        let Some(last) = last else {
            return Some((assembly, None));
        };
        let merged = merge_smoothed(
            &SmoothedMerge {
                repository: &self.plan.repository,
                branch,
                batch: id.as_str(),
                base: &self.plan.base_commit,
                earlier: clean,
                member: last,
            },
            &mut assembly,
            self.cancellation,
        );
        if self.cancellation.is_cancelled() {
            return None;
        }
        let (reason, detail) = match merged {
            Ok(LastMerge::Clean) => return Some((assembly, None)),
            Ok(LastMerge::Resolved(resolution)) => {
                journal(
                    "INFO",
                    self.head,
                    &format!(
                        "verification batch {id}: {} joins last with an automated resolution \
                         ({}) of {}",
                        last.story_id,
                        resolution.strategy,
                        resolution
                            .files
                            .iter()
                            .map(|file| file.path.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                );
                return Some((assembly, Some(resolution)));
            }
            Ok(LastMerge::Refused(why)) => (BatchExclusionReason::ConflictNotSmoothable, why),
            Err(error) => (
                BatchExclusionReason::ResolutionFailed,
                format!("resolving its conflict failed: {error}"),
            ),
        };
        if let Some(locks) = self.locks.as_mut() {
            locks.release(&last.story_id);
        }
        let left_out = format!("{} was left out: {detail}", last.story_id);
        self.exclude(&last.story_id, reason, detail);
        if self.plan.members.len() < 2 {
            self.dissolved = Some(format!(
                "fewer than two members remain after assembly; {left_out}"
            ));
            return None;
        }
        Some((assembly, None))
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
        let receipt = match receipt {
            SubmissionOutcome::PullRequest(receipt) => receipt,
            SubmissionOutcome::AlreadyLanded(evidence) => {
                let completed = !self.cancellation.is_cancelled()
                    && matches!(
                        self.queue
                            .complete_already_landed(self.ctx, &member.candidate, &evidence)
                            .map_err(|error| {
                                (BatchExclusionReason::SubmissionFailed, error.to_string())
                            })?,
                        GenerationWrite::Applied(())
                    );
                return Err((
                    BatchExclusionReason::Superseded,
                    if completed {
                        "member completed: its head is already on the default branch"
                    } else {
                        "member authority changed before already-landed completion"
                    }
                    .into(),
                ));
            }
        };
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
}
