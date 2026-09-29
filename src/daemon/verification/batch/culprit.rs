//! How a bisection ends (SH-833; spec B7), after the batch observer: the
//! culprit goes back to its own agent, the certified members before it land
//! together, and every other member goes back to the queue.
//!
//! This runs after the observer on purpose: a culprit's return changes its
//! state, which the observer reads as lost authority, and a landing
//! admission makes members landing-pending (SH-832). A head culprit is
//! returned by the tick, which takes the probe's red verdict as the head's
//! own gate outcome (decision D7); any other culprit is returned here under
//! its own workspace lock, without a slot reservation (decision D8).

use super::attempt::Attempt;
use super::bisect::{Bisecting, update_parent};
use super::end::{retire, stopped_result};
use super::*;
use crate::daemon::verification::repair_return::{ReturnTransport, deliver_return, red_diagnosis};

/// Delivery under a batch member's own workspace lock: the helper refuses
/// any other story's lock (decision D8).
struct MemberTransport<'a> {
    batching: &'a dyn BatchActuator,
    lock: &'a WorkspaceLock,
    cancellation: &'a Cancellation,
}

impl ReturnTransport for MemberTransport<'_> {
    fn notify(
        &self,
        candidate: &VerificationCandidate,
        message: &str,
    ) -> Result<NotifyDelivery, AppError> {
        self.batching.notify_member(
            candidate,
            message,
            MemberOwner(self.lock),
            self.cancellation,
        )
    }

    fn redispatch(
        &self,
        candidate: &VerificationCandidate,
        plan: &ResumePlan,
    ) -> Result<(), AppError> {
        self.batching
            .redispatch_member(candidate, plan, MemberOwner(self.lock), self.cancellation)
    }
}

