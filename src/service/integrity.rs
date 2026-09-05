//! `story doctor`: what is wrong with this project, and what can be fixed.
//!
//! # Doctor asks three questions, where the legacy one asked a third of them
//!
//! The third arrived last and is the shortest: **is the catalog whole?** Every
//! project must define `todo`, `in-progress`, `verifying` and `blocked` as OPEN states and
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
//! inverses, parent/child cycles, unknown types. Those are
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
    STATE_ROLE_ACTIVE, StateDef, StoryEvent, TypeDef, active_state, compute_integrity_issues,
    inverse_relation, normalize_labels, validate_required_states, validate_type_slug,
};
use crate::error::{AppError, IntegrityDetail};
use crate::store::{
    AttachmentBlobRow, ExpectedSeq, ProjectId, ReadOps, Store, StoryNo, StoryQuery, diff,
    repair_read_model,
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

            // The oracle the legacy read model never had, asked once. On a
            // healthy project it contributes nothing, which is why adding it
            // did not move a single byte of `doctor`'s existing output.
            //
            // Asked **before** the story checks since SH-286, though still
            // exactly once: it is the thing that says which rows those checks
            // may believe. The order is the whole of the plumbing — no second
            // fold, no second transaction, no new store query.
            let drift = diff(tx, project)?;
            let prefix = project_prefix(tx, project)?;
            let unattested = rendered_ids(&drift.unattested(), &prefix);

            findings.extend(story_issues(tx, project, &prefix, &unattested)?);
            findings.extend(drift_issues(&drift, &prefix));

            let mut notices = notice_issues(&drift, &prefix);
            notices.extend(unvouched_row_notices(&drift, &prefix));
            notices.extend(active_state_notice(&states));
            notices.extend(blocked_without_reason_notices(tx, project)?);
            notices.extend(unlinked_blocker_notices(tx, project)?);
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
    /// 2. **Every story-level finding is asked for a repair**, and the ones
    ///    that have one are written as events on the story each finding's
    ///    `remedy` names — a missing inverse edge on the end that lacks it, a
    ///    retraction on the end claiming a relation to a story that does not
    ///    exist, a normalized label set on the story carrying a malformed one.
    ///    They are the report's own findings rather than a second walk of the
    ///    same graph — see *The repairs are the report's own findings*
    ///    (SH-273).
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
    /// end raised the question — which is exactly what a finding's `remedy`
    /// names, and since SH-273 it is read from there rather than recomputed.
    ///
    /// # The repairs are the report's own findings (SH-273)
    ///
    /// This pass used to walk `story.relationships` itself, recomputing the
    /// dangling edges and missing inverses that [`compute_integrity_issues`]
    /// had already computed for [`examine`](Self::examine), over the same story
    /// set. That duplicate walk was the **only** reason the two halves of one
    /// contract could disagree at all — and every defect in this family was a
    /// disagreement between them. SH-268: a filter applied to one walk and not
    /// the other, so `--fix` repaired what the report called healthy. SH-271:
    /// one walk's answer rendered after the other walk's world had changed.
    /// SH-225: a repair one walk identified and skipped in silence, which the
    /// other never mentioned. Each was fixed at its own encounter point; the
    /// origin was that the predicate had two implementations.
    ///
    /// So the pass asks [`story_issues`] — the same producer `examine` reads —
    /// and maps each finding to a repair through [`plan_repair`]. Since SH-244
    /// a [`Finding`] carries everything that needs: the [`FindingCode`] to
    /// match on, the `subject` it is about, the `remedy` naming the story a
    /// repair is written *to*, and the [`FindingData`] the event is built from.
    /// The two halves now agree by construction rather than by two authors
    /// keeping step, and `plan_repair`'s match is exhaustive over `FindingCode`,
    /// so a check added later cannot ship without somebody deciding whether
    /// `--fix` repairs it.
    ///
    /// The findings are asked for **inside this write transaction**, not from a
    /// second [`examine`](Self::examine). A second `examine` would fold the
    /// whole project again — the cost SH-267 removed, pinned by
    /// `tests/service_integrity.rs::a_doctor_fix_folds_the_project_twice` — and
    /// would read the project at an instant other than the one the repairs are
    /// made at, which is the hazard `examine`'s own "one transaction" section
    /// describes.
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
            // What the repair above could *not* put right, which is what the
            // pass below may not believe (SH-286). Read from the report the
            // repair just returned rather than from a fresh diff — that would
            // be the third fold SH-267 removed — and from `unattested()` rather
            // than from `repaired` whole, because `repaired` is the damage as
            // it stood *before* the repair. Skipping a story on the strength of
            // that would skip the label repair on the very row this run had
            // just restored.
            let unattested = rendered_ids(&repair.unattested(), &prefix);
            let states = tx.state_map(project)?;
            let open = open_story_ids(&*tx, project)?;
            let mut blocked: BTreeSet<BlockedRepair> = BTreeSet::new();
            // Keyed by the story the repair appends to, so a story owed
            // several takes one append rather than one per finding. The whole
            // pass is one transaction either way, so the grouping is not about
            // atomicity — it is about not writing a history in more pieces
            // than the run has decisions.
            let mut pending: BTreeMap<String, Vec<StoryEvent>> = BTreeMap::new();

            for finding in story_issues(&*tx, project, &prefix, &unattested)? {
                let Some(planned) = plan_repair(&*tx, project, &prefix, &now, &finding)? else {
                    continue;
                };
                // Every repair appends to exactly one story, so a closed
                // destination puts it out of reach — and it says so instead of
                // vanishing (SH-225). Frequently that is not the story the
                // finding names: a missing inverse is reported against the end
                // that already has its half.
                if !open.contains(&planned.destination) {
                    blocked.insert(BlockedRepair {
                        reopen: planned.destination,
                        repair: planned.repair,
                    });
                    continue;
                }
                pending
                    .entry(planned.destination)
                    .or_default()
                    .push(planned.event);
            }

            let mut touched: BTreeSet<String> = BTreeSet::new();
            for (destination, events) in pending {
                let story_no = StoryNo::parse_id(&prefix, &destination)
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
                touched.insert(destination);
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
        // A row this run deliberately did not rewrite (SH-410). Beside
        // `unrepairable` rather than inside it: that list is stories whose
        // events do not fold at all, where nothing may be concluded from the
        // row; this one is stories whose row is intact and probably better
        // than this build's own fold of them. Naming the withholding is the
        // whole remedy — `--fix` is the command an operator runs when they
        // already suspect something, and a silent no-op reads as "checked,
        // fine".
        if !repair.withheld.is_empty() {
            advice.push(format!(
                "{} row{} left exactly as found — this build cannot fold {} \
                 stor{} whole:\n{}",
                repair.withheld.len(),
                if repair.withheld.len() == 1 { "" } else { "s" },
                if repair.withheld.len() == 1 {
                    "that"
                } else {
                    "those"
                },
                if repair.withheld.len() == 1 {
                    "y"
                } else {
                    "ies"
                },
                repair
                    .withheld
                    .iter()
                    .map(|(story, unknown)| {
                        format!(
                            "{}: {}",
                            story.to_id(&prefix),
                            unknown
                                .iter()
                                .map(|event| format!("event {} (`{}`)", event.seq, event.kind))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    })
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

/// A repair [`plan_repair`] derived from one [`Finding`], before the
/// destination's state decides whether it is written or
/// [`blocked`](BlockedRepair).
struct PlannedRepair {
    /// The story the event is appended to — always the finding's `remedy`,
    /// which is frequently not the story the finding is *about* (SH-225).
    destination: String,
    /// What would be appended.
    event: StoryEvent,
    /// The sentence describing it, for the operator who has to make it happen
    /// by hand.
    repair: String,
}

/// The repair one finding calls for, or `None` when it calls for none.
///
/// The whole of what `story doctor --fix` knows about turning a report into
/// writes, in one exhaustive match (SH-273). Two properties follow from that
/// and are the point of the function:
///
/// * **`--fix` cannot repair anything the report did not raise**, because a
///   repair can only be reached by handing this a finding; and
/// * **a check added later cannot ship undecided**, because a new
///   [`FindingCode`] stops this match compiling until somebody says whether
///   `--fix` repairs it. That is the same forcing function `FindingCode`'s own
///   documentation claims for the renderer.
///
/// Every repair lands on the finding's `remedy` and is built from its
/// [`FindingData`]. A repairable code carries both unconditionally — the
/// producers set them in the same expression that mints the finding — so a
/// missing one is a defect in this build rather than damage in the store, and
/// it fails the run loudly instead of silently withholding a repair the report
/// has already promised.
///
/// It answers for any finding a report can carry, not only the ones this pass
/// is fed. The catalog and read-model halves reach it as `None` because their
/// repairs are not per-story appends at all: they are the two dedicated steps
/// [`IntegrityService::repair`] runs *before* this pass, each in its own
/// transaction.
fn plan_repair(
    tx: &impl ReadOps,
    project: ProjectId,
    prefix: &str,
    now: &str,
    finding: &Finding,
) -> Result<Option<PlannedRepair>, AppError> {
    match finding.code {
        FindingCode::DanglingRelation => {
            let (relation, other) = relation_payload(finding)?;
            // Missing from the read model is not missing from the project
            // (SH-285). The row was restored before this pass ran unless the
            // story's events will not fold, and an unfoldable story is still a
            // story: its inbound edges are valid and retracting them would
            // destroy correct data.
            //
            // **This used to overrule the report, and now it contradicts it**
            // (SH-286). Between SH-285 and SH-286 the probe answered the
            // existence question a second time and silently declined a repair
            // the report had promised — two authorities for one question, which
            // is the SH-273 shape every defect in this family had. The report
            // resolves it now, from the same events, and never raises this
            // finding for a story it can see is there. So reaching here with an
            // endpoint that has events is not damage in the store, it is this
            // build's report and its repair table disagreeing.
            //
            // The probe stays anyway, and stays here, because what is on the
            // other side of it is an irreversible append that destroys correct
            // data. It is no longer a second authority: it decides nothing
            // except whether to fail loudly, in the style of [`incomplete`].
            if endpoint_exists(tx, project, prefix, other)? {
                return Err(AppError::Storage(format!(
                    "internal: `story doctor --fix` was asked to retract `{relation}` to \
                     `{other}`, but that story has events — its report should have withheld the \
                     finding rather than promising a repair that would destroy a valid edge: {}",
                    finding.message
                )));
            }
            Ok(Some(PlannedRepair {
                destination: remedy(finding)?,
                event: StoryEvent::StoryRelationshipRemoved {
                    at: now.to_string(),
                    other_id: other.to_string(),
                    relation: relation.to_string(),
                },
                repair: format!(
                    "retract its dangling relation `{relation}` to the missing story `{other}`"
                ),
            }))
        }
        // One arm for both spellings: they differ only in wording, and a
        // mutual relation is its own inverse.
        FindingCode::MissingInverseRelation | FindingCode::MissingReciprocalRelation => {
            let (relation, _) = relation_payload(finding)?;
            let subject = finding
                .subject
                .as_deref()
                .ok_or_else(|| incomplete(finding, "subject"))?;
            let expected = inverse_relation(relation).ok_or_else(|| {
                AppError::Storage(format!(
                    "internal: `{relation}` has no inverse, so `story doctor --fix` cannot build \
                     the repair for a finding that says one is missing: {}",
                    finding.message
                ))
            })?;
            Ok(Some(PlannedRepair {
                destination: remedy(finding)?,
                event: StoryEvent::StoryRelationshipAdded {
                    at: now.to_string(),
                    other_id: subject.to_string(),
                    relation: expected.to_string(),
                },
                repair: format!(
                    "write the missing inverse relation `{expected}` of {subject}'s `{relation}`"
                ),
            }))
        }
        // A comma-bearing or blank label (SH-164) is repaired the way a stale
        // read-model row is: re-emit the normalized set as a fresh event.
        FindingCode::MalformedLabels => {
            let Some(FindingData::Labels { labels }) = &finding.data else {
                return Err(incomplete(finding, "label payload"));
            };
            let normalized = normalize_labels(labels);
            Ok(Some(PlannedRepair {
                destination: remedy(finding)?,
                event: StoryEvent::StoryLabelsSet {
                    at: now.to_string(),
                    labels: normalized.clone(),
                },
                repair: format!("normalize its labels to {normalized:?}"),
            }))
        }

        // Repaired by this run, but not by this pass: the catalog write and
        // the read-model re-fold each ran ahead of it, in a transaction of
        // their own.
        FindingCode::RequiredStates
        | FindingCode::MissingRow
        | FindingCode::ReadModelDivergence => Ok(None),

        // Not automatically repairable at all, each for its own reason. An
        // unaddressable type slug (SH-134) and a story typed with one the
        // catalog does not define: the only repairs available are a rename the
        // write path bans and retyping stories the user never mentioned.
        // A parent/child cycle: which edge is the wrong one is not this
        // command's to guess. An extra row: re-folding does
        // not remove it. A fold failure and a torn event payload: the evidence
        // is the thing that is damaged, and overwriting it with a guess would
        // destroy what an operator needs to read. And an unstructured finding
        // is not a doctor check at all — it is what a raise site with only
        // prose to offer mints.
        FindingCode::UnaddressableType
        | FindingCode::UnknownType
        | FindingCode::MultipleParents
        | FindingCode::ParentChildCycle
        | FindingCode::ExtraRow
        | FindingCode::FoldFailure
        | FindingCode::UndecodableEvent
        // And the one finding that exists *because* nothing was repairable:
        // `UnexaminedStory` is the disclosure attached to a story whose row the
        // events do not vouch for (SH-286), so a repair derived from that row
        // is the fabrication the disclosure exists to refuse.
        | FindingCode::UnexaminedStory
        // SH-315: none of the three attachment findings has a safe automatic
        // repair. A missing or orphaned blob could mean either half of a
        // half-completed write — appending the event without writing the
        // bytes, or the reverse — and guessing which one happened would mean
        // either fabricating bytes this build never received or silently
        // discarding a real image. A mismatch is evidence of corruption on
        // disk; overwriting it with a guess would destroy that evidence the
        // same way an unrepairable `FoldFailure` does.
        | FindingCode::MissingAttachmentBlob
        | FindingCode::OrphanedAttachmentBlob
        | FindingCode::AttachmentBlobMismatch
        | FindingCode::Unstructured => Ok(None),
    }
}

/// The edge a relation finding carries, as `(relation, other)`.
fn relation_payload(finding: &Finding) -> Result<(&str, &str), AppError> {
    match &finding.data {
        Some(FindingData::Relation { relation, other }) => Ok((relation, other)),
        _ => Err(incomplete(finding, "relation payload")),
    }
}

/// The story a finding's repair is written to.
fn remedy(finding: &Finding) -> Result<String, AppError> {
    finding
        .remedy
        .clone()
        .ok_or_else(|| incomplete(finding, "remedy"))
}

/// A finding that broke its producer's promise, as an error.
///
/// Loud rather than skipped: a repairable finding with no remedy or no payload
/// means this build's producer and its repair table disagree, which is exactly
/// the divergence SH-273 removed the duplicate walk to prevent. Silently
/// declining the repair would restore it, one finding at a time, and the
/// operator would see a report that never clears.
fn incomplete(finding: &Finding, missing: &str) -> AppError {
    AppError::Storage(format!(
        "internal: a {:?} finding carries no {missing}, so `story doctor --fix` cannot build the \
         repair its own report promised: {}",
        finding.code, finding.message
    ))
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
///
/// # Two readers, one walk (SH-273)
///
/// [`IntegrityService::repair`] reads this too, and derives its repairs from
/// what it returns rather than walking the same graph a second time. That is
/// why the checks live here rather than inside `examine`: report and `--fix`
/// are two halves of one contract — fix's postcondition is that report goes
/// clean — and every defect in that family (SH-225, SH-268, SH-271) was the
/// two halves computing the predicate separately and disagreeing. A filter
/// added here now applies to both by construction, which is precisely what
/// SH-268 found was not true of the one deleted above.
fn story_issues(
    tx: &impl ReadOps,
    project: ProjectId,
    prefix: &str,
    unattested: &BTreeSet<String>,
) -> Result<Vec<Finding>, AppError> {
    let types: BTreeSet<String> = tx
        .types(project)?
        .into_iter()
        .map(|story_type| story_type.slug)
        .collect();

    let stories = story_map(tx, project)?;
    let mut by_story = compute_integrity_issues(&stories, unattested);

    // SH-315: one project-wide query, not one per story — see
    // `ReadOps::attachment_blobs`'s own doc comment for why. `story_no` is
    // reparsed from each story's `id` below, matching `attachment_blobs`'
    // own keying.
    let mut blobs_by_story: BTreeMap<StoryNo, Vec<AttachmentBlobRow>> = BTreeMap::new();
    for (story_no, blob) in tx.attachment_blobs(project)? {
        blobs_by_story.entry(story_no).or_default().push(blob);
    }

    let mut issues = Vec::new();
    for story in stories.values() {
        // Rule 3 of [`compute_integrity_issues`], applied to the two checks
        // that live here rather than there: a row the events do not vouch for
        // answers no question about its own labels or its own type either.
        // Nothing is dropped by skipping ahead of the `remove` below — that
        // function never keys a finding on an unattested id.
        if unattested.contains(&story.id) {
            continue;
        }
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
        // A label written before the canonical write-path guard existed — a
        // comma-bearing one (unsplittable and unaddressable by
        // `story unlabel`/`list --label`), a blank/untrimmed one, or a
        // pre-SH-204 mixed-case spelling.
        // `--fix` repairs this on an open story; on a closed one it stays a
        // finding, the same as any other issue a closed story's history
        // cannot be appended to fix — and `--fix` names the story to reopen
        // rather than leaving the finding unexplained (SH-225).
        if story.labels != normalize_labels(&story.labels) {
            issues.push(
                Finding::new(
                    FindingCode::MalformedLabels,
                    format!(
                        "{}: malformed labels {:?} — labels must be lowercase, nonblank, and comma-delimited",
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

        // SH-315: the snapshot's own attachment list against the blob
        // table's rows for the same story. `story_no` reparses cleanly for
        // every story here — each came from a row this store minted, so a
        // parse failure would mean the snapshot's own `id` is corrupt, which
        // `ReadModelDivergence` already covers; skip rather than double-report.
        if let Ok(story_no) = StoryNo::parse_id(prefix, &story.id) {
            let blobs = blobs_by_story
                .get(&story_no)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let blob_by_id: BTreeMap<u32, &AttachmentBlobRow> = blobs
                .iter()
                .map(|blob| (blob.attachment_id, blob))
                .collect();
            for attachment in &story.attachments {
                match blob_by_id.get(&attachment.id) {
                    None => issues.push(
                        Finding::new(
                            FindingCode::MissingAttachmentBlob,
                            format!(
                                "{}: attachment {} (\"{}\") has no stored bytes",
                                story.id, attachment.id, attachment.name
                            ),
                        )
                        .about(&story.id),
                    ),
                    Some(blob)
                        if blob.byte_len != attachment.byte_len
                            || blob.sha256 != attachment.sha256 =>
                    {
                        issues.push(
                            Finding::new(
                                FindingCode::AttachmentBlobMismatch,
                                format!(
                                    "{}: attachment {} (\"{}\") disagrees with its stored bytes \
                                     — recorded {} bytes/sha256 {}, stored {} bytes/sha256 {}",
                                    story.id,
                                    attachment.id,
                                    attachment.name,
                                    attachment.byte_len,
                                    attachment.sha256,
                                    blob.byte_len,
                                    blob.sha256
                                ),
                            )
                            .about(&story.id),
                        );
                    }
                    Some(_) => {}
                }
            }
            let known_ids: BTreeSet<u32> = story.attachments.iter().map(|a| a.id).collect();
            for blob in blobs {
                if !known_ids.contains(&blob.attachment_id) {
                    issues.push(
                        Finding::new(
                            FindingCode::OrphanedAttachmentBlob,
                            format!(
                                "{}: stored bytes for attachment {} have no matching \
                                 attachment record",
                                story.id, blob.attachment_id
                            ),
                        )
                        .about(&story.id),
                    );
                }
            }
        }
    }

    // The disclosure the three suppression rules are conditional on (SH-286).
    // Appended after the per-story pass rather than woven into it because most
    // of this set has no row to be reached at, and one order for all of them
    // beats two.
    issues.extend(unattested.iter().map(|id| {
        Finding::new(
            FindingCode::UnexaminedStory,
            format!(
                "{id}: not examined — the events do not vouch for its read-model row, so its own \
                 label, type, parent and cycle checks were skipped, and no edge naming it was \
                 called dangling or one-sided. The finding naming the damage itself is elsewhere \
                 in this report"
            ),
        )
        .about(id)
    }));
    Ok(issues)
}

/// A set of story numbers as the ids a report prints (SH-269).
fn rendered_ids(stories: &BTreeSet<StoryNo>, prefix: &str) -> BTreeSet<String> {
    stories.iter().map(|story| story.to_id(prefix)).collect()
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

/// One line per story whose read-model row this build refused to judge (SH-410).
///
/// The companion to [`notice_issues`], and a different subject: that one is
/// about an *event* this build could not read, this one is about the *row* it
/// consequently cannot vouch for. A story reaches this list only when its
/// persisted row actually disagrees with the partial fold — a newer
/// storyhook's event that changed nothing this build stores produces the event
/// notice alone, because there is nothing to withhold.
///
/// Health-neutral, like every notice. That is not a softening of the verdict:
/// the disagreement is genuinely unattributable, since the row was very likely
/// written whole by the newer storyhook that also wrote the undecodable event.
/// A story whose incompleteness comes from a *torn* payload of a known kind is
/// separately unhealthy already — [`drift_issues`] mints an `UndecodableEvent`
/// finding for it — so nothing is lost by reporting the withheld fields here in
/// both cases.
fn unvouched_row_notices(drift: &crate::store::ReadModelDiff, prefix: &str) -> Vec<String> {
    let mut fields: BTreeMap<StoryNo, BTreeSet<String>> = BTreeMap::new();
    for divergence in &drift.unvouched {
        fields
            .entry(divergence.story_no)
            .or_default()
            .insert(divergence.field.clone());
    }
    for (story, relation, other) in &drift.unvouched_relations {
        fields
            .entry(*story)
            .or_default()
            .insert(format!("relation `{relation}` to {}", other.to_id(prefix)));
    }

    let mut causes: BTreeMap<StoryNo, BTreeSet<String>> = BTreeMap::new();
    for unknown in &drift.unknown_events {
        causes
            .entry(unknown.story_no)
            .or_default()
            .insert(format!("event {} (`{}`)", unknown.seq, unknown.kind));
    }

    fields
        .into_iter()
        .map(|(story, fields)| {
            let names: Vec<String> = fields.into_iter().collect();
            let because = causes
                .get(&story)
                .map(|c| c.iter().cloned().collect::<Vec<_>>().join(", "))
                .unwrap_or_default();
            format!(
                "{}: its read-model row was left exactly as found — {} field{} ({}) \
                 disagree{} with what this build can fold, and it cannot tell which side is \
                 right, because {} did not decode. Install a storyhook that knows {} and \
                 re-run `story doctor --fix`.",
                story.to_id(prefix),
                names.len(),
                if names.len() == 1 { "" } else { "s" },
                names.join(", "),
                if names.len() == 1 { "s" } else { "" },
                because,
                if causes.get(&story).is_some_and(|c| c.len() == 1) {
                    "it"
                } else {
                    "them"
                },
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
/// already has four OPEN states (`todo`, `in-progress`, `verifying`, `blocked`), so
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

/// A story whose `awaiting` reason names another story with nothing recording
/// it as a `blocked-by` edge (SH-398) — the detection layer for
/// [`crate::block_notice`]'s nudge, which only fires at the moment a reason
/// is written. A prose reason typed before this story existed, or edited by
/// hand, never passed through that nudge at all; this sweep is what still
/// finds it, project-wide, one notice per story in [`story_views`]'s own
/// ordering.
fn unlinked_blocker_notices(
    tx: &impl ReadOps,
    project: ProjectId,
) -> Result<Vec<String>, AppError> {
    Ok(story_views(tx, project, false)?
        .into_iter()
        .filter_map(|view| {
            let awaiting = view.story.awaiting.as_deref()?;
            let mentioned = crate::block_notice::unlinked_mentions_tx(
                tx,
                project,
                &view.story.id,
                awaiting,
                &view.story.relationships,
            )
            .unwrap_or_default();
            crate::block_notice::warning(&view.story.id, &mentioned)
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
/// (SH-285). The story map answers from the `stories` table, which is a fold
/// of the events and can be missing a row the events fully support — so a
/// **valid** edge naming that story read as dangling, and
/// [`IntegrityService::repair`] retracted it. The events cannot be missing in
/// that way: they are the thing a row is derived from.
///
/// This is asked only of an endpoint the story map could not find, and it is
/// an indexed `MAX(seq)` probe against the events table's primary key rather
/// than a fold, so it costs a lookup on a path that was already the unusual
/// one. On a healthy project it is never asked at all.
///
/// An id no [`StoryNo`] can be parsed out of answers **false**: it names no
/// story in this project, so there is nothing to look up and nothing to
/// preserve, and the edge is genuinely dangling. That branch is a guard rather
/// than a case with a fixture — `put_story` parses every relation it persists
/// and refuses the row outright, so no snapshot reaching this function can
/// carry one. Raising instead would turn a malformed id into a failure of the
/// whole repair run, which is the wrong answer to bad data.
pub(super) fn endpoint_exists(
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

/// The ids `stories` names by a relation, does not contain, and this project
/// nonetheless holds events for — `story_views`' share of SH-286's rule.
///
/// A **strict subset** of the set `story doctor` uses, and deliberately so.
/// `story doctor` derives its set from a whole-project diff it already pays for
/// ([`crate::store::ReadModelDiff::unattested`]); `story_views` runs on every
/// `story list`, `show`, `next` and `summary`, and charging each of those a
/// project-wide fold — or even an index scan of every story that has events —
/// to catch a fault the doctor exists to report would be paying on the healthy
/// path for the damaged one.
///
/// So this asks the narrower question that costs nothing when nothing is wrong:
/// only an endpoint some story's relation *names* and the map cannot show is a
/// candidate, and on a healthy project there are none, so not a single probe
/// runs and not even the project's prefix is read. What it can miss relative to
/// the doctor — an unattested story with a row, or one nothing points at — is
/// only ever a `flagged_reasons` entry the doctor would also have suppressed, so
/// `story show` may over-report against `story doctor` and can never
/// under-report. `tests/service_integrity.rs::story_views_never_flags_an_edge_
/// the_doctor_would_withhold` pins the direction.
pub(super) fn unattested_endpoints(
    tx: &impl ReadOps,
    project: ProjectId,
    stories: &BTreeMap<String, crate::domain::StorySnapshot>,
) -> Result<BTreeSet<String>, AppError> {
    let candidates: BTreeSet<&str> = stories
        .values()
        .flat_map(|story| story.relationships.iter())
        .map(|relation| relation.other_id.as_str())
        .filter(|other| !stories.contains_key(*other))
        .collect();
    if candidates.is_empty() {
        return Ok(BTreeSet::new());
    }

    let prefix = project_prefix(tx, project)?;
    let mut unattested = BTreeSet::new();
    for candidate in candidates {
        if endpoint_exists(tx, project, &prefix, candidate)? {
            unattested.insert(candidate.to_string());
        }
    }
    Ok(unattested)
}

/// The ids of the project's unarchived stories.
///
/// `--fix` appends to these and no others: an archived story's history is
/// closed, and appending a repair event to it would reopen a question the
/// project already settled. Membership is the *destination* test, not the
/// question test — see [`IntegrityService::repair`] for the difference, and
/// [`blocked_repairs_detail`] for what a repair with no open destination
/// becomes instead.
///
/// Ids alone, because that is the whole of what the destination test needs.
/// Every claim a repair is *built* from now arrives on the finding that asked
/// for it (SH-273), so the snapshots this used to carry beside them had no
/// remaining reader.
fn open_story_ids(tx: &impl ReadOps, project: ProjectId) -> Result<BTreeSet<String>, AppError> {
    Ok(tx
        .stories(project, &StoryQuery::all().archived(false))?
        .into_iter()
        .map(|row| row.snapshot.id)
        .collect())
}
