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
//! [`IntegrityService::notices`], which never contributes to [`report`] or
//! [`fix`]'s verdicts — see that method's doc comment for why.
//!
//! [`report`]: IntegrityService::report
//! [`fix`]: IntegrityService::fix

use std::collections::{BTreeMap, BTreeSet};

use crate::domain::finding::{Finding, FindingCode, FindingData};
use crate::domain::{
    STATE_ROLE_ACTIVE, StateDef, StoryEvent, StorySnapshot, TypeDef, active_state,
    compute_integrity_issues, inverse_relation, normalize_labels, validate_required_states,
    validate_type_slug,
};
use crate::error::{AppError, IntegrityDetail};
use crate::store::{
    ExpectedSeq, ProjectId, ReadOps, Store, StoryNo, StoryQuery, diff_read_model, repair_read_model,
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

    /// Every problem `story doctor` reports, in the order it reports them:
    /// the catalog first, then story-level findings, then read-model drift.
    ///
    /// The catalog leads because it is a property of the *project*, and because
    /// a project below the required-state floor cannot have its states edited
    /// at all until this is cleared — so it is the finding that explains the
    /// others' refusals.
    ///
    /// An empty vector means the project is healthy; the caller hands a
    /// non-empty one to [`IntegrityDetail::report`]. Deliberately excludes
    /// [`notices`](Self::notices): a project whose only anomaly is an event
    /// kind this build has never heard of is healthy by this definition,
    /// because that anomaly is a newer storyhook's data, not damage (SH-185).
    ///
    /// Every element carries the sentence it used to *be* (SH-244), so the
    /// rendered report is these findings' own messages joined and nothing
    /// re-renders them.
    pub fn report(&self) -> Result<Vec<Finding>, AppError> {
        // One transaction for both: a report whose catalog half and story half
        // came from different instants could name a state the other did not see.
        let mut issues = self.ctx.store().read(|tx| {
            let mut issues =
                catalog_issues(&tx.states(self.project())?, &tx.types(self.project())?);
            issues.extend(story_issues(tx, self.project())?);
            Ok(issues)
        })?;

        // The oracle the legacy read model never had. On a healthy project it
        // contributes nothing, which is why adding it does not move a single
        // byte of `doctor`'s existing output.
        let prefix = self
            .ctx
            .store()
            .read(|tx| project_prefix(tx, self.project()))?;
        issues.extend(drift_issues(
            &diff_read_model(self.ctx.store(), self.project())?,
            &prefix,
        ));
        Ok(issues)
    }

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
    /// contribute to [`report`](Self::report)'s health verdict,
    /// [`fix`](Self::fix)'s success or failure, or `story doctor`'s exit code
    /// — SH-185's council put the first one here specifically so it could
    /// not, and the other two follow the same reasoning
    /// [`crate::domain::with_required_states`] already gives for never
    /// awarding a role during a floor repair: which state should be active,
    /// or why a story is blocked, is not this command's to guess. The caller
    /// still owes all three visibility: see `notice_issues`'s doc comment.
    pub fn notices(&self) -> Result<Vec<String>, AppError> {
        let mut notices = notice_issues(&diff_read_model(self.ctx.store(), self.project())?);
        let (states, blocked) = self.ctx.store().read(|tx| {
            let states = tx.states(self.project())?;
            let blocked = blocked_without_reason_notices(tx, self.project())?;
            Ok((states, blocked))
        })?;
        notices.extend(active_state_notice(&states));
        notices.extend(blocked);
        Ok(notices)
    }

    /// Repairs what can be repaired, then reports what is left.
    ///
    /// Three things happen, in this order and for this reason:
    ///
    /// 1. **Missing inverse edges are written**, as events, on the story that
    ///    is missing them — the only layer that can fix an asymmetric history.
    /// 2. **Relations pointing at stories that do not exist are retracted**,
    ///    also as events.
    /// 3. **The read model is re-folded** from the resulting events, which
    ///    subsumes the legacy path's archived-snapshot repair and covers every
    ///    story rather than only the archived ones.
    ///
    /// Then [`report`](Self::report) runs again and its verdict is the
    /// command's: a repair that did not actually fix the project must not exit
    /// zero.
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
    /// `--fix` indistinguishable from a broken one: `report` kept naming a
    /// finding the operator had just told the doctor to fix, and nothing said
    /// that a manual reopen was the only way through. See
    /// [`blocked_repairs_detail`].
    ///
    /// # What it *undid*, it stops saying (SH-271)
    ///
    /// That list is decided in step 1 and printed after step 3, so step 3 can
    /// dissolve a finding step 1 wrote advice about: a story with events and no
    /// read-model row is absent from `all_stories`, which makes a *valid* edge
    /// naming it read as dangling — and the advice was then to reopen the
    /// claiming story and retract an edge the same run had just made whole.
    /// [`surviving_repairs`] reconciles the list against the post-repair
    /// findings, per entry.
    ///
    /// # What it *did*, it says — including when it failed (SH-266)
    ///
    /// The message and the failed run's `advice` are one list, assembled
    /// before the verdict. They used to be two, and the failing half carried a
    /// subset: a repair this command really made — states added back to a
    /// catalog below the floor — went unmentioned whenever anything else
    /// remained, which reads as "nothing happened" to an operator who then
    /// repeats it.
    pub fn fix(&self) -> Result<String, AppError> {
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

        let (touched, blocked) = self.ctx.store().write(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let states = tx.state_map(project)?;
            let all = all_stories(&*tx, project)?;
            let open = open_stories(&*tx, project)?;
            let mut touched: BTreeSet<String> = BTreeSet::new();
            let mut blocked: BTreeSet<BlockedRepair> = BTreeSet::new();

            for (id, story) in &all {
                // Each candidate carries the event, the imperative sentence
                // describing it, and the finding it answers: a story too closed
                // to be appended to owes the operator the sentence rather than
                // the event, and owes this run's own verdict the finding.
                let mut own_repairs: Vec<OwnRepair> = Vec::new();
                for relation in &story.relationships {
                    let claim = FindingKey::Edge {
                        claimant: id.clone(),
                        relation: relation.relation.clone(),
                        other: relation.other_id.clone(),
                    };
                    let Some(other) = all.get(&relation.other_id) else {
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
                            cause: claim,
                        });
                        continue;
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
                            cause: claim,
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
                        cause: FindingKey::Labels {
                            story: id.clone(),
                            labels: story.labels.clone(),
                        },
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
                        cause: candidate.cause,
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
            Ok((touched, blocked))
        })?;

        let repair = repair_read_model(self.ctx.store(), project)?;
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

        let remaining = self.report()?;
        let notices = self.notices()?;
        // Everything `blocked` says was decided before `repair_read_model` ran,
        // against the read model it has since repaired — so an entry can name a
        // finding this very run dissolved (SH-271). Reconciling drops those and
        // keeps the rest, per entry: a run that repairs one thing must not fall
        // silent about another it could not.
        let blocked = surviving_repairs(blocked, &remaining);
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
                    .map(|(story, reason)| format!("{story}: {reason}"))
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

        if let Some(detail) = IntegrityDetail::report(remaining, advice.clone()) {
            return Err(AppError::Integrity(detail));
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
        Ok(std::iter::once(headline.to_string())
            .chain(advice)
            .collect::<Vec<_>>()
            .join("\n"))
    }

    fn project(&self) -> ProjectId {
        self.ctx.project()
    }
}

/// A repair [`IntegrityService::fix`] identified, could not make, and owes the
/// operator a sentence about (SH-225).
///
/// It carries the finding it would have answered because the list is built
/// *before* the read-model repair and rendered *after* it — see
/// [`surviving_repairs`].
///
/// Ordered by the story to reopen, then the repair, which is the order
/// [`blocked_repairs_detail`] prints; `cause` is last so it never reorders a
/// report an operator reads.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct BlockedRepair {
    /// The closed story whose history blocks the repair — the one to reopen.
    reopen: String,
    /// The imperative sentence describing what would have been written.
    repair: String,
    /// The finding this repair exists to clear.
    cause: FindingKey,
}

