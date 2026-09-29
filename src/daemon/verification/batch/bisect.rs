//! Bisecting a red batch (SH-833; spec B7): the probes, run inside the
//! batch's observed steps so that a member change or a stop cancels them as
//! it cancels the batch gate.
//!
//! The search is [`PrefixBisection`] over the batch's merge chain. Prefix 1
//! is gated as the head's own pull request, whose merge tree is the first
//! merge commit's; a longer prefix is a probe batch of its own (members
//! `1..=j`, tip `Pj`), published and gated by the batch code. A verdict
//! counts only for the exact prefix tree (decision D9): anything else stops
//! the search and blames nobody. The red parent ends `released` before the
//! first probe exists, because a project has one live batch, and records
//! every probe and the outcome in its `bisection` (decision D6).

use super::attempt::Attempt;
use super::end::{outcome_detail, retire, write};
use super::record::{Backstop, advance_record, publication, pull_request_link, store_new};
use super::*;
use crate::domain::prefix_bisection::{PrefixBisection, ProbeVerdict, Step};
use crate::store::{BisectionOf, BisectionProbe, ProbeKind};
use std::collections::BTreeMap;

/// Why the red batch record ends `released` while bisection starts.
const BISECTING_DETAIL: &str = "gated red; bisecting to find the member that turns it red";

/// The evidence of one red prefix tree.
#[derive(Clone, Debug)]
pub(super) struct Red {
    /// The red tree.
    pub(super) tree: String,
    /// The gate's full log.
    pub(super) log: String,
    /// The gate command.
    pub(super) gate: String,
    /// The gate's own words.
    pub(super) detail: String,
}

/// A certified probe batch that is still live: the prefix that lands.
pub(super) struct LiveProbe {
    /// How many leading members it merges.
    pub(super) prefix: usize,
    /// Its record, in `gating`.
    pub(super) record: VerificationBatch,
    /// Its certified gate.
    pub(super) outcome: VerificationOutcome,
    /// How long that gate ran.
    pub(super) seconds: u64,
}

/// A red batch being bisected.
pub(super) struct Bisecting {
    /// The red batch, ended `released`, as last written.
    pub(super) parent: VerificationBatch,
    /// The red batch's members in queue order, head first.
    pub(super) members: Vec<Planned>,
    /// Each prefix's tip and tree.
    pub(super) chain: Vec<(String, String)>,
    /// The search.
    pub(super) search: PrefixBisection,
    /// Red evidence by prefix; the whole batch's is at its size.
    pub(super) reds: BTreeMap<usize, Red>,
    /// How each green prefix was certified.
    pub(super) greens: BTreeMap<usize, ProbeKind>,
    /// The certified probe batch that lands, while it is live.
    pub(super) live: Option<LiveProbe>,
    /// The culprit's 1-based position and the certified prefix, once the
    /// finding is recorded.
    pub(super) found: Option<(usize, usize)>,
    /// Why the search stopped without a culprit it could record.
    pub(super) stopped: Option<String>,
    /// Why the certified prefix cannot land, when it cannot.
    pub(super) landing_refused: Option<String>,
    /// A probe gate that could not clean up: the queue halts.
    pub(super) halt: Option<(VerificationOutcome, u64)>,
    /// Search gates run.
    pub(super) runs: u32,
}

impl Bisecting {
    /// The ids of the members in `range` (0-based positions).
    pub(super) fn ids(&self, range: std::ops::Range<usize>) -> Vec<String> {
        self.members[range]
            .iter()
            .map(|member| member.candidate.story_id.clone())
            .collect()
    }
}

/// What one probe found.
pub(super) enum Probe {
    /// The prefix tree passed; a probe batch that did stays live.
    Green(Option<LiveProbe>),
    /// The prefix tree failed its tests.
    Red(Red),
    /// No verdict that may count, and why.
    Void(String),
    /// The gate could not clean up.
    Halt(VerificationOutcome, u64),
}

