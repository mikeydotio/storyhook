//! `story doctor`: what is wrong with this project, and what can be fixed.
//!
//! # Doctor asks three questions, where the legacy one asked a third of them
//!
//! The third arrived last and is the shortest: **is the catalog whole?** Every
//! project must define `todo`, `in-progress` and `blocked` as OPEN states and
//! `done` as a CLOSED one (SH-125), and a project written before that rule
//! existed is reported here and repaired by `--fix`. It leads the report
//! because it is a property of the project rather than of any story, and
//! because a project below the floor cannot have its states edited at all until
//! it is cleared — so it explains the refusals the other findings do not.
//!
//! The other two are about stories, and the second of those is what the store
//! made possible.
//!
//! The legacy doctor asked only about *stories*: dangling relations, missing
//! inverses, multiple parents, parent/child cycles, unknown types. Those are
//! all properties of the snapshots — and a snapshot is a **cache** of a fold of
//! a story's events. If the cache and the events disagree, every one of those
//! answers is computed from the wrong input, and the legacy doctor could not
//! tell: it had no independent copy of the truth to compare against.
//!
//! The store does. [`crate::store::diff_read_model`] re-folds every story from
//! its own history and compares the result with the persisted row, which is
//! exactly the oracle the read model lacked. So this service reports both
//! dimensions:
//!
//! * **story-level integrity** — the legacy checks, reproduced verbatim; and
//! * **read-model drift** — rows that disagree with their events, stories with
//!   events and no row (or a row and no events), histories that will not fold,
//!   and edges only one end's history claims.
//!
//! Only the first kind of drift is repairable. Re-folding fixes a stale row;
//! it cannot fix a missing *event*, which is what an asymmetric relation is.
//! `--fix` says so rather than claiming a repair it did not make.
//!
//! One kind of read-model drift is not a *question* at all: an event whose
//! kind this build has never heard of. SH-185 gave that its own channel,
//! [`Examination::notices`], which never contributes to
//! [`Examination::findings`] or [`repair`]'s verdicts — see that field's doc
//! comment for why.
//!
//! Both halves come out of [`examine`], and out of **one** fold of the project:
//! the drift oracle is the expensive question here, and asking it twice per
//! `story doctor` was SH-267.
//!
//! [`examine`]: IntegrityService::examine
//! [`repair`]: IntegrityService::repair

use std::collections::{BTreeMap, BTreeSet};

use crate::domain::finding::{Finding, FindingCode, FindingData};
use crate::domain::{
    STATE_ROLE_ACTIVE, StateDef, StoryEvent, StorySnapshot, TypeDef, active_state,
    compute_integrity_issues, inverse_relation, normalize_labels, validate_required_states,
    validate_type_slug,
};
use crate::error::{AppError, IntegrityDetail};
use crate::store::{
    ExpectedSeq, ProjectId, ReadOps, Store, StoryNo, StoryQuery, diff, repair_read_model,
};

use super::state_set::write_states_repairing;
use super::{
    Ctx, append_and_fold, project_prefix,
    query::{story_map, story_views},
};

/// Reports and repairs a project's integrity.
pub struct IntegrityService<'a, S: Store> {
    ctx: &'a Ctx<'a, S>,
}

impl<'a, S: Store> IntegrityService<'a, S> {
    /// An integrity service for `ctx`'s project.
    pub fn new(ctx: &'a Ctx<'a, S>) -> Self {
        Self { ctx }
    }

    /// Everything a `story doctor` read pass has to say about this project,
    /// from **one** fold of it.
    ///
    /// # Why one method and not two (SH-267)
    ///
    /// The two halves used to be `report` and `notices`, and every caller
    /// wanted both — the read arm of `story doctor` to print them, `repair`
    /// below to decide its verdict and its advice. Each began by calling
    /// [`crate::store::diff_read_model`], which is not a lookup: it re-folds
    /// every story in the project from its own events. So a `story doctor`
    /// folded the whole project twice and a `--fix` four times, and nothing
    /// said so, because re-asking a question whose answer you already hold
    /// moves no output. Invisible on a small project; on a large one it is the
    /// dominant cost of the command an operator reaches for when the store is
    /// *already* misbehaving.
    ///
    /// Returning both halves is what makes the second fold unaskable rather
    /// than merely unasked: there is no entry point left that computes one half
    /// alone, so the pairing cannot drift back apart.
    ///
    /// # And one transaction
    ///
    /// For the reason the catalog and story halves already shared one: a report
    /// assembled from several instants can name a state, a story, or a
    /// divergence that the rest of it did not see. The drift half used to be
    /// read from a transaction of its own, which is the same hazard one layer
    /// down.
    pub fn examine(&self) -> Result<Examination, AppError> {
        let project = self.project();
        Ok(self.ctx.store().read(|tx| {
            let states = tx.states(project)?;
            let mut findings = catalog_issues(&states, &tx.types(project)?);
            findings.extend(story_issues(tx, project)?);

            // The oracle the legacy read model never had, asked once. On a
            // healthy project it contributes nothing, which is why adding it
            // did not move a single byte of `doctor`'s existing output.
            let drift = diff(tx, project)?;
            let prefix = project_prefix(tx, project)?;
            findings.extend(drift_issues(&drift, &prefix));

            let mut notices = notice_issues(&drift, &prefix);
            notices.extend(active_state_notice(&states));
            notices.extend(blocked_without_reason_notices(tx, project)?);
            Ok(Examination { findings, notices })
        })?)
    }