/// A repair that appends to the story that raised it, before that story's state
/// decides whether it is written or [`blocked`](BlockedRepair).
struct OwnRepair {
    /// What would be appended.
    event: StoryEvent,
    /// The sentence describing it, for the operator who has to make it happen
    /// by hand.
    repair: String,
    /// The finding it answers.
    cause: FindingKey,
}

/// A finding keyed by the fact it reports rather than by the sentence it
/// prints.
///
/// Two producers have to agree on it: the repair loop, which knows the claim it
/// is about to skip, and [`report`](IntegrityService::report), which recomputes
/// findings from the repaired read model. Keying on the sentence would make
/// them agree by string formatting; keying on the *fact* is why they agree at
/// all — the same values `compute_integrity_issues` puts in
/// [`Finding::subject`] and [`FindingData`].
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum FindingKey {
    /// An edge as its claiming end records it. The [`FindingCode`] is
    /// deliberately not part of the key: one claim produces at most one finding
    /// — dangling *or* missing-inverse, never both — so the code adds nothing,
    /// while including it would split a key over
    /// [`FindingCode::MissingReciprocalRelation`], which is the same fact
    /// spelled for a mutual relation.
    Edge {
        /// The story whose history asserts the edge, and which the finding is
        /// reported against.
        claimant: String,
        /// The relation it asserts.
        relation: String,
        /// The story at the far end.
        other: String,
    },
    /// A story's unrepaired label set (SH-164).
    Labels {
        /// The story carrying them.
        story: String,
        /// The labels, exactly as stored.
        labels: Vec<String>,
    },
}