/// Re-reads `parent`, applies `change` and writes it in the same phase:
/// an ended record may still record its bisection. `parent` becomes the
/// record as written.
pub(super) fn update_parent(
    store: &impl Store,
    env: &Environment,
    parent: &mut VerificationBatch,
    change: impl FnOnce(&mut VerificationBatch),
) -> Result<(), AppError> {
    let current = store
        .read(|tx| tx.verification_batches(parent.project))?
        .into_iter()
        .find(|batch| batch.id == parent.id)
        .ok_or_else(|| AppError::Storage(format!("verification batch {} is gone", parent.id)))?;
    let mut next = current.clone();
    change(&mut next);
    next.revision = current.revision + 1;
    next.updated_at = env.now();
    write(store, &next, current.revision)?;
    *parent = next;
    Ok(())
}

/// Judges one probe's gate against the prefix it gated (decision D9).
/// `head` is the commit the gate must have certified: the head's pushed
/// head for prefix 1, else the prefix's merge commit.
pub(super) fn classify(
    outcome: &VerificationOutcome,
    tree: &str,
    head: &str,
    kind: ProbeKind,
    seconds: u64,
) -> Probe {
    match outcome {
        VerificationOutcome::Certified {
            head: certified,
            tree: judged,
            ..
        } if certified == head && (judged == tree || kind == ProbeKind::Landing) => {
            Probe::Green(None)
        }
        VerificationOutcome::Certified {
            head: certified,
            tree: judged,
            ..
        } => Probe::Void(format!(
            "the gate certified {certified} as tree {judged}, not the prefix {head} as tree {tree}: the base or a head moved, so the verdict does not count"
        )),
        VerificationOutcome::TestsFailed {
            tree: judged,
            log,
            detail,
            gate,
        } if judged == tree && kind == ProbeKind::Search => Probe::Red(Red {
            tree: judged.clone(),
            log: log.clone(),
            gate: gate.clone(),
            detail: detail.clone(),
        }),
        VerificationOutcome::TestsFailed { tree: judged, .. } => Probe::Void(format!(
            "the gate failed tree {judged} where the prefix tree is {tree}; no story is blamed for it"
        )),
        VerificationOutcome::CleanupFailed { .. } => Probe::Halt(outcome.clone(), seconds),
        VerificationOutcome::Cancelled => Probe::Void("the gate was stopped".into()),
        other => Probe::Void(format!(
            "the gate ended {} and judged nothing: {}",
            GateVerdict::of(&Ok(Some(other.clone())), false).as_str(),
            outcome_detail(other)
        )),
    }
}