    /// Repairs what can be repaired, then reports what is left.
    ///
    /// Three things happen, in this order and for this reason:
    ///
    /// 1. **The read model is re-folded** from the events, which subsumes the
    ///    legacy path's archived-snapshot repair and covers every story rather
    ///    than only the archived ones. It goes **first** because the story
    ///    repairs below read what it writes — see *The cache is repaired before
    ///    it is consulted* (SH-285).
    /// 2. **Missing inverse edges are written**, as events, on the story that
    ///    is missing them — the only layer that can fix an asymmetric history.
    /// 3. **Relations pointing at stories that do not exist are retracted**,
    ///    also as events.
    ///
    /// Then [`examine`](Self::examine) runs again, and what it still finds rides
    /// out on the returned [`FixOutcome`] rather than being raised here: a
    /// repair that did not actually fix the project must not exit zero, and
    /// [`FixOutcome::verdict`] is what makes sure of it.
    ///
    /// # Every story is asked; only an open one is written to
    ///
    /// The legacy repair asked its questions of the open stories only, and
    /// that was data loss twice over. Relate two stories, close one, and it
    /// retracted the survivor's edges — the archived counterpart was not in
    /// the open set, so the edge read as dangling — then reported the
    /// asymmetry it had just created as a failure the command could never
    /// clear. And a repair a *closed* story's own relation implied was never
    /// even attempted, however open the story it had to be written to.
    ///
    /// So the questions are asked of every story and the answer decides where
    /// the repair lands, which is what the open/closed distinction was always
    /// really about: **appends go to open stories only**. A missing inverse is
    /// written to the end that lacks it whenever that end is open, whichever
    /// end raised the question.
    ///
    /// # What it could not do, it says (SH-225)
    ///
    /// When the only story a repair could append to is closed, the repair is
    /// skipped — a closed history stays closed — and named in the output, with
    /// the story to reopen. It used to be skipped in silence, which made
    /// `--fix` indistinguishable from a broken one: the report kept naming a
    /// finding the operator had just told the doctor to fix, and nothing said
    /// that a manual reopen was the only way through. See
    /// [`blocked_repairs_detail`].
    ///
    /// # The cache is repaired before it is consulted, and existence is not a
    /// cache question (SH-285)
    ///
    /// The story pass reads snapshots — every story's own claims, labels and
    /// the ids it names — out of the `stories` table, which is a *cache* of a
    /// fold of the events. Repairing that cache used to be step 3, after the
    /// pass that reads it, and the consequences ran in both directions. A story
    /// with events and no row is absent from `all_stories`, so a **valid** edge
    /// naming it read as dangling, and an open claimant had that edge
    /// *retracted* — silent destruction of correct data by the repair tool,
    /// from a store that held nothing worse than a rebuildable cache miss. In
    /// the other direction, a row that merely disagreed with its events made
    /// the pass fabricate: an inverse the claimant's history never asserted, or
    /// a normalized label set computed from labels the story does not have.
    /// Every one of those is an **append**, and an append cannot be un-written.
    ///
    /// So the re-fold moved to step 1. It sits after the catalog write because
    /// [`crate::store::rebuild`] resolves superstates through the project's
    /// state definitions, and a project below the required-state floor cannot
    /// fold the stories sitting in the states it is missing.
    ///
    /// Ordering alone is not enough, because it only arranges for the cache to
    /// be *correct* when it is consulted, and one story defeats that: one whose
    /// events do not fold keeps no row however often the model is repaired. Its
    /// inbound edges are still valid. So the existence question — the only one
    /// whose wrong answer destroys data — is asked of the events instead, by
    /// [`endpoint_exists`]. The two are one mechanism each rather than two
    /// guarding one invariant: the probe answers *does this story exist*, the
    /// ordering answers *is what I am reading about it true*.
    ///
    /// # What it *undid*, it stops saying — and why nothing reconciles that now
    /// (SH-271, closed by SH-285)
    ///
    /// The blocked list is decided in the story pass and printed after it, so a
    /// later step could once dissolve a finding the pass had written advice
    /// about: the missing row above made a valid edge read as dangling, and the
    /// advice was then to reopen the claiming story and retract an edge the
    /// same run had just made whole. SH-271 reconciled the rendered list
    /// against the post-repair findings to drop those entries — the same defect
    /// answered one layer away from where it happened.
    ///
    /// With the re-fold ahead of the pass there is nothing left to reconcile.
    /// Only the pass's own appends land between the list being built and the
    /// closing report, and each is made for a repair that was *not* blocked —
    /// an entry is blocked precisely because the story it would append to is
    /// closed. So the filter became an identity whose only reachable effect was
    /// to *delete* advice, which is the SH-225 defect it was written to avoid,
    /// and it is gone. That it was an identity is not an argument from control
    /// flow: it was asserted here and the whole suite run against it before the
    /// deletion, and nothing tripped.
    ///
    /// # What it *did*, it says — including when it failed (SH-266)
    ///
    /// The message and the failed run's `advice` are one list, assembled
    /// before the verdict. They used to be two, and the failing half carried a
    /// subset: a repair this command really made — states added back to a
    /// catalog below the floor — went unmentioned whenever anything else
    /// remained, which reads as "nothing happened" to an operator who then
    /// repeats it.
    ///
    /// # Why the verdict is a value and not an error (SH-270)
    ///
    /// Because `Err(AppError::Integrity)` cannot answer the question its one
    /// caller has to ask. Three unrelated things reach that variant from here:
    ///
    /// * the **verdict** — repairs ran, findings remain — minted below;
    /// * a story whose events will not fold, raised by
    ///   [`crate::domain::fold_story`] from *inside* [`append_and_fold`] while
    ///   a repair is being written, which rolls that write back; and
    /// * whatever the store layer raises beneath either.
    ///
    /// A caller matching on the variant to mean the first would act on an
    /// aborted repair as though it had completed. `story doctor --fix` is that
    /// caller, and what it goes on to do — [`deregister_orphaned`] and
    /// [`register_found_origins`] — are durable mutations spanning *every*
    /// project in the store, so the misreading is not academic.
    ///
    /// Returning the outcome makes the distinction structural: the `?` in that
    /// arm can now only carry a genuine failure, and the verdict is minted once,
    /// after the caller has added what it knows. It also makes both `doctor`
    /// arms the same three lines — the read arm already hands
    /// [`examine`](Self::examine)'s findings and advice to
    /// [`IntegrityDetail::report`] rather than deciding its own health (SH-244).
    ///
    /// [`deregister_orphaned`]: super::CatalogService::deregister_orphaned
    /// [`register_found_origins`]: super::CatalogService::register_found_origins
    pub fn repair(&self) -> Result<FixOutcome, AppError> {
        let now = self.ctx.now();
        let project = self.project();

        // The catalog first, and in a write of its own. Every refusal the
        // required-state floor produces names this command, so it has to be the
        // thing that actually clears them — and it must not be entangled with
        // the story repairs below, which would roll it back on an unrelated
        // failure.
        let states_added = self.ctx.store().write(|tx| {
            let before = tx.states(project)?;
            let after = write_states_repairing(tx, project, &before)?;
            Ok(after.len() - before.len())
        })?;

        // Then the read model, before anything reads it (SH-285). Still a write
        // of its own, and still atomic: folding it into the story repairs below
        // would put the one repair that is always safe at the mercy of an
        // unrelated append failure.
        let repair = repair_read_model(self.ctx.store(), project)?;

        // `prefix` rides out of the write because the advice assembled after it
        // has to render a `StoryNo` the way the report does (SH-269), and this
        // is where the project already answered for it.
        let (touched, blocked, prefix) = self.ctx.store().write(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let states = tx.state_map(project)?;
            let all = all_stories(&*tx, project)?;
            let open = open_stories(&*tx, project)?;
            let mut touched: BTreeSet<String> = BTreeSet::new();
            let mut blocked: BTreeSet<BlockedRepair> = BTreeSet::new();

            for (id, story) in &all {
                // Each candidate carries the event and the imperative sentence
                // describing it: a story too closed to be appended to owes the
                // operator the sentence rather than the event.
                let mut own_repairs: Vec<OwnRepair> = Vec::new();
                for relation in &story.relationships {
                    let other = match all.get(&relation.other_id) {
                        Some(other) => other,
                        None => {
                            // Missing from the read model is not missing from
                            // the project (SH-285). The row was restored above
                            // unless this story's events will not fold, and an
                            // unfoldable story is still a story: its inbound
                            // edges are valid and retracting them would destroy
                            // correct data. Nothing more can be said about it
                            // here — its claims are exactly what could not be
                            // read — so the pass leaves it to the report, which
                            // names the fold failure the operator has to act on.
                            if endpoint_exists(&*tx, project, &prefix, &relation.other_id)? {
                                continue;
                            }
                            own_repairs.push(OwnRepair {
                                event: StoryEvent::StoryRelationshipRemoved {
                                    at: now.clone(),
                                    other_id: relation.other_id.clone(),
                                    relation: relation.relation.clone(),
                                },
                                repair: format!(
                                    "retract its dangling relation `{}` to the missing story `{}`",
                                    relation.relation, relation.other_id
                                ),
                            });
                            continue;
                        }
                    };
                    let Some(expected) = inverse_relation(&relation.relation) else {
                        continue;
                    };
                    if has_relation(other, expected, id) {
                        continue;
                    }
                    // An archived story's history is closed; a missing inverse
                    // on one is a finding, not something to append to. Named
                    // here rather than left silent (SH-225), and named against
                    // the *other* end, because that is the story an operator
                    // has to reopen — `compute_integrity_issues` reports this
                    // finding against `id`, which is the end that already has
                    // its half.
                    if !open.contains_key(&relation.other_id) {
                        blocked.insert(BlockedRepair {
                            reopen: relation.other_id.clone(),
                            repair: format!(
                                "write the missing inverse relation `{expected}` of {id}'s `{}`",
                                relation.relation
                            ),
                        });
                        continue;
                    }
                    let other_no = StoryNo::parse_id(&prefix, &relation.other_id)
                        .map_err(|error| AppError::Storage(format!("unparseable id: {error}")))?;
                    append_and_fold(
                        tx,
                        project,
                        other_no,
                        &prefix,
                        &states,
                        ExpectedSeq::Any,
                        &[StoryEvent::StoryRelationshipAdded {
                            at: now.clone(),
                            other_id: id.clone(),
                            relation: expected.to_string(),
                        }],
                        self.ctx.provenance(),
                    )?;
                    touched.insert(relation.other_id.clone());
                }

                // A comma-bearing or blank label (SH-164) is repaired the same
                // way a stale read-model row is: re-emit the normalized set as
                // a fresh event.
                let normalized_labels = normalize_labels(&story.labels);
                if normalized_labels != story.labels {
                    let repair = format!("normalize its labels to {normalized_labels:?}");
                    own_repairs.push(OwnRepair {
                        event: StoryEvent::StoryLabelsSet {
                            at: now.clone(),
                            labels: normalized_labels,
                        },
                        repair,
                    });
                }

                if own_repairs.is_empty() {
                    continue;
                }
                // Every repair above appends to `story` itself, so a closed one
                // is out of reach and says so instead of vanishing (SH-225).
                if !open.contains_key(id) {
                    blocked.extend(own_repairs.into_iter().map(|candidate| BlockedRepair {
                        reopen: id.clone(),
                        repair: candidate.repair,
                    }));
                    continue;
                }
                let events: Vec<StoryEvent> = own_repairs
                    .into_iter()
                    .map(|candidate| candidate.event)
                    .collect();
                let story_no = StoryNo::parse_id(&prefix, id)
                    .map_err(|error| AppError::Storage(format!("unparseable id: {error}")))?;
                append_and_fold(
                    tx,
                    project,
                    story_no,
                    &prefix,
                    &states,
                    ExpectedSeq::Any,
                    &events,
                    self.ctx.provenance(),
                )?;
                touched.insert(id.clone());
            }
            Ok((touched, blocked, prefix))
        })?;