impl<S: Store> Attempt<'_, S> {
    /// Ends a bisection: stop, halt, no culprit, a head culprit, or a member
    /// culprit and the landing of the certified members before it.
    pub(super) fn finish_bisection(
        mut self,
        owner: &VerificationGuard,
        stale: Vec<String>,
        started: Instant,
    ) -> Result<BatchEnd, AppError> {
        let stopped = owner.is_cancelled();
        let mut bisecting = self
            .bisection
            .take()
            .expect("finish_bisection runs only for a bisection");
        // A probe a failed step left live.
        if let Some(orphan) = self.record.take() {
            self.end_probe(
                &orphan,
                BatchPhase::Abandoned,
                "the bisection step failed while this probe was live",
                None,
            );
        }
        let failure = self.failure.take();
        let mut summary = BatchSummary {
            id: Some(bisecting.parent.id.to_string()),
            members: bisecting
                .members
                .iter()
                .map(|member| member.candidate.story_id.clone())
                .collect(),
            phase: Some(BatchPhase::Released),
            verdict: Some(GateVerdict::TestsFailed),
            tree: bisecting
                .parent
                .gate
                .as_ref()
                .and_then(|gate| gate.tree.clone()),
            seconds: 0,
            detail: String::new(),
            bisection: None,
        };

        if let Some((outcome, _)) = bisecting.halt.take() {
            self.end_live_of(
                &mut bisecting,
                BatchPhase::Abandoned,
                "a probe gate could not clean up",
            );
            self.settle(
                &mut bisecting,
                "a probe gate could not clean up, so the queue halts and no member is returned",
            );
            summary.detail = "red; a bisection probe could not clean up".into();
            let ended = self.close(bisecting, &mut summary, started, true);
            return self.halt(&ended.id, &outcome, summary);
        }

        if stopped {
            self.end_live_of(
                &mut bisecting,
                BatchPhase::Abandoned,
                "an operator stopped the attempt",
            );
            self.settle_interrupted(&mut bisecting, "an operator stopped the verifier");
            summary.detail = "red; an operator stopped its bisection".into();
            self.close(bisecting, &mut summary, started, false);
            // The batch gate ran, so the head's attempt was interrupted.
            record_generation_interrupted(
                self.queue,
                self.ctx,
                self.env,
                self.head,
                &owner.active,
            )?;
            return Ok(BatchEnd::Tick {
                result: stopped_result(self.queue, self.head)?,
                outcome: Box::new(VerificationOutcome::Cancelled),
                summary,
            });
        }

        let Some((position, certified)) = bisecting.found else {
            let why = bisecting
                .stopped
                .clone()
                .or(failure)
                .or_else(|| {
                    (!stale.is_empty())
                        .then(|| format!("{} changed during the bisection", stale.join(", ")))
                })
                .unwrap_or_else(|| "the search ended without a verdict".into());
            let phase = if stale.is_empty() {
                BatchPhase::Released
            } else {
                BatchPhase::Abandoned
            };
            self.end_live_of(&mut bisecting, phase, &why);
            self.settle(&mut bisecting, &why);
            summary.detail = format!("red; bisection blamed no member: {why}");
            self.close(bisecting, &mut summary, started, true);
            return Ok(BatchEnd::Done(summary));
        };

        if position == 1 {
            return self.head_culprit(bisecting, summary, started);
        }
        self.member_culprit(
            owner, bisecting, summary, stale, started, position, certified,
        )
    }

    /// The head is the culprit: the tick returns it with the probe's red
    /// verdict as its own gate outcome (decision D7), unless a project
    /// recovery started meanwhile, which needs the head's own admitted gate.
    fn head_culprit(
        &mut self,
        mut bisecting: Bisecting,
        mut summary: BatchSummary,
        started: Instant,
    ) -> Result<BatchEnd, AppError> {
        let recovering = self.store.read(|tx| {
            Ok(tx
                .project_recoveries(self.head.project)?
                .iter()
                .any(|recovery| recovery.active))
        })?;
        let red = bisecting.reds.get(&1).cloned();
        let (Some(red), false) = (red, recovering) else {
            let why =
                "a project recovery started during the bisection, so the head takes its own gate";
            self.found_detail(&mut bisecting, why);
            summary.detail = format!("red; {why}");
            self.close(bisecting, &mut summary, started, true);
            return Ok(BatchEnd::Done(summary));
        };
        let found_by = found_by(&bisecting, 1, 0);
        self.found_detail(
            &mut bisecting,
            "returned by the head's own attempt with this red verdict",
        );
        summary.detail = format!("red; bisection returns the head {}", self.head.story_id);
        self.close(bisecting, &mut summary, started, true);
        Ok(BatchEnd::HeadRed {
            outcome: Box::new(VerificationOutcome::TestsFailed {
                tree: red.tree,
                log: red.log,
                detail: red.detail,
                gate: red.gate,
            }),
            found_by,
            summary,
        })
    }

    /// A member is the culprit: return it to its own agent, then land the
    /// certified members before it (two or more through their live probe
    /// batch; the head alone through its own gate, which reuses the
    /// prefix's receipt).
    #[allow(clippy::too_many_arguments)]
    fn member_culprit(
        &mut self,
        owner: &VerificationGuard,
        mut bisecting: Bisecting,
        mut summary: BatchSummary,
        stale: Vec<String>,
        started: Instant,
        position: usize,
        certified: usize,
    ) -> Result<BatchEnd, AppError> {
        let culprit = bisecting.members[position - 1].candidate.clone();
        let red = bisecting.reds.get(&position).cloned().ok_or_else(|| {
            AppError::Storage(format!(
                "verification batch {}: no red verdict is recorded for prefix {position}",
                bisecting.parent.id
            ))
        })?;
        let diagnosis = red_diagnosis(
            &culprit,
            &red.tree,
            &red.gate,
            &red.log,
            &red.detail,
            Some(&found_by(&bisecting, position, certified)),
        );
        progress_item(self.env, self.head, "bisection culprit return", "running");
        let returned = self.return_member(owner, &culprit, &diagnosis);
        progress_item(
            self.env,
            self.head,
            "bisection culprit return",
            if returned.starts_with("returned") {
                "passed"
            } else {
                "failed"
            },
        );
        journal(
            "INFO",
            self.head,
            &format!(
                "verification batch {}: culprit {}: {returned}",
                bisecting.parent.id, culprit.story_id
            ),
        );
        if let Some(locks) = self.locks.as_mut() {
            locks.release(&culprit.story_id);
        }
        self.found_detail(&mut bisecting, &returned);
        summary.detail = format!("red; bisection found {}: {returned}", culprit.story_id);

        let live = bisecting.live.take();
        let refused = bisecting.landing_refused.clone();
        match live {
            Some(live) if stale.is_empty() && refused.is_none() => {
                // Only the members that land keep their locks.
                for member in &bisecting.members[certified..] {
                    if let Some(locks) = self.locks.as_mut() {
                        locks.release(&member.candidate.story_id);
                    }
                }
                let mut landing_summary = BatchSummary {
                    phase: Some(BatchPhase::Landing),
                    detail: format!(
                        "red; bisection returned {}; its first {certified} members land as batch {}",
                        culprit.story_id, live.record.id
                    ),
                    ..summary.clone()
                };
                let ended = self.close(bisecting, &mut landing_summary, started, true);
                match self.admit(&live.record, &live.outcome, live.seconds, landing_summary) {
                    Ok(landing) => return Ok(BatchEnd::Land(Box::new(landing))),
                    Err(why) => {
                        let gate = super::bisect::probe_gate(&live.outcome, live.seconds);
                        self.end_probe(
                            &live.record,
                            BatchPhase::Released,
                            &format!("certified, but its landing was refused: {why}"),
                            Some(gate),
                        );
                        summary.detail = format!(
                            "{}; the certified members did not land: {why}",
                            summary.detail
                        );
                        summary.bisection = ended.bisection;
                        summary.seconds = started.elapsed().as_secs();
                        return Ok(BatchEnd::Done(summary));
                    }
                }
            }
            Some(live) => {
                let why = refused
                    .unwrap_or_else(|| format!("{} changed after the bisection", stale.join(", ")));
                let gate = super::bisect::probe_gate(&live.outcome, live.seconds);
                self.end_probe(
                    &live.record,
                    BatchPhase::Abandoned,
                    &format!("certified, but it does not land: {why}"),
                    Some(gate),
                );
                summary.detail = format!(
                    "{}; the certified members did not land: {why}",
                    summary.detail
                );
            }
            None => {
                if let Some(why) = refused {
                    summary.detail = format!(
                        "{}; the certified members did not land: {why}",
                        summary.detail
                    );
                }
            }
        }
        self.close(bisecting, &mut summary, started, true);
        Ok(BatchEnd::Done(summary))
    }

    /// Returns the member culprit to its own agent: the transition and RED
    /// comment, then delivery under its own lock. Answers what happened, as
    /// an operator reads it; never fails the tick.
    fn return_member(
        &self,
        owner: &VerificationGuard,
        culprit: &VerificationCandidate,
        diagnosis: &str,
    ) -> String {
        let attempt = || -> Result<String, AppError> {
            if owner.is_cancelled() {
                return Ok("not returned: the verifier was stopped".into());
            }
            if !self.queue.human_permits(culprit)? {
                return Ok("not returned: a person holds it".into());
            }
            let Some(lock) = self
                .locks
                .as_ref()
                .and_then(|locks| locks.get(&culprit.story_id))
            else {
                return Ok("not returned: the batch no longer holds its workspace lock".into());
            };
            if matches!(
                self.queue
                    .record_generation_returned(self.ctx, culprit, diagnosis)?,
                GenerationWrite::Superseded
            ) {
                return Ok("not returned: it changed after the bisection found it".into());
            }
            let transport = MemberTransport {
                batching: self.batching,
                lock,
                cancellation: &owner.cancellation,
            };
            Ok(
                match deliver_return(
                    self.queue,
                    self.ctx,
                    &transport,
                    culprit,
                    diagnosis,
                    &owner.cancellation,
                )? {
                    GenerationWrite::Applied(true) => "returned to its agent".into(),
                    GenerationWrite::Applied(false) => {
                        "returned, but its diagnosis did not reach a live agent; its comments say why"
                            .into()
                    }
                    GenerationWrite::Superseded => {
                        "returned; it changed while its diagnosis was delivered".into()
                    }
                },
            )
        };
        attempt().unwrap_or_else(|error| {
            journal(
                "ERROR",
                self.head,
                &format!("the return of culprit {} failed: {error}", culprit.story_id),
            );
            format!("the return failed: {error}")
        })
    }

    /// Ends the live certified probe, if any, in `phase`.
    fn end_live_of(&mut self, bisecting: &mut Bisecting, phase: BatchPhase, why: &str) {
        if let Some(live) = bisecting.live.take() {
            let gate = super::bisect::probe_gate(&live.outcome, live.seconds);
            self.end_probe(&live.record, phase, why, Some(gate));
        }
    }

    /// Records how a bisection with no culprit ended, unless a culprit was
    /// already recorded (then `why` joins its detail).
    fn settle(&self, bisecting: &mut Bisecting, why: &str) {
        if bisecting.found.is_some() {
            self.found_detail(bisecting, why);
            return;
        }
        self.write_outcome(
            bisecting,
            BisectionOutcome::Inconclusive {
                detail: why.to_owned(),
            },
        );
    }

    /// Records an interrupted bisection, keeping a recorded culprit.
    fn settle_interrupted(&self, bisecting: &mut Bisecting, why: &str) {
        if bisecting.found.is_some() {
            self.found_detail(bisecting, &format!("not returned: {why}"));
            return;
        }
        self.write_outcome(
            bisecting,
            BisectionOutcome::Interrupted {
                detail: why.to_owned(),
            },
        );
    }

    /// Sets the recorded culprit's detail to what the verifier did.
    fn found_detail(&self, bisecting: &mut Bisecting, what: &str) {
        let result = update_parent(self.store, self.env, &mut bisecting.parent, |batch| {
            if let Some(BisectionOutcome::Culprit { detail, .. }) = batch
                .bisection
                .as_mut()
                .and_then(|bisection| bisection.outcome.as_mut())
            {
                what.clone_into(detail);
            }
        });
        if let Err(error) = result {
            self.journal_unrecorded(&bisecting.parent.id, &error);
        }
    }

    fn write_outcome(&self, bisecting: &mut Bisecting, outcome: BisectionOutcome) {
        let result = update_parent(self.store, self.env, &mut bisecting.parent, |batch| {
            if let Some(bisection) = batch.bisection.as_mut() {
                bisection.outcome = Some(outcome);
            }
        });
        if let Err(error) = result {
            self.journal_unrecorded(&bisecting.parent.id, &error);
        }
    }

    /// A bisection end the store refused stays unfinished: the next worker
    /// start or batch settles it as interrupted.
    fn journal_unrecorded(&self, batch: &BatchId, error: &AppError) {
        journal(
            "ERROR",
            self.head,
            &format!(
                "verification batch {batch}: the end of its bisection is not recorded: {error}"
            ),
        );
    }

    /// Retires the red parent (unless `retire_now` is false: an operator
    /// stop leaves retirement to the next batch) and completes `summary`
    /// with its bisection. Answers the parent as last written before its
    /// retirement.
    fn close(
        &self,
        bisecting: Bisecting,
        summary: &mut BatchSummary,
        started: Instant,
        retire_now: bool,
    ) -> VerificationBatch {
        let parent = bisecting.parent;
        journal(
            "INFO",
            self.head,
            &format!(
                "verification batch {} bisection ended after {} search gates: {}",
                parent.id, bisecting.runs, summary.detail
            ),
        );
        summary.bisection = parent.bisection.clone();
        summary.seconds = started.elapsed().as_secs();
        if retire_now {
            retire(self.store, self.env, self.batching, self.head, &parent);
        }
        parent
    }
}