impl<S: Store> Attempt<'_, S> {
    /// Bisects the batch when its gate judged the batch tip's tree red and
    /// nothing cancelled it; otherwise does nothing.
    pub(super) fn bisect(&mut self) -> Result<(), AppError> {
        let Some((outcome, seconds)) = self.gate.clone() else {
            return Ok(());
        };
        let VerificationOutcome::TestsFailed {
            tree,
            log,
            detail,
            gate,
        } = outcome.clone()
        else {
            return Ok(());
        };
        if self.cancellation.is_cancelled() {
            return Ok(());
        }
        let Some(record) = self.record.clone() else {
            return Ok(());
        };
        let Some(chain) = record.merge_chain() else {
            return Ok(());
        };
        if chain.last().map(|(_, tip_tree)| tip_tree.as_str()) != Some(tree.as_str()) {
            journal(
                "INFO",
                self.head,
                &format!(
                    "verification batch {} is red on tree {tree}, not on its tip's tree: the base moved, so it is not bisected",
                    record.id
                ),
            );
            return Ok(());
        }
        let Some(members) = record
            .members
            .iter()
            .map(|member| {
                self.plan
                    .members
                    .iter()
                    .find(|planned| planned.candidate.story_id == member.story_id)
                    .cloned()
            })
            .collect::<Option<Vec<_>>>()
        else {
            return Ok(());
        };
        let search = PrefixBisection::new(members.len())
            .map_err(|error| AppError::Validation(error.to_string()))?;
        let red_gate = BatchGate {
            verdict: GateVerdict::TestsFailed,
            tree: Some(tree.clone()),
            detail: outcome_detail(&outcome),
            seconds,
        };
        let parent = advance_record(
            self.store,
            self.env,
            self.head,
            &record,
            BatchPhase::Released,
            |batch| {
                batch.gate = Some(red_gate);
                batch.detail = Some(BISECTING_DETAIL.into());
                batch.bisection = Some(BatchBisection::default());
            },
        )?;
        self.record = None;
        journal(
            "INFO",
            self.head,
            &format!(
                "verification batch {} is red; bisecting its {} members",
                parent.id,
                members.len()
            ),
        );
        let size = members.len();
        self.bisection = Some(Bisecting {
            parent,
            members,
            chain,
            search,
            reds: BTreeMap::from([(
                size,
                Red {
                    tree,
                    log,
                    gate,
                    detail,
                },
            )]),
            greens: BTreeMap::new(),
            live: None,
            found: None,
            stopped: None,
            landing_refused: None,
            halt: None,
            runs: 0,
        });
        self.use_receipts();
        self.search()
    }

    fn bisecting(&mut self) -> &mut Bisecting {
        self.bisection
            .as_mut()
            .expect("the bisection is recorded before it is searched")
    }

    /// Raises the green prefix to the longest prefix whose tree already
    /// carries a qualifying receipt, largest first (decision D4). A receipt
    /// that cannot be read is journaled and costs only a gate.
    fn use_receipts(&mut self) {
        let (base, chain, id) = {
            let bisecting = self.bisecting();
            (
                bisecting.parent.base_commit.clone(),
                bisecting.chain.clone(),
                bisecting.parent.id.clone(),
            )
        };
        for prefix in (1..chain.len()).rev() {
            if self.cancellation.is_cancelled() {
                return;
            }
            let (commit, tree) = &chain[prefix - 1];
            match self
                .batching
                .certified(self.head, &base, commit, self.cancellation)
            {
                Ok(true) => {
                    if self.bisecting().search.certified(prefix) {
                        self.bisecting().greens.insert(prefix, ProbeKind::Receipt);
                        self.note(BisectionProbe {
                            prefix: prefix as u32,
                            kind: ProbeKind::Receipt,
                            batch: None,
                            tree: tree.clone(),
                            verdict: GateVerdict::Certified,
                            log: None,
                            detail: "a qualifying gate receipt already certifies this tree".into(),
                            seconds: 0,
                        });
                    }
                    return;
                }
                Ok(false) => {}
                Err(error) => journal(
                    "ERROR",
                    self.head,
                    &format!(
                        "verification batch {id}: the receipt of prefix {prefix} (tree {tree}) could not be read, so the search gates that prefix if it must: {error}"
                    ),
                ),
            }
        }
    }

    /// Runs the search until it finds the culprit, or something stops it.
    fn search(&mut self) -> Result<(), AppError> {
        loop {
            if self.cancellation.is_cancelled() {
                return Ok(());
            }
            let bisecting = self.bisecting();
            if bisecting.stopped.is_some() || bisecting.halt.is_some() {
                return Ok(());
            }
            let prefix = match bisecting.search.next() {
                Step::Culprit {
                    position,
                    certified,
                } => return self.found(position, certified),
                Step::Probe(prefix) => prefix,
            };
            self.end_live("certified, but the search gates a longer prefix next");
            match self.probe(prefix, ProbeKind::Search)? {
                Probe::Green(live) => {
                    let bisecting = self.bisecting();
                    bisecting.runs += 1;
                    record(&mut bisecting.search, prefix, ProbeVerdict::Green)?;
                    bisecting.greens.insert(prefix, ProbeKind::Search);
                    // A certified probe stays live only when it ends the
                    // search: it is the prefix that lands (decision D10).
                    let ends_here = matches!(
                        bisecting.search.next(),
                        Step::Culprit { certified, .. } if certified == prefix
                    );
                    if ends_here {
                        bisecting.live = live;
                    } else if let Some(live) = live {
                        let gate = probe_gate(&live.outcome, live.seconds);
                        self.end_probe(
                            &live.record,
                            BatchPhase::Released,
                            "certified, but the search goes on with a longer prefix",
                            Some(gate),
                        );
                    }
                }
                Probe::Red(red) => {
                    let bisecting = self.bisecting();
                    bisecting.runs += 1;
                    record(&mut bisecting.search, prefix, ProbeVerdict::Red)?;
                    bisecting.reds.insert(prefix, red);
                    self.narrow(prefix);
                }
                Probe::Void(why) => {
                    self.bisecting().stopped = Some(format!("prefix {prefix}: {why}"));
                    return Ok(());
                }
                Probe::Halt(outcome, seconds) => {
                    self.bisecting().halt = Some((outcome, seconds));
                    return Ok(());
                }
            }
        }
    }

    /// Records the culprit, freezes it, and makes sure the certified prefix
    /// has a live, certified probe batch to land (decision D10).
    fn found(&mut self, position: usize, certified: usize) -> Result<(), AppError> {
        let (culprit, red) = {
            let bisecting = self.bisecting();
            let red = bisecting.reds.get(&position).cloned().ok_or_else(|| {
                AppError::Storage(format!(
                    "verification batch {}: no red verdict is recorded for prefix {position}",
                    bisecting.parent.id
                ))
            })?;
            (
                bisecting.members[position - 1].candidate.story_id.clone(),
                red,
            )
        };
        let (store, env) = (self.store, self.env);
        let written = update_parent(store, env, &mut self.bisecting().parent, |batch| {
            if let Some(bisection) = batch.bisection.as_mut() {
                bisection.outcome = Some(BisectionOutcome::Culprit {
                    story_id: culprit.clone(),
                    position: position as u32,
                    tree: red.tree.clone(),
                    log: red.log.clone(),
                    certified: certified as u32,
                    detail: "found; the verifier returns it to its agent".into(),
                });
            }
        });
        if let Err(error) = written {
            // No blame without a durable record of it.
            self.bisecting().stopped = Some(format!(
                "{culprit} was found but the finding could not be recorded, so it is not returned: {error}"
            ));
            return Ok(());
        }
        self.bisecting().found = Some((position, certified));
        let parent = self.bisecting().parent.id.clone();
        journal(
            "INFO",
            self.head,
            &format!(
                "verification batch {parent}: bisection found {culprit} at position {position}; {certified} leading members are certified"
            ),
        );
        // Frozen: a later change to the culprit is its own return's to meet
        // (the return is refused for a changed generation), and never stops
        // the landing of the members before it.
        if position > 1 {
            self.leave(&culprit, false);
        }
        let live = self.bisecting().live.as_ref().map(|live| live.prefix);
        if certified < 2 || live == Some(certified) || self.cancellation.is_cancelled() {
            return Ok(());
        }
        self.end_live("certified, but a fresh probe of the certified prefix lands instead");
        match self.probe(certified, ProbeKind::Landing)? {
            Probe::Green(live) => self.bisecting().live = live,
            Probe::Red(red) => {
                self.bisecting().landing_refused = Some(format!(
                    "the certified prefix failed its landing gate as tree {}",
                    red.tree
                ));
            }
            Probe::Void(why) => self.bisecting().landing_refused = Some(why),
            Probe::Halt(outcome, seconds) => self.bisecting().halt = Some((outcome, seconds)),
        }
        Ok(())
    }

    /// Gates the tree of the first `prefix` members.
    fn probe(&mut self, prefix: usize, kind: ProbeKind) -> Result<Probe, AppError> {
        let (parent, commit, tree, head_commit) = {
            let bisecting = self.bisecting();
            let (commit, tree) = bisecting.chain[prefix - 1].clone();
            (
                bisecting.parent.id.clone(),
                commit,
                tree,
                bisecting.members[0].commit.clone(),
            )
        };
        if let Some(membership) = &self.membership {
            membership.show_bisecting(&parent);
        }
        let item = format!("bisection prefix {prefix}");
        progress_item(self.env, self.head, &item, "running");
        let probe = if prefix == 1 {
            self.probe_head(&tree, &head_commit, kind)
        } else {
            self.probe_batch(prefix, &commit, &tree, kind)?
        };
        let status = if matches!(probe, Probe::Green(_)) {
            "passed"
        } else {
            "failed"
        };
        progress_item(self.env, self.head, &item, status);
        Ok(probe)
    }

    /// Gates the head alone: its own pull request, whose merge tree is the
    /// first prefix's.
    fn probe_head(&mut self, tree: &str, head_commit: &str, kind: ProbeKind) -> Probe {
        let link = match &self.head.pull_request {
            Ok(link) => link.clone(),
            Err(problem) => {
                return Probe::Void(format!(
                    "the head has no pull request to gate alone: {}",
                    problem.message()
                ));
            }
        };
        let started = Instant::now();
        let outcome = self.batching.gate(self.head, &link, self.cancellation);
        let seconds = started.elapsed().as_secs();
        let probe = classify(&outcome, tree, head_commit, kind, seconds);
        self.note_gate(1, kind, None, tree, &outcome, &probe, seconds);
        probe
    }

    /// Records, publishes and gates a probe batch of the first `prefix`
    /// members, at their merge commit `commit`.
    fn probe_batch(
        &mut self,
        prefix: usize,
        commit: &str,
        tree: &str,
        kind: ProbeKind,
    ) -> Result<Probe, AppError> {
        let (parent, members) = {
            let bisecting = self.bisecting();
            (
                bisecting.parent.id.clone(),
                bisecting.parent.members[..prefix].to_vec(),
            )
        };
        let mut child =
            self.new_record(BatchId::generate(), commit.to_owned(), members, Vec::new());
        child.bisects = Some(BisectionOf {
            parent: parent.clone(),
            prefix: prefix as u32,
        });
        store_new(self.store, &child, Backstop::AbandonLive)?;
        // A live probe is the attempt's live record until it ends.
        self.record = Some(child.clone());
        journal(
            "INFO",
            self.head,
            &format!(
                "verification batch {} probes prefix {prefix} of batch {parent} at {commit}",
                child.id
            ),
        );
        let pull_request =
            match self
                .batching
                .publish(self.head, &publication(&child), self.cancellation)
            {
                Ok(pull_request) => pull_request,
                Err(error) => {
                    let why = format!("publishing probe batch {} failed: {error}", child.id);
                    self.end_probe(&child, BatchPhase::Abandoned, &why, None);
                    return Ok(Probe::Void(why));
                }
            };
        child = advance_record(
            self.store,
            self.env,
            self.head,
            &child,
            BatchPhase::Submitted,
            |batch| batch.pull_request = Some(pull_request),
        )?;
        self.record = Some(child.clone());
        if self.cancellation.is_cancelled() {
            self.end_probe(
                &child,
                BatchPhase::Abandoned,
                "stopped before its gate",
                None,
            );
            return Ok(Probe::Void("stopped before its gate".into()));
        }
        child = advance_record(
            self.store,
            self.env,
            self.head,
            &child,
            BatchPhase::Gating,
            |_| {},
        )?;
        self.record = Some(child.clone());
        let link = pull_request_link(&child, self.env.now())?;
        let started = Instant::now();
        let outcome = self.batching.gate(self.head, &link, self.cancellation);
        let seconds = started.elapsed().as_secs();
        let probe = classify(&outcome, tree, commit, kind, seconds);
        self.note_gate(
            prefix,
            kind,
            Some(child.id.clone()),
            tree,
            &outcome,
            &probe,
            seconds,
        );
        Ok(match probe {
            // Unchanged, as written: its landing admission compares it with
            // the stored record.
            Probe::Green(_) => {
                self.record = None;
                Probe::Green(Some(LiveProbe {
                    prefix,
                    record: child,
                    outcome,
                    seconds,
                }))
            }
            other => {
                let (phase, why) = match &other {
                    Probe::Red(red) => (
                        BatchPhase::Released,
                        format!(
                            "red as tree {}; a bisection probe never lands red",
                            red.tree
                        ),
                    ),
                    Probe::Void(why) if self.cancellation.is_cancelled() => {
                        (BatchPhase::Abandoned, why.clone())
                    }
                    Probe::Void(why) => (BatchPhase::Released, why.clone()),
                    _ => (
                        BatchPhase::Released,
                        "its gate could not clean up".to_owned(),
                    ),
                };
                self.end_probe(&child, phase, &why, Some(probe_gate(&outcome, seconds)));
                other
            }
        })
    }

    /// Records one gate of a probe on the parent's bisection.
    #[allow(clippy::too_many_arguments)]
    fn note_gate(
        &mut self,
        prefix: usize,
        kind: ProbeKind,
        batch: Option<BatchId>,
        tree: &str,
        outcome: &VerificationOutcome,
        probe: &Probe,
        seconds: u64,
    ) {
        let log = match outcome {
            VerificationOutcome::TestsFailed { log, .. } => Some(log.clone()),
            _ => None,
        };
        let detail = match probe {
            Probe::Void(why) => why.clone(),
            _ => outcome_detail(outcome),
        };
        self.note(BisectionProbe {
            prefix: prefix as u32,
            kind,
            batch,
            tree: tree.to_owned(),
            verdict: GateVerdict::of(&Ok(Some(outcome.clone())), false),
            log,
            detail,
            seconds,
        });
    }

    /// Appends `probe` to the parent's bisection. A record that cannot be
    /// written stops the search: no story is blamed on an unrecorded search.
    fn note(&mut self, probe: BisectionProbe) {
        let (store, env) = (self.store, self.env);
        let written = update_parent(store, env, &mut self.bisecting().parent, |batch| {
            if let Some(bisection) = batch.bisection.as_mut() {
                bisection.probes.push(probe);
            }
        });
        if let Err(error) = written {
            self.bisecting().stopped =
                Some(format!("the bisection could not be recorded: {error}"));
        }
    }

    /// Ends the live certified probe, if any, with `why`.
    pub(super) fn end_live(&mut self, why: &str) {
        if let Some(live) = self.bisection.as_mut().and_then(|b| b.live.take()) {
            let gate = probe_gate(&live.outcome, live.seconds);
            self.end_probe(&live.record, BatchPhase::Released, why, Some(gate));
        }
    }

    /// Ends probe batch `record` in `phase` with `why` and its `gate`, and
    /// retires it unless the batch was cancelled (the next batch retires
    /// what is left). Best effort: a failure is journaled.
    pub(super) fn end_probe(
        &mut self,
        record: &VerificationBatch,
        phase: BatchPhase,
        why: &str,
        gate: Option<BatchGate>,
    ) {
        if self
            .record
            .as_ref()
            .is_some_and(|live| live.id == record.id)
        {
            self.record = None;
        }
        match advance_record(self.store, self.env, self.head, record, phase, |batch| {
            batch.detail = Some(why.to_owned());
            if gate.is_some() {
                batch.gate = gate;
            }
        }) {
            Ok(ended) => {
                if !self.cancellation.is_cancelled() {
                    retire(self.store, self.env, self.batching, self.head, &ended);
                }
            }
            Err(error) => journal(
                "ERROR",
                self.head,
                &format!(
                    "bisection probe batch {} could not record its end ({why}): {error}",
                    record.id
                ),
            ),
        }
    }

    /// Takes the members after the red prefix out of play: they go back to
    /// the queue at their existing age, free to change.
    fn narrow(&mut self, red: usize) {
        let leaving = {
            let bisecting = self.bisecting();
            bisecting.ids(red..bisecting.members.len())
        };
        for story_id in leaving {
            self.leave(&story_id, true);
        }
    }

    /// Stops observing `story_id` and lists it no longer as running; with
    /// `release`, frees its workspace lock too.
    pub(super) fn leave(&mut self, story_id: &str, release: bool) {
        self.tracked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|tracked| tracked.story_id != story_id);
        if let Some(membership) = &self.membership {
            membership.leave(story_id);
        }
        if release && let Some(locks) = self.locks.as_mut() {
            locks.release(story_id);
        }
    }
}

/// Records `verdict` on `prefix`, which the search itself proposed.
fn record(
    search: &mut PrefixBisection,
    prefix: usize,
    verdict: ProbeVerdict,
) -> Result<(), AppError> {
    search
        .record(prefix, verdict)
        .map_err(|error| AppError::Storage(format!("bisection: {error}")))
}

/// A probe batch's gate as its record keeps it.
pub(super) fn probe_gate(outcome: &VerificationOutcome, seconds: u64) -> BatchGate {
    BatchGate {
        verdict: GateVerdict::of(&Ok(Some(outcome.clone())), false),
        tree: gated_tree(outcome),
        detail: outcome_detail(outcome),
        seconds,
    }
}

/// The tree a probe gate judged, when it judged one.
fn gated_tree(outcome: &VerificationOutcome) -> Option<String> {
    match outcome {
        VerificationOutcome::Certified { tree, .. }
        | VerificationOutcome::TestsFailed { tree, .. } => Some(tree.clone()),
        _ => None,
    }
}
