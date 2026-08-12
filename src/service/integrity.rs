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
//! * **story-level integrity** — the legacy checks, reproduced verbatim,
//!   including the two flag kinds `doctor` deliberately suppresses; and
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

use crate::domain::{
    STATE_ROLE_ACTIVE, StateDef, StoryEvent, StorySnapshot, TypeDef, active_state,
    inverse_relation, normalize_labels, validate_required_states, validate_type_slug,
};
use crate::error::AppError;
use crate::store::{
    ExpectedSeq, ProjectId, ReadOps, Store, StoryNo, StoryQuery, diff_read_model, repair_read_model,
};

use super::state_set::write_states_repairing;
use super::{Ctx, append_and_fold, project_prefix, query::story_views};

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
    /// An empty vector means the project is healthy; the caller turns a
    /// non-empty one into [`AppError::Integrity`]. Deliberately excludes
    /// [`notices`](Self::notices): a project whose only anomaly is an event
    /// kind this build has never heard of is healthy by this definition,
    /// because that anomaly is a newer storyhook's data, not damage (SH-185).
    pub fn report(&self) -> Result<Vec<String>, AppError> {
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
        issues.extend(drift_issues(&diff_read_model(
            self.ctx.store(),
            self.project(),
        )?));
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
    /// # One deliberate divergence from the legacy repair
    ///
    /// "Does the other end exist?" is asked of **every** story, where the
    /// legacy path asked it of the open ones only. That difference is data
    /// loss: relate two stories, close or delete one of them, and legacy
    /// `doctor --fix` retracts the survivor's edges — because the archived
    /// counterpart is not in the open set — and then reports the asymmetry it
    /// just created as an integrity failure the command can never clear.
    /// Appends still go to open stories only, which is the part of that rule
    /// that was right.
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

        let touched = self.ctx.store().write(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let states = tx.state_map(project)?;
            let all = all_stories(&*tx, project)?;
            let open = open_stories(&*tx, project)?;
            let mut touched: BTreeSet<String> = BTreeSet::new();

            for (id, story) in &open {
                let mut own_events: Vec<StoryEvent> = Vec::new();
                for relation in &story.relationships {
                    let Some(other) = all.get(&relation.other_id) else {
                        own_events.push(StoryEvent::StoryRelationshipRemoved {
                            at: now.clone(),
                            other_id: relation.other_id.clone(),
                            relation: relation.relation.clone(),
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
                    // on one is a finding, not something to append to.
                    if !open.contains_key(&relation.other_id) {
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
                // a fresh event. Only reachable on an open story — the loop
                // this sits in is `open` only — so a malformed label on a
                // closed story stays a finding `report` keeps naming, the
                // same as any other issue a closed story cannot be repaired
                // out of.
                let normalized_labels = normalize_labels(&story.labels);
                if normalized_labels != story.labels {
                    own_events.push(StoryEvent::StoryLabelsSet {
                        at: now.clone(),
                        labels: normalized_labels,
                    });
                }

                if !own_events.is_empty() {
                    let story_no = StoryNo::parse_id(&prefix, id)
                        .map_err(|error| AppError::Storage(format!("unparseable id: {error}")))?;
                    append_and_fold(
                        tx,
                        project,
                        story_no,
                        &prefix,
                        &states,
                        ExpectedSeq::Any,
                        &own_events,
                        self.ctx.provenance(),
                    )?;
                    touched.insert(id.clone());
                }
            }
            Ok(touched)
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

        let remaining = self.report()?;
        let notices = self.notices()?;
        if !remaining.is_empty() {
            return Err(AppError::Integrity(detail_with_notices(
                &remaining, &notices,
            )));
        }

        let mut message = if touched.is_empty() && states_added == 0 {
            "doctor found nothing to fix".to_string()
        } else {
            "doctor repaired supported integrity issues".to_string()
        };
        if states_added > 0 {
            message.push_str(&format!(
                "\nadded {states_added} required {} this project was missing",
                if states_added == 1 { "state" } else { "states" }
            ));
        }
        if !repair.unrepairable.is_empty() {
            message.push_str(&format!(
                "\n{} stor{} could not be repaired:\n{}",
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
            message.push_str(&format!(
                "\n{} notice{} — nothing to fix, by design:\n{}",
                notices.len(),
                if notices.len() == 1 { "" } else { "s" },
                notices.join("\n")
            ));
        }
        Ok(message)
    }

    fn project(&self) -> ProjectId {
        self.ctx.project()
    }
}

/// Whether the project's catalog meets the required-state floor (SH-125).
///
/// One finding, not one per missing state: the remedy is the same command
/// however many are missing, and [`validate_required_states`] already names
/// them all in its own sentence.
fn catalog_issues(states: &[StateDef], types: &[TypeDef]) -> Vec<String> {
    let mut issues: Vec<String> = validate_required_states(states)
        .err()
        .map(|error| error.to_string())
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
            format!(
                "type `{}` cannot be addressed: {error}. Retype its stories (`story set <id> \
                 --type none`) and remove it with `story type remove -- '{}'`",
                story_type.slug, story_type.slug
            )
        })
    }));
    issues
}

/// The legacy story-level checks, reproduced exactly.
///
/// Two flag kinds are deliberately suppressed, as they always have been:
/// `obviated` (a deliberate authoring decision, not damage) and `conflicts`
/// (advisory). The unknown-type check is separate because it is a property of
/// the story *and the catalog*, which the cross-story integrity pass does not
/// see.
fn story_issues(tx: &impl ReadOps, project: ProjectId) -> Result<Vec<String>, AppError> {
    let types: BTreeSet<String> = tx
        .types(project)?
        .into_iter()
        .map(|story_type| story_type.slug)
        .collect();

    let mut issues = Vec::new();
    for view in story_views(tx, project, false)? {
        for issue in view.flagged_reasons {
            if issue.contains("obviated") || issue.contains("conflicts") {
                continue;
            }
            issues.push(format!("{}: {}", view.story.id, issue));
        }
        if let Some(slug) = &view.story.story_type
            && !types.contains(slug)
        {
            issues.push(format!("{}: unknown type `{slug}`", view.story.id));
        }
        // A label written before SH-164's write-path guard existed — a
        // comma-bearing one (unsplittable and unaddressable by
        // `story unlabel`/`list --label`) or a blank/untrimmed one.
        // `--fix` repairs this on an open story; on a closed one it stays a
        // finding, the same as any other issue a closed story's history
        // cannot be appended to fix.
        if view.story.labels != normalize_labels(&view.story.labels) {
            issues.push(format!(
                "{}: malformed labels {:?} — a label cannot contain a comma or be blank",
                view.story.id, view.story.labels
            ));
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
fn drift_issues(drift: &crate::store::ReadModelDiff) -> Vec<String> {
    let mut lines = Vec::new();
    for story in &drift.missing_rows {
        lines.push(format!("story {story}: has events but no read-model row"));
    }
    for story in &drift.extra_rows {
        lines.push(format!("story {story}: read-model row with no events"));
    }
    for (story, reason) in &drift.fold_failures {
        lines.push(format!("story {story}: cannot be folded: {reason}"));
    }
    for divergence in &drift.divergences {
        lines.push(format!(
            "story {}: {} is `{}` but the events say `{}`",
            divergence.story_no, divergence.field, divergence.persisted, divergence.rebuilt
        ));
    }
    for unknown in &drift.unknown_events {
        if crate::domain::is_known_event_kind(&unknown.kind) {
            lines.push(format!(
                "story {}: event {} is a `{}` this build cannot decode — retained verbatim, but \
                 not folded",
                unknown.story_no, unknown.seq, unknown.kind
            ));
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

/// Renders `report()`'s findings for display when the run is unhealthy,
/// appending [`notices`](IntegrityService::notices) after them.
///
/// The reason this exists at all: a notice plays no part in deciding whether
/// the run is healthy, but a project can carry a notice **and** a real finding
/// in the same run, and the notice still owes the caller visibility — SH-185's
/// council was explicit that excluding it from the health vector must not
/// mean excluding it from the output entirely. `story doctor`'s plain report
/// path and `--fix`'s failure path both need exactly this rendering, so it is
/// shared rather than duplicated.
pub fn detail_with_notices(issues: &[String], notices: &[String]) -> String {
    let mut detail = issues.join("\n");
    if !notices.is_empty() {
        detail.push('\n');
        detail.push_str(&notices.join("\n"));
    }
    detail
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
/// `--fix` has always worked on open stories only: an archived story's history
/// is closed, and appending a repair event to it would reopen a question the
/// project already settled.
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