/// The sentence the culprit's RED comment names its batch with: the red
/// batch, the members merged before the culprit and the tree they passed
/// as, and the red tree (the acceptance of SH-833).
pub(super) fn found_by(bisecting: &Bisecting, position: usize, certified: usize) -> String {
    let parent = &bisecting.parent;
    let all = bisecting.ids(0..bisecting.members.len()).join(", ");
    let culprit = &bisecting.members[position - 1].candidate.story_id;
    let mut text = format!(
        "Verification batch {} ({all}) failed its gate, and bisection found that {culprit} turns it red:",
        parent.id
    );
    if certified == 0 {
        text.push_str(&format!(
            " {culprit} merged alone onto `{}` at {} failed.",
            parent.base_branch, parent.base_commit
        ));
    } else {
        let before = bisecting.ids(0..certified).join(", ");
        let green = &bisecting.chain[certified - 1].1;
        let how = match bisecting.greens.get(&certified) {
            Some(crate::store::ProbeKind::Receipt) => "a gate receipt certifies them as",
            _ => "they passed their gate as",
        };
        text.push_str(&format!(
            " {before} merged before it, {how} tree `{green}`, and the merge of {culprit} onto them failed."
        ));
    }
    if position == bisecting.search.members() {
        text.push_str(
            " This verdict rests on one gate run of the whole batch; if the failure has no link to this story, the test may be flaky.",
        );
    }
    if let Some(member) = parent.members.get(position - 1)
        && let Some(resolution) = &member.resolution
    {
        // SH-834 D7: the red tree holds a union no member wrote as one.
        let files: Vec<String> = resolution
            .files
            .iter()
            .map(|file| format!("`{}`", file.path))
            .collect();
        let with = if resolution.conflicted_with.is_empty() {
            "an earlier member".to_owned()
        } else {
            resolution.conflicted_with.join(", ")
        };
        text.push_str(&format!(
            " {culprit} joined the batch through an automated conflict resolution ({}) of {} with {with}, in merge commit {}: the red may come from that resolution, not from {culprit}'s own change. After the members before it land, merge `{}` into this branch, resolve those files yourself, and submit again.",
            resolution.strategy,
            files.join(", "),
            member.merge_commit.as_deref().unwrap_or("of the batch tip"),
            parent.base_branch,
        ));
    }
    text
}