        let mut touched: BTreeSet<String> = touched;
        touched.extend(
            repair
                .repaired
                .divergences
                .iter()
                .map(|divergence| divergence.story_no.to_string()),
        );
        // A row a story's own history supports, put back — a repair, and one
        // this command reported having *not* made until SH-271. Not
        // `repair.rewritten`, which is every story that folds: ingesting that
        // would make every run on a healthy project claim a repair. Nor
        // `extra_rows`, which re-folding does not remove, or `fold_failures`,
        // which it cannot fix — a story in either is still a finding below, so
        // the run fails and never reaches the headline.
        touched.extend(repair.repaired.missing_rows.iter().map(ToString::to_string));

        // One fold for both halves, and the *only* fold this run performs after
        // the repair above (SH-267).
        let Examination {
            findings: remaining,
            notices,
        } = self.examine()?;
        let blocked_detail = blocked_repairs_detail(&blocked);

        // Everything this run has to say that is not a finding, assembled
        // once and in the order it prints. Each entry is one rendered block,
        // so a multi-line one keeps its shape.
        //
        // **Built before the verdict, not inside either outcome** (SH-266).
        // The success message used to be assembled here by hand and the
        // failure path picked two of its four parts to carry as advice, so a
        // repair that succeeded and a run that failed reported different
        // things about the same run: the states this command had just added to
        // the catalog went unmentioned, and "could not be repaired" was
        // unreachable in *both* — a story the read model cannot fold is also a
        // `FoldFailure` finding, so a run with one always fails.
        let mut advice: Vec<String> = Vec::new();
        if states_added > 0 {
            advice.push(format!(
                "added {states_added} required {} this project was missing",
                if states_added == 1 { "state" } else { "states" }
            ));
        }
        if !blocked_detail.is_empty() {
            advice.push(blocked_detail);
        }
        if !repair.unrepairable.is_empty() {
            advice.push(format!(
                "{} stor{} could not be repaired:\n{}",
                repair.unrepairable.len(),
                if repair.unrepairable.len() == 1 {
                    "y"
                } else {
                    "ies"
                },
                repair
                    .unrepairable
                    .iter()
                    // The rendered id, like every finding above it (SH-269).
                    // This line named a bare `StoryNo` without even the word
                    // "story" in front of it, so `41: …` sat under `SH-41: …`
                    // in one block.
                    .map(|(story, reason)| format!("{}: {reason}", story.to_id(&prefix)))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }
        // Nothing to fix here, by design — not a repair failure. A notice
        // never enters `remaining` above, so without this it would vanish
        // from `--fix`'s own report the moment nothing else needed repairing.
        // Deliberately generic: `notices` now holds two unrelated kinds
        // (SH-242 added the second), so the wrapper can no longer presume
        // they are all about an unrecognised event the way it once did —
        // each notice string is already a complete, self-describing sentence.
        if !notices.is_empty() {
            advice.push(format!(
                "{} notice{} — nothing to fix, by design:\n{}",
                notices.len(),
                if notices.len() == 1 { "" } else { "s" },
                notices.join("\n")
            ));
        }

        let headline = if !touched.is_empty() || states_added > 0 {
            "doctor repaired supported integrity issues"
        } else if blocked.is_empty() {
            "doctor found nothing to fix"
        } else {
            // Not "nothing to fix": there is something, and this command
            // cannot be the one to fix it (SH-225).
            "doctor found nothing it could fix"
        };
        Ok(FixOutcome {
            findings: remaining,
            advice,
            headline,
        })
    }

    fn project(&self) -> ProjectId {
        self.ctx.project()
    }
}

/// What a `story doctor` read pass found: the damage, and the remarks that are
/// not damage.
///
/// One value rather than two calls because it is one fold of the project — see
/// [`IntegrityService::examine`] for why that matters.
pub struct Examination {
    /// Every problem `story doctor` reports, in the order it reports them:
    /// the catalog first, then story-level findings, then read-model drift.
    ///
    /// The catalog leads because it is a property of the *project*, and because
    /// a project below the required-state floor cannot have its states edited
    /// at all until this is cleared — so it is the finding that explains the
    /// others' refusals.
    ///
    /// Empty means the project is healthy; the caller hands a non-empty one to
    /// [`IntegrityDetail::report`]. Deliberately excludes
    /// [`notices`](Self::notices): a project whose only anomaly is an event
    /// kind this build has never heard of is healthy by this definition,
    /// because that anomaly is a newer storyhook's data, not damage (SH-185).
    ///
    /// Every element carries the sentence it used to *be* (SH-244), so the
    /// rendered report is these findings' own messages joined and nothing
    /// re-renders them.
    pub findings: Vec<Finding>,
    /// Findings that are informational rather than damage.
    ///
    /// Three occupants. An event whose kind this build has never heard of,
    /// which is a newer storyhook's data (SH-67), not corruption. A
    /// project with no state configured `role=active` (SH-242) — not damage
    /// either, since `active_state`'s "exactly two open states" fallback
    /// covers a project the required-states floor (SH-125) has already made
    /// unreachable in practice (the floor alone puts three states OPEN), so
    /// silence here is the *common* case for anything written before the
    /// role concept existed, not a rare one. And a story sitting in the
    /// reserved `blocked` state with no `awaiting` reason recorded (SH-205)
    /// — human-legibility only, since `is_ready` (SH-126) already treats
    /// `state == "blocked"` as not-ready with or without a reason attached,
    /// so this is the backstop for a skipped dashboard prompt or a bare
    /// scripted `move` rather than a dispatch-safety gap. None of the three
    /// contribute to [`findings`](Self::findings)' health verdict,
    /// [`IntegrityService::repair`]'s success or failure, or `story doctor`'s
    /// exit code
    /// — SH-185's council put the first one here specifically so it could
    /// not, and the other two follow the same reasoning
    /// [`crate::domain::with_required_states`] already gives for never
    /// awarding a role during a floor repair: which state should be active,
    /// or why a story is blocked, is not this command's to guess. The caller
    /// still owes all three visibility: see `notice_issues`'s doc comment.
    pub notices: Vec<String>,
}

/// Everything a `--fix` run did, and everything still wrong after it.
///
/// The value [`IntegrityService::repair`] returns, so that "repairs ran,
/// findings remain" is a *state of this struct* rather than an error variant
/// three unrelated failures also reach. See that method for why.
///
/// # One advice list, still (SH-266)
///
/// `advice` arrives already holding everything the repair itself had to say,
/// in the order it prints, and a caller with more to add **pushes onto it**
/// rather than keeping a second list beside it. That is the whole point of the
/// field: SH-266 fixed a defect whose shape was exactly two lists kept in step
/// by hand, where the failing path carried a subset of the succeeding one. A
/// caller that appends cannot reintroduce it, because both outcomes are
/// rendered from this one vector — [`message`](Self::message) joins it, and
/// [`verdict`](Self::verdict) hands it to [`IntegrityDetail`] whole.
pub struct FixOutcome {
    /// What is still wrong once every repair this command can make has been
    /// made. Empty means the project is healthy.
    ///
    /// The emptiness question is **not** asked here — [`IntegrityDetail::report`]
    /// owns it (SH-244), and [`verdict`](Self::verdict) is the one place that
    /// asks it of this value.
    pub findings: Vec<Finding>,
    /// Everything this run has to say that is not damage, in the order it
    /// prints: repairs made, repairs blocked, stories beyond repair, notices.
    /// Each entry is one rendered block, so a multi-line one keeps its shape.
    pub advice: Vec<String>,
    /// The first line of a successful run's report, decided by what the run
    /// actually did. Unused when `findings` is non-empty: a run that failed has
    /// no headline, it has a report.
    pub headline: &'static str,
}

impl FixOutcome {
    /// The report a run with nothing left wrong prints: the headline, then
    /// every advice entry.
    ///
    /// Borrows rather than consumes, because the caller that has to render this
    /// on a *failure* path — where the error is what propagates and this is
    /// merely context — still owns the outcome afterwards.
    #[must_use]
    pub fn message(&self) -> String {
        std::iter::once(self.headline.to_string())
            .chain(self.advice.iter().cloned())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// This outcome as the command's result: the rendered message, or the
    /// integrity error a remaining finding makes it.
    ///
    /// The single place the verdict is minted, and it delegates the decision to
    /// [`IntegrityDetail::report`] rather than re-asking it — so a caller cannot
    /// disagree with `story doctor`'s read path about what "healthy" means.
    pub fn verdict(self) -> Result<String, AppError> {
        // Rendered before the fields move, and unconditionally: the branch below
        // belongs to `IntegrityDetail::report`, and re-deciding here which of
        // the two outputs is needed would be the second copy of the emptiness
        // question that SH-244 exists to prevent.
        let message = self.message();
        match IntegrityDetail::report(self.findings, self.advice) {
            Some(detail) => Err(AppError::Integrity(detail)),
            None => Ok(message),
        }
    }
}

/// A repair [`IntegrityService::repair`] identified, could not make, and owes the
/// operator a sentence about (SH-225).
///
/// Ordered by the story to reopen, then the repair, which is the order
/// [`blocked_repairs_detail`] prints.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct BlockedRepair {
    /// The closed story whose history blocks the repair — the one to reopen.
    reopen: String,
    /// The imperative sentence describing what would have been written.
    repair: String,
}

/// A repair that appends to the story that raised it, before that story's state
/// decides whether it is written or [`blocked`](BlockedRepair).
struct OwnRepair {
    /// What would be appended.
    event: StoryEvent,
    /// The sentence describing it, for the operator who has to make it happen
    /// by hand.
    repair: String,
}

/// Whether the project's catalog meets the required-state floor (SH-125).
///
/// One finding, not one per missing state: the remedy is the same command
/// however many are missing, and [`validate_required_states`] already names
/// them all in its own sentence.
fn catalog_issues(states: &[StateDef], types: &[TypeDef]) -> Vec<Finding> {
    let mut issues: Vec<Finding> = validate_required_states(states)
        .err()
        // No `subject`: this is a property of the *project*, and a story id
        // here would be an invention.
        .map(|error| Finding::new(FindingCode::RequiredStates, error.to_string()))
        .into_iter()
        .collect();

    // SH-134. `add_type` refuses these now, so one can only be here because an
    // older storyhook wrote it or a document carried it in — and a document's
    // catalog is deliberately left raw, because the only repair is a rename and
    // every `StoryTypeSet` event names the slug it set. Reported, never fixed:
    // the two automatic repairs available are that banned rename and retyping
    // stories the user never mentioned, so `--fix` says what it cannot do
    // rather than claiming a repair it did not make.
    issues.extend(types.iter().filter_map(|story_type| {
        validate_type_slug(&story_type.slug).err().map(|error| {
            Finding::new(
                FindingCode::UnaddressableType,
                format!(
                    "type `{}` cannot be addressed: {error}. Retype its stories (`story set <id> \
                     --type none`) and remove it with `story type remove -- '{}'`",
                    story_type.slug, story_type.slug
                ),
            )
            .carrying(FindingData::Type {
                slug: story_type.slug.clone(),
            })
        })
    }));
    issues
}

/// The legacy story-level checks, reproduced exactly.
///
/// # Where the checks are read from, and why it changed
///
/// This used to read `StoryView::flagged_reasons`, which is prose. It now
/// reads [`compute_integrity_issues`] directly, because that is where the
/// codes and the `remedy` live and rendering them to sentences only to parse
/// them back would be the defect SH-244 exists to remove.
///
/// One consequence is deliberate: `service::query` appends "story is obviated
/// by another story" to `flagged_reasons` *after* the checks run, for
/// `StoryView`'s benefit. Reading the checks directly means that sentence
/// never reaches here at all.
///
/// `flagged_reasons` keeps its published `Vec<String>` shape; only this
/// consumer stopped going through it.
///
/// # Nothing is filtered out, and that is a change (SH-268)
///
/// There used to be an `is_suppressed` here —
/// `reason.contains("obviated") || reason.contains("conflicts")` — inherited
/// from the legacy doctor, where it filtered `flagged_reasons` and its only two
/// matching producers were the *authoring flag sentences* "story is obviated by
/// another story" and "story conflicts with another story". Both targets are
/// gone: `conflicts-with` left the relation vocabulary, and the paragraph above
/// is why the obviation sentence can no longer arrive. What the substring test
/// still caught was collateral it was never aimed at — structural findings
/// about an obviation edge — and it caught them **asymmetrically**, because the
/// two finding kinds spell opposite ends of the edge into their sentences: a
/// missing inverse names the *expected inverse* (so the suppressed claim was
/// `obviates`), a dangling edge names the *claimed relation* (so the suppressed
/// claim was `obviated-by`). Which end of a broken pair survived decided
/// whether `doctor` mentioned it.
///
/// It was the harmful end that went unmentioned. [`crate::domain::is_ready`]
/// excludes a story only when *that story* carries `obviated-by`, so the
/// suppressed missing-inverse case is exactly the one where a story an author
/// declared unnecessary keeps being recommended by `story next`. And `--fix`
/// never consulted the filter at all: it repaired these edges while the report
/// called the project healthy — one contract, two halves, disagreeing.
///
/// So it is deleted rather than made precise. The authoring decision it was
/// written for is a *symmetric* obviation edge, which produces no finding here
/// in the first place.
///
/// The unknown-type check is separate because it is a property of the story
/// *and the catalog*, which the cross-story integrity pass does not see.
fn story_issues(tx: &impl ReadOps, project: ProjectId) -> Result<Vec<Finding>, AppError> {
    let types: BTreeSet<String> = tx
        .types(project)?
        .into_iter()
        .map(|story_type| story_type.slug)
        .collect();

    let stories = story_map(tx, project)?;
    let mut by_story = compute_integrity_issues(&stories);

    let mut issues = Vec::new();
    for story in stories.values() {
        // Sorted and deduplicated by sentence, which is what `story_views`
        // did to `flagged_reasons` before this read the checks directly — the
        // report's byte order is part of its golden snapshot.
        let mut found = by_story.remove(&story.id).unwrap_or_default();
        found.sort_by(|a, b| a.message.cmp(&b.message));
        found.dedup_by(|a, b| a.message == b.message);
        issues.extend(found.into_iter().map(|finding| {
            let message = format!("{}: {}", story.id, finding.message);
            Finding { message, ..finding }
        }));

        if let Some(slug) = &story.story_type
            && !types.contains(slug)
        {
            issues.push(
                Finding::new(
                    FindingCode::UnknownType,
                    format!("{}: unknown type `{slug}`", story.id),
                )
                .about(&story.id)
                .carrying(FindingData::Type { slug: slug.clone() }),
            );
        }
        // A label written before SH-164's write-path guard existed — a
        // comma-bearing one (unsplittable and unaddressable by
        // `story unlabel`/`list --label`) or a blank/untrimmed one.
        // `--fix` repairs this on an open story; on a closed one it stays a
        // finding, the same as any other issue a closed story's history
        // cannot be appended to fix — and `--fix` names the story to reopen
        // rather than leaving the finding unexplained (SH-225).
        if story.labels != normalize_labels(&story.labels) {
            issues.push(
                Finding::new(
                    FindingCode::MalformedLabels,
                    format!(
                        "{}: malformed labels {:?} — a label cannot contain a comma or be blank",
                        story.id, story.labels
                    ),
                )
                .about(&story.id)
                // Repaired in place: the normalized set is re-emitted as an
                // event on this story itself.
                .repaired_on(&story.id)
                .carrying(FindingData::Labels {
                    labels: story.labels.clone(),
                }),
            );
        }
    }
    Ok(issues)
}

/// The read-model drift a doctor report adds to the story-level findings.
///
/// Deliberately **not** [`ReadModelDiff::describe`]: that also lists
/// asymmetric relations, which [`story_issues`] already reports from the
/// snapshots in the legacy wording (`missing inverse relation ...`). Printing
/// both would say the same thing twice, in two vocabularies, and would move
/// `doctor`'s bytes for a project the legacy path already diagnosed. What is
/// left is exactly the question the legacy doctor could not ask: does the read
/// model still equal a fold of its own events?
///
/// # And the one question nothing asked at all
///
/// `unknown_events` was computed here and dropped on the floor until SH-67, so
/// nothing in the product ever told a user their store held an event this build
/// could not decode. That silence was affordable while the fold quietly skipped
/// them and `story export` quietly dropped them: an unreported loss and an
/// unreported retention look the same from outside. Export carries them now.
///
/// Only *half* of `unknown_events` belongs here, though — the half that is
/// damage. The store's decoder falls back to `Unknown` on *any* failure, so a
/// kind this build knows and could not read is a torn payload, while a kind it
/// has never heard of is a newer storyhook's data sitting patiently in a store
/// an older one is reading. SH-185's council put that second half somewhere it
/// cannot move `report()`'s health verdict at all: [`notice_issues`].
///
/// # One name per story (SH-269)
///
/// These sentences used to lead with the bare [`StoryNo`] — `story 41:` —
/// while every story-level one led with the rendered id, `SH-41:`. One command,
/// two names for one story, in one report, and neither spelling a substring of
/// the other: a reader of a mixed report could not tell the two lines were
/// about the same story. SH-244 had already plumbed `prefix` in here to give
/// each finding a `subject`, which made a machine consumer immune and left the
/// prose as the only surface still saying it twice.
///
/// So the sentence is rendered from the same value `subject` carries, not
/// merely from the same prefix — one expression, used twice, which is why they
/// cannot drift apart again.
/// `tests/service_integrity.rs::every_finding_that_names_a_story_leads_with_
/// the_rendered_id` pins the rule over the whole report rather than over these
/// five sentences, so a check added later inherits it.
fn drift_issues(drift: &crate::store::ReadModelDiff, prefix: &str) -> Vec<Finding> {
    let id = |story: &StoryNo| story.to_id(prefix);
    let mut lines = Vec::new();
    for story in &drift.missing_rows {
        let story = id(story);
        lines.push(
            Finding::new(
                FindingCode::MissingRow,
                format!("{story}: has events but no read-model row"),
            )
            .about(story),
        );
    }
    for story in &drift.extra_rows {
        let story = id(story);
        lines.push(
            Finding::new(
                FindingCode::ExtraRow,
                format!("{story}: read-model row with no events"),
            )
            .about(story),
        );
    }
    for (story, reason) in &drift.fold_failures {
        let story = id(story);
        lines.push(
            Finding::new(
                FindingCode::FoldFailure,
                format!("{story}: cannot be folded: {reason}"),
            )
            .about(story)
            .carrying(FindingData::Reason {
                reason: reason.clone(),
            }),
        );
    }
    for divergence in &drift.divergences {
        let story = id(&divergence.story_no);
        lines.push(
            Finding::new(
                FindingCode::ReadModelDivergence,
                format!(
                    "{story}: {} is `{}` but the events say `{}`",
                    divergence.field, divergence.persisted, divergence.rebuilt
                ),
            )
            .about(story)
            // The four values SH-243 hand-parsed out of 1.68MB of prose. They
            // were structured here all along and thrown away one line later.
            .carrying(FindingData::Divergence {
                field: divergence.field.clone(),
                persisted: divergence.persisted.clone(),
                rebuilt: divergence.rebuilt.clone(),
            }),
        );
    }
    for unknown in &drift.unknown_events {
        if crate::domain::is_known_event_kind(&unknown.kind) {
            let story = id(&unknown.story_no);
            lines.push(
                Finding::new(
                    FindingCode::UndecodableEvent,
                    format!(
                        "{story}: event {} is a `{}` this build cannot decode — retained \
                         verbatim, but not folded",
                        unknown.seq, unknown.kind
                    ),
                )
                .about(story)
                .carrying(FindingData::Event {
                    seq: unknown.seq.get(),
                    event_kind: unknown.kind.clone(),
                }),
            );
        }
    }
    lines
}

/// The other half of `unknown_events`: kinds this build has never heard of,
/// which is not damage, so it must never be folded into [`drift_issues`].
///
/// SH-185's council answer to what SH-67 left open: this is a *notice*, not a
/// finding. It plays no part in [`IntegrityService::report`]'s health verdict,
/// [`IntegrityService::repair`]'s success or failure, or `story doctor`'s exit
/// code — the caller decides separately how (never *whether*) to surface it,
/// because dropping it silently just because it lost a seat in the health
/// vector would be its own regression.
///
/// It takes `prefix` for the reason [`drift_issues`] does (SH-269): a notice is
/// rendered in the same report as the findings, so it names a story the same
/// way they do. This half had no `subject` to fall back on — a notice is a bare
/// `String` — which made it the worse of the two.
fn notice_issues(drift: &crate::store::ReadModelDiff, prefix: &str) -> Vec<String> {
    drift
        .unknown_events
        .iter()
        .filter(|unknown| !crate::domain::is_known_event_kind(&unknown.kind))
        .map(|unknown| {
            format!(
                "{}: event {} is of kind `{}`, which this build does not know — retained \
                 verbatim and carried by `story export`, but not folded. A newer storyhook wrote \
                 it.",
                unknown.story_no.to_id(prefix),
                unknown.seq,
                unknown.kind
            )
        })
        .collect()
}

/// Whether the project has a state `is_claimable` can resolve as "active"
/// (SH-242), and the advice to give when it does not.
///
/// Computed unconditionally, same as [`notice_issues`] — a project below the
/// floor (SH-125) gets this alongside that harder failure rather than after
/// it, since nothing about this check depends on the floor being met first.
/// In practice it matters most once the floor *is* met: a conforming project
/// already has three OPEN states (`todo`, `in-progress`, `blocked`), so
/// [`active_state`]'s "exactly two open states" fallback can never apply to
/// one; only an explicit `role=active` resolves anything from there.
/// `default_states()` sets that role on `in-progress` for every project
/// `story project new` creates, so this fires only for a project written
/// before that default existed, or one whose states were hand-edited past it.
fn active_state_notice(states: &[StateDef]) -> Vec<String> {
    if active_state(states).is_some() {
        return Vec::new();
    }
    vec![format!(
        "no state carries role `{STATE_ROLE_ACTIVE}` — `story next` and `is_claimable` cannot \
         tell that a story already sitting in `in-progress` is claimed, and will keep \
         recommending it. Run `story state set in-progress --role active` (or name whichever \
         state means work is underway) to fix it."
    )]
}

/// A story in the reserved `blocked` state with no `awaiting` reason
/// recorded (SH-205) — one notice per such story, in story-id order via
/// [`story_views`]'s own ordering. Only `state == "blocked"` is checked, not
/// superstate: `blocked` is pinned to `SuperState::Open` for every project by
/// the required-states floor (SH-125), so no closed story can carry it.
fn blocked_without_reason_notices(
    tx: &impl ReadOps,
    project: ProjectId,
) -> Result<Vec<String>, AppError> {
    Ok(story_views(tx, project, false)?
        .into_iter()
        .filter(|view| view.story.state == "blocked" && view.story.awaiting.is_none())
        .map(|view| {
            format!(
                "{}: sitting in `blocked` with no awaiting reason — `story block {} \"<reason>\"` \
                 (or `story move {} blocked --reason \"<text>\"` next time) explains why",
                view.story.id, view.story.id, view.story.id
            )
        })
        .collect())
}

/// What `--fix` identified and did not do, because the only story it could
/// have appended to is closed. Empty for an empty set, so a caller can splice
/// it in unconditionally.
///
/// Every line names the story to **reopen**, which is frequently not the story
/// the matching finding names: `compute_integrity_issues` reports a missing
/// inverse against the end that already has its half, while the repair belongs
/// on the end that lacks it. An operator working from the finding alone
/// reopens the wrong story — SH-225, where eight closed stories' malformed
/// labels sat behind this silence for a week because nothing said a manual
/// reopen was the only way through.
fn blocked_repairs_detail(blocked: &BTreeSet<BlockedRepair>) -> String {
    if blocked.is_empty() {
        return String::new();
    }
    format!(
        "{} repair{} could not be made — a closed story's history cannot be appended to. Reopen \
         it (`story reopen <id>`), re-run `story doctor --fix`, then close it again:\n{}",
        blocked.len(),
        if blocked.len() == 1 { "" } else { "s" },
        blocked
            .iter()
            .map(|entry| format!("{}: {}", entry.reopen, entry.repair))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

/// Whether this project holds any events for the story `id` names.
///
/// "Does this story exist?" asked of the authority rather than of the cache
/// (SH-285). [`all_stories`] answers from the `stories` table, which is a fold
/// of the events and can be missing a row the events fully support — so a
/// **valid** edge naming that story read as dangling, and
/// [`IntegrityService::repair`] retracted it. The events cannot be missing in
/// that way: they are the thing a row is derived from.
///
/// This is asked only of an endpoint [`all_stories`] could not find, and it is
/// an indexed `MAX(seq)` probe against the events table's primary key rather
/// than a fold, so it costs a lookup on a path that was already the unusual
/// one.
///
/// An id no [`StoryNo`] can be parsed out of answers **false**: it names no
/// story in this project, so there is nothing to look up and nothing to
/// preserve, and the edge is genuinely dangling. That branch is a guard rather
/// than a case with a fixture — `put_story` parses every relation it persists
/// and refuses the row outright, so no snapshot reaching this function can
/// carry one. Raising instead would turn a malformed id into a failure of the
/// whole repair run, which is the wrong answer to bad data.
fn endpoint_exists(
    tx: &impl ReadOps,
    project: ProjectId,
    prefix: &str,
    id: &str,
) -> Result<bool, crate::store::StoreError> {
    let Ok(story) = StoryNo::parse_id(prefix, id) else {
        return Ok(false);
    };
    Ok(tx.head_seq(project, story)?.get() > 0)
}

/// Every story in the project, keyed by id — archived and deleted included.
fn all_stories(
    tx: &impl ReadOps,
    project: ProjectId,
) -> Result<BTreeMap<String, StorySnapshot>, AppError> {
    Ok(tx
        .stories(project, &StoryQuery::all())?
        .into_iter()
        .map(|row| (row.snapshot.id.clone(), row.snapshot))
        .collect())
}

/// The project's unarchived stories, keyed by id.
///
/// `--fix` appends to these and no others: an archived story's history is
/// closed, and appending a repair event to it would reopen a question the
/// project already settled. Membership is the *destination* test, not the
/// question test — see [`IntegrityService::repair`] for the difference, and
/// [`blocked_repairs_detail`] for what a repair with no open destination
/// becomes instead.
fn open_stories(
    tx: &impl ReadOps,
    project: ProjectId,
) -> Result<BTreeMap<String, StorySnapshot>, AppError> {
    Ok(tx
        .stories(project, &StoryQuery::all().archived(false))?
        .into_iter()
        .map(|row| (row.snapshot.id.clone(), row.snapshot))
        .collect())
}

/// Whether `story` already asserts `relation` to `other_id`.
fn has_relation(story: &StorySnapshot, relation: &str, other_id: &str) -> bool {
    story
        .relationships
        .iter()
        .any(|candidate| candidate.relation == relation && candidate.other_id == other_id)
}