/// The fact a finding reports, when it is one a blocked repair can answer.
///
/// `None` for every other finding — a cycle, an unknown type, read-model drift
/// — none of which [`IntegrityService::fix`] ever puts in its blocked list, so
/// none of which can keep an entry alive.
fn finding_key(finding: &Finding) -> Option<FindingKey> {
    let subject = finding.subject.clone()?;
    match (finding.code, &finding.data) {
        (
            FindingCode::DanglingRelation
            | FindingCode::MissingInverseRelation
            | FindingCode::MissingReciprocalRelation,
            Some(FindingData::Relation { relation, other }),
        ) => Some(FindingKey::Edge {
            claimant: subject,
            relation: relation.clone(),
            other: other.clone(),
        }),
        (FindingCode::MalformedLabels, Some(FindingData::Labels { labels })) => {
            Some(FindingKey::Labels {
                story: subject,
                labels: labels.clone(),
            })
        }
        _ => None,
    }
}

/// The blocked repairs still worth advising, given what the run left behind.
///
/// SH-271: `blocked` is computed inside the write that repairs *stories*, and
/// `repair_read_model` runs after it. A story with events and no read-model row
/// is absent from `all_stories`, so a perfectly valid edge naming it reads as
/// dangling — and when the claiming story is closed, the run advises reopening
/// it to retract an edge the same run then makes whole again. Following that
/// advice destroys good data.
///
/// So an entry whose finding is gone from the post-repair report is dropped.
/// Dropping is per entry rather than wholesale, because a run that repairs one
/// thing must not fall silent about another it could not: that silence is
/// exactly the SH-225 defect this list exists to end.
///
/// The remaining findings are the oracle rather than a second walk of the
/// graph, which is as close to SH-273's fix as this change goes: `report` is
/// already the authority on what is wrong with the project, so asking it is the
/// only way `fix` can be sure it is not answering a question that no longer
/// exists.
fn surviving_repairs(
    blocked: BTreeSet<BlockedRepair>,
    remaining: &[Finding],
) -> BTreeSet<BlockedRepair> {
    let live: BTreeSet<FindingKey> = remaining.iter().filter_map(finding_key).collect();
    blocked
        .into_iter()
        .filter(|entry| live.contains(&entry.cause))
        .collect()
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
/// never consulted the filter at all: it repaired these edges while `report`
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
/// # `subject` is the rendered id; the sentence keeps the bare number
///
/// These sentences say `story 41:` while every story-level one says `SH-41:` —
/// one command with two names for one story. The prose is left exactly as it
/// was, because it is pinned by golden snapshots and changing it here would be
/// two hats in one commit; `subject` carries the id every other surface
/// speaks, so a consumer never has to know the difference. The inconsistency
/// itself is filed separately.
fn drift_issues(drift: &crate::store::ReadModelDiff, prefix: &str) -> Vec<Finding> {
    let id = |story: &StoryNo| story.to_id(prefix);
    let mut lines = Vec::new();
    for story in &drift.missing_rows {
        lines.push(
            Finding::new(
                FindingCode::MissingRow,
                format!("story {story}: has events but no read-model row"),
            )
            .about(id(story)),
        );
    }
    for story in &drift.extra_rows {
        lines.push(
            Finding::new(
                FindingCode::ExtraRow,
                format!("story {story}: read-model row with no events"),
            )
            .about(id(story)),
        );
    }
    for (story, reason) in &drift.fold_failures {
        lines.push(
            Finding::new(
                FindingCode::FoldFailure,
                format!("story {story}: cannot be folded: {reason}"),
            )
            .about(id(story))
            .carrying(FindingData::Reason {
                reason: reason.clone(),
            }),
        );
    }
    for divergence in &drift.divergences {
        lines.push(
            Finding::new(
                FindingCode::ReadModelDivergence,
                format!(
                    "story {}: {} is `{}` but the events say `{}`",
                    divergence.story_no, divergence.field, divergence.persisted, divergence.rebuilt
                ),
            )
            .about(id(&divergence.story_no))
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
            lines.push(
                Finding::new(
                    FindingCode::UndecodableEvent,
                    format!(
                        "story {}: event {} is a `{}` this build cannot decode — retained \
                         verbatim, but not folded",
                        unknown.story_no, unknown.seq, unknown.kind
                    ),
                )
                .about(id(&unknown.story_no))
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
/// [`IntegrityService::fix`]'s success or failure, or `story doctor`'s exit
/// code — the caller decides separately how (never *whether*) to surface it,
/// because dropping it silently just because it lost a seat in the health
/// vector would be its own regression.
fn notice_issues(drift: &crate::store::ReadModelDiff) -> Vec<String> {
    drift
        .unknown_events
        .iter()
        .filter(|unknown| !crate::domain::is_known_event_kind(&unknown.kind))
        .map(|unknown| {
            format!(
                "story {}: event {} is of kind `{}`, which this build does not know — retained \
                 verbatim and carried by `story export`, but not folded. A newer storyhook wrote \
                 it.",
                unknown.story_no, unknown.seq, unknown.kind
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
/// question test — see [`IntegrityService::fix`] for the difference, and
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
