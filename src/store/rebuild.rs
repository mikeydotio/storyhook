//! The rebuild-and-diff oracle: re-fold every story from its events and say
//! where the persisted read model disagrees.
//!
//! This is one facility with two jobs, and that is deliberate. It is the
//! implementation of `story doctor`'s integrity check, and it is the oracle
//! every store and service test measures itself against. A drift detector that
//! is only ever exercised by tests rots; one that users run keeps working. The
//! previous design had `repair_archived_snapshots`, which could only fix one
//! known shape of divergence and could not report any other.
//!
//! It is also the one place in `store/` that folds. Everywhere else the fold
//! stays in [`crate::domain`] and the caller performs it — but an oracle that
//! reused the caller's folded result would be checking nothing. Here the
//! independence *is* the value.

use std::collections::{BTreeMap, BTreeSet};

use crate::domain::{StorySnapshot, fold_story};
use crate::store::error::StoreError;
use crate::store::ids::{EventSeq, GlobalSeq, ProjectId, StoryNo};
use crate::store::types::{StoryQuery, StoryRow, UnknownEventDiagnostic, partition_known};
use crate::store::{ReadOps, Store, WriteOps};

/// How many change-feed entries a rebuild reads at a time.
const FEED_PAGE: u32 = 4096;

/// Counts project-wide re-folds, so a test can pin how many one command
/// performs (SH-267).
///
/// A fold is invisible from outside: it reads, it allocates, it produces the
/// same answers whether it ran once or four times. That is precisely why
/// `story doctor` re-folded the whole project twice per read and four times per
/// `--fix` for as long as it did — no output moved, so nothing said so. A
/// caller sees results, not work, and this is the only place the work is
/// countable at all.
///
/// Gated behind `test-seam` — the same mechanism, and the same reasoning, as
/// [`crate::store::fault`]'s crash points: with the feature off this module
/// does not exist and [`note_fold`] is an empty inlined function, so a release
/// binary carries no counter. The alternative was a `Store` decorator counting
/// [`ReadOps::events_since`] from outside, which is ~70 delegating methods of
/// test-support to observe a number the fold already knows.
///
/// Thread-local, because integration tests share a process: a process-global
/// counter would make one test's fold another test's failure.
#[cfg(feature = "test-seam")]
pub mod folds {
    use std::cell::Cell;

    thread_local! {
        static FOLDS: Cell<usize> = const { Cell::new(0) };
    }

    /// Runs `f` and reports how many project-wide folds it performed on this
    /// thread.
    ///
    /// Scoped rather than a `reset`/`read` pair, so a test cannot forget to
    /// zero the counter and measure whatever ran before it. Nesting composes:
    /// an inner scope's folds are counted by the outer one too.
    pub fn counting<T>(f: impl FnOnce() -> T) -> (T, usize) {
        let outer = FOLDS.replace(0);
        let value = f();
        let counted = FOLDS.get();
        FOLDS.set(outer + counted);
        (value, counted)
    }

    /// Records one project-wide fold.
    pub(super) fn note() {
        FOLDS.set(FOLDS.get() + 1);
    }
}

/// Records that a project-wide fold happened; compiles to nothing without the
/// `test-seam` feature. See [`folds`](self::folds) for why it exists.
#[inline]
fn note_fold() {
    #[cfg(feature = "test-seam")]
    folds::note();
}

/// One story as the events say it should be.
#[derive(Clone, Debug)]
pub struct RebuiltStory {
    /// The story's number.
    pub story_no: StoryNo,
    /// The event sequence the rebuild folded up to.
    pub head_seq: EventSeq,
    /// The change-feed position of the same event `head_seq` names (SH-336) —
    /// see [`crate::store::types::StoryRow::head_global_seq`].
    pub head_global_seq: GlobalSeq,
    /// The snapshot the fold produced, or why it could not.
    ///
    /// A fold failure is reported rather than propagated: one unfoldable story
    /// must not stop a project-wide integrity check, which is exactly when you
    /// most want the report.
    pub snapshot: Result<StorySnapshot, String>,
    /// Events skipped because this binary does not know their kind.
    pub unknown_events: Vec<UnknownEventDiagnostic>,
}

impl RebuiltStory {
    /// Whether every event this story has was folded into
    /// [`snapshot`](Self::snapshot).
    ///
    /// When this is false the snapshot is still `Ok` — the fold succeeded on
    /// what it could read — but it is a fold of *some* of the story, and
    /// nothing may be concluded from a comparison against it (SH-410). The
    /// store deliberately does not ask **why** an event was skipped: a kind
    /// from the future and a torn payload of a known kind make the fold
    /// equally partial, and which of the two it was is a policy question
    /// [`crate::service::IntegrityService`] answers with
    /// [`crate::domain::is_known_event_kind`]. Keeping that question out of
    /// here is what keeps SH-185's damage-versus-data-from-the-future split in
    /// one place.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.unknown_events.is_empty()
    }
}

/// [`Divergence::field`] for the whole embedded snapshot — the column
/// `service::query::story_map` builds every story-level integrity check from.
///
/// Named once so [`diff_rebuilt`], which produces the divergence, and
/// [`ReadModelDiff::unattested`], which singles it out of the rest, cannot come
/// to mean different columns by the same string.
const SNAPSHOT_FIELD: &str = "snapshot";

/// What a comparison's *rebuilt* side was derived from.
///
/// The distinction only matters for a story whose fold is incomplete
/// ([`RebuiltStory::is_complete`]), and there it decides everything: a value
/// this build folded from a subset of the events proves nothing about the row,
/// while a value read straight off the event list proves exactly as much as it
/// always did.
///
/// [`rebuild`] reads `head_seq` and `head_global_seq` from `events.last()`
/// **before** [`partition_known`] runs, so those two are facts about the log
/// rather than about the fold. A row behind them is stale whatever this build
/// can decode, which is the one damage class SH-410's withholding must not
/// blind the doctor to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Basis {
    /// Derived from the raw event list, independently of what folded.
    Events,
    /// Derived from the folded snapshot, and therefore only as complete as it.
    Fold,
}

/// One disagreement between the persisted read model and the events.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Divergence {
    /// The story it concerns.
    pub story_no: StoryNo,
    /// Which field, or `"<row>"` for a missing or extra row.
    pub field: String,
    /// What the read model holds.
    pub persisted: String,
    /// What the events say it should hold.
    pub rebuilt: String,
}

/// The result of comparing a project's read model against its events.
#[derive(Clone, Debug, Default)]
pub struct ReadModelDiff {
    /// Field-level disagreements, ordered by story then field.
    pub divergences: Vec<Divergence>,
    /// Stories with events but no read-model row.
    pub missing_rows: Vec<StoryNo>,
    /// Read-model rows with no events behind them.
    pub extra_rows: Vec<StoryNo>,
    /// Stories carrying at least one event this binary cannot decode.
    pub unknown_events: Vec<UnknownEventDiagnostic>,
    /// Stories whose events could not be folded at all, with the reason.
    pub fold_failures: Vec<(StoryNo, String)>,
    /// Edges one story's history asserts and the other's does not.
    ///
    /// The SH-60 defect at the level it actually lives: in the *events*. The
    /// relations table is symmetric by construction, so queries are unaffected
    /// and no repair of the read model can help — the missing half is a missing
    /// event, and only the layer that writes events can add it. Reported as
    /// `(claimant, relation, other)`.
    pub asymmetric_relations: Vec<(StoryNo, String, StoryNo)>,
    /// Disagreements this build is **not entitled to call damage** (SH-410).
    ///
    /// Same shape as [`divergences`](Self::divergences), and computed by the
    /// same comparison — the difference is who the disagreement is evidence
    /// against. These belong to a story whose fold is incomplete
    /// ([`RebuiltStory::is_complete`]), so the rebuilt side is a fold of only
    /// part of the history and the persisted row may well be the *more*
    /// authoritative artifact: written whole by the newer storyhook that also
    /// wrote the event this build cannot read.
    ///
    /// They are a separate vector rather than a flag on `Divergence` because
    /// every reader of `divergences` draws a conclusion from it —
    /// [`is_clean`](Self::is_clean), [`unattested`](Self::unattested),
    /// `service::integrity`'s `drift_issues`, and the `touched` set that mints
    /// `--fix`'s headline. A filter would have to be remembered at each, which
    /// is the drift this project has already paid for five times (SH-136,
    /// SH-198, SH-258, SH-260/276, SH-364). Routing at the point of
    /// construction makes all four correct with nothing to remember.
    pub unvouched: Vec<Divergence>,
    /// Asymmetric edges claimed by a story whose fold is incomplete.
    ///
    /// [`asymmetric_relations`](Self::asymmetric_relations) is what
    /// [`has_integrity_issues`](Self::has_integrity_issues) answers on, and the
    /// missing half of an edge may be exactly the event this build could not
    /// decode. Reported, never counted as damage.
    pub unvouched_relations: Vec<(StoryNo, String, StoryNo)>,
}

impl ReadModelDiff {
    /// Whether the read model agrees with the events it was folded from.
    ///
    /// This is the *repairable* question — everything it covers is something
    /// [`repair_read_model`] can put right by re-folding, so a repair followed
    /// by a diff is clean by construction.
    ///
    /// Two findings are deliberately outside it. Unknown event kinds are not
    /// damage: retaining an event this binary does not understand is correct
    /// behaviour. Asymmetric relations are damage, but not to the read model —
    /// see [`ReadModelDiff::asymmetric_relations`] and
    /// [`ReadModelDiff::has_integrity_issues`].
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.divergences.is_empty()
            && self.missing_rows.is_empty()
            && self.extra_rows.is_empty()
            && self.fold_failures.is_empty()
    }

    /// Whether the project has problems a re-fold cannot fix.
    ///
    /// `story doctor` reports both questions; only [`is_clean`](Self::is_clean)
    /// is the one a `--repair` can answer.
    #[must_use]
    pub fn has_integrity_issues(&self) -> bool {
        !self.asymmetric_relations.is_empty() || !self.fold_failures.is_empty()
    }

    /// Every story in this project the events do not corroborate — the set
    /// `story doctor`'s story-level checks may assert nothing from (SH-286).
    ///
    /// The doctor's story-level checks read the `stories` table, which is a
    /// cache of a fold of the events. This is every way this diff has found
    /// that cache untrustworthy for one story, and the four are here rather
    /// than the one SH-286 was filed about because each defeats those checks
    /// the same way:
    ///
    /// * [`missing_rows`](Self::missing_rows) — the events describe a story the
    ///   checks cannot see at all. The case SH-285 found: its inbound edges are
    ///   valid and read as dangling.
    /// * [`fold_failures`](Self::fold_failures) — the events will not fold, so
    ///   nothing derives a row. **Whether or not one is still sitting there:**
    ///   [`diff_rebuilt`] files a story here and moves on *before* it looks for
    ///   a row, so a stale one survives untouched, and a check computed from it
    ///   is computed from a snapshot this very run failed to reproduce.
    /// * [`extra_rows`](Self::extra_rows) — a row no events back. It is not a
    ///   story at all by the authority that decides, yet it sits in the map,
    ///   where it silently blesses every edge naming it. The SH-285 defect
    ///   inverted.
    /// * [`divergences`](Self::divergences) on the `snapshot` field — the story
    ///   map is built from that column (`service::query::story_map` reads
    ///   `row.snapshot`), so this is a row the checks compute *every* finding
    ///   from and the same run proves wrong. Only that field: a `title` or
    ///   `state` column the fold disagrees with is drift the checks never read,
    ///   and widening this to all divergences would take a badly drifted
    ///   project's whole report down to `UnexaminedStory`, which is the SH-268
    ///   shape of going quiet.
    ///
    /// Ordered and deduplicated — a story can reach this by more than one route,
    /// and it is named once however many.
    #[must_use]
    pub fn unattested(&self) -> BTreeSet<StoryNo> {
        self.missing_rows
            .iter()
            .copied()
            .chain(self.fold_failures.iter().map(|(story, _)| *story))
            .chain(self.extra_rows.iter().copied())
            .chain(
                self.divergences
                    .iter()
                    .filter(|divergence| divergence.field == SNAPSHOT_FIELD)
                    .map(|divergence| divergence.story_no),
            )
            .collect()
    }

    /// A human-readable summary, one line per finding.
    #[must_use]
    pub fn describe(&self) -> String {
        let mut lines = Vec::new();
        for story in &self.missing_rows {
            lines.push(format!("story {story}: has events but no read-model row"));
        }
        for story in &self.extra_rows {
            lines.push(format!("story {story}: read-model row with no events"));
        }
        for (story, reason) in &self.fold_failures {
            lines.push(format!("story {story}: cannot be folded: {reason}"));
        }
        for (story, relation, other) in &self.asymmetric_relations {
            lines.push(format!(
                "story {story}: claims `{relation}` to story {other}, whose history does not \
                 claim the inverse"
            ));
        }
        for divergence in &self.divergences {
            lines.push(format!(
                "story {}: {} is `{}` but the events say `{}`",
                divergence.story_no, divergence.field, divergence.persisted, divergence.rebuilt
            ));
        }
        lines.join("\n")
    }
}

/// What [`repair_read_model`] changed.
#[derive(Clone, Debug, Default)]
pub struct RepairReport {
    /// Stories whose read-model row was rewritten.
    pub rewritten: Vec<StoryNo>,
    /// The divergences that were found before the repair.
    pub repaired: ReadModelDiff,
    /// Stories that could not be repaired because their events do not fold.
    pub unrepairable: Vec<(StoryNo, String)>,
    /// Stories whose row was deliberately **left as found** (SH-410), with the
    /// events that made this build's fold of them incomplete.
    ///
    /// Not [`unrepairable`](Self::unrepairable), and the distinction is the
    /// whole point: an unrepairable story's events do not fold at all, so there
    /// is no trustworthy row and nothing may be concluded from the one sitting
    /// there. A withheld story's row is intact and is very likely *more*
    /// authoritative than this build's fold — written whole by the newer
    /// storyhook that also wrote the event this build cannot read. Since
    /// [`unattested`](Self::unattested) is by definition the set the doctor's
    /// story-level checks may assert nothing from, filing a withheld story
    /// there would skip the checks that are still perfectly valid on it.
    pub withheld: Vec<(StoryNo, Vec<UnknownEventDiagnostic>)>,
}

impl RepairReport {
    /// Every story the events still do not corroborate **after** this repair —
    /// what [`ReadModelDiff::unattested`] would answer if it were asked again
    /// now (SH-286).
    ///
    /// Two members, and the reason is what the repair could and could not do:
    ///
    /// * [`unrepairable`](Self::unrepairable) — the fold failures, which a
    ///   re-fold by definition cannot clear; and
    /// * [`repaired`](Self::repaired)`.extra_rows` — a row with no events, which
    ///   re-folding does not remove either, so it survives the repair unchanged.
    ///
    /// Everything else in `repaired` is *gone*: missing rows were written and
    /// divergent ones rewritten, which is the whole of what this repair does.
    /// Reading the rest of that field would be reading the damage as it was
    /// **before** the repair and skipping checks on the very rows the repair had
    /// just restored — the label a story could not be repaired of because the
    /// same run had already fixed the row carrying it
    /// (`tests/service_integrity.rs::one_fix_run_repairs_a_label_on_a_story_
    /// whose_row_it_had_to_restore`).
    #[must_use]
    pub fn unattested(&self) -> BTreeSet<StoryNo> {
        self.unrepairable
            .iter()
            .map(|(story, _)| *story)
            .chain(self.repaired.extra_rows.iter().copied())
            .collect()
    }
}

/// Re-folds every story in a project from its events.
///
/// Reads only, and works from a [`ReadOps`] rather than a whole [`Store`] so
/// that a repair can rebuild and rewrite inside one transaction.
pub fn rebuild(tx: &impl ReadOps, project: ProjectId) -> Result<Vec<RebuiltStory>, StoreError> {
    note_fold();
    let states = tx.state_map(project)?;
    let prefix = tx
        .project(project)?
        .ok_or_else(|| StoreError::NotFound(format!("project {project} does not exist")))?
        .prefix;

    // Grouped out of the change feed rather than read per story: the feed is
    // already every event in the project, in a total order, and paging it costs
    // one query per page instead of one per story.
    let mut by_story: BTreeMap<StoryNo, Vec<crate::store::types::StoredEvent>> = BTreeMap::new();
    let mut cursor = GlobalSeq::ZERO;
    loop {
        let page = tx.events_since(project, cursor, FEED_PAGE)?;
        if page.is_empty() {
            break;
        }
        for entry in page {
            cursor = entry.event.global_seq;
            by_story
                .entry(entry.story_no)
                .or_default()
                .push(entry.event);
        }
    }

    let mut rebuilt = Vec::with_capacity(by_story.len());
    for (story_no, mut events) in by_story {
        // The feed is ordered by `global_seq`, which is project-wide; a single
        // story's events must be folded in *its* order.
        events.sort_by_key(|event| event.seq);
        let head_seq = events.last().map_or(EventSeq::ZERO, |event| event.seq);
        let head_global_seq = events
            .last()
            .map_or(GlobalSeq::ZERO, |event| event.global_seq);
        let (known, unknown_events) = partition_known(story_no, &events);
        let id = story_no.to_id(&prefix);
        let snapshot = fold_story(&id, &known, &states).map_err(|error| error.to_string());
        rebuilt.push(RebuiltStory {
            story_no,
            head_seq,
            head_global_seq,
            snapshot,
            unknown_events,
        });
    }
    Ok(rebuilt)
}

/// Re-folds every story in a project, in its own read transaction.
pub fn rebuild_read_model<S: Store>(
    store: &S,
    project: ProjectId,
) -> Result<Vec<RebuiltStory>, StoreError> {
    store.read(|tx| rebuild(tx, project))
}

/// Compares a project's persisted read model against a rebuild of it.
pub fn diff_read_model<S: Store>(
    store: &S,
    project: ProjectId,
) -> Result<ReadModelDiff, StoreError> {
    store.read(|tx| diff(tx, project))
}

/// [`diff_read_model`], inside a transaction the caller already has.
pub fn diff(tx: &impl ReadOps, project: ProjectId) -> Result<ReadModelDiff, StoreError> {
    diff_rebuilt(tx, project, &rebuild(tx, project)?)
}

/// [`diff`] against a rebuild the caller has already performed.
///
/// The split exists for [`repair_read_model`], which needs both the comparison
/// and the rebuilt stories themselves — and used to obtain them by folding the
/// whole project twice inside one transaction, where the second fold could not
/// possibly differ from the first (SH-267).
pub fn diff_rebuilt(
    tx: &impl ReadOps,
    project: ProjectId,
    rebuilt: &[RebuiltStory],
) -> Result<ReadModelDiff, StoreError> {
    let prefix = tx
        .project(project)?
        .ok_or_else(|| StoreError::NotFound(format!("project {project} does not exist")))?
        .prefix;

    let persisted: BTreeMap<StoryNo, crate::store::types::StoryRow> = tx
        .stories(project, &StoryQuery::all())?
        .into_iter()
        .map(|row| (row.story_no, row))
        .collect();

    let mut diff = ReadModelDiff::default();
    let closure = relation_closure(rebuilt, &prefix);
    let expected_edges = closure.edges;
    diff.asymmetric_relations = closure.asymmetric;
    diff.unvouched_relations = closure.unvouched;
    let mut seen = BTreeSet::new();

    for story in rebuilt {
        seen.insert(story.story_no);
        diff.unknown_events.extend(story.unknown_events.clone());

        let expected = match &story.snapshot {
            Ok(snapshot) => snapshot,
            Err(reason) => {
                diff.fold_failures.push((story.story_no, reason.clone()));
                continue;
            }
        };
        let Some(row) = persisted.get(&story.story_no) else {
            diff.missing_rows.push(story.story_no);
            continue;
        };

        // Where a disagreement is filed, not whether it is computed (SH-410).
        // Every comparison still runs; a `Basis::Fold` one on a story whose
        // fold is incomplete is evidence of nothing, so it is reported as
        // unvouched rather than as damage.
        let vouched = story.is_complete();
        let mut report = |field: &str, persisted: String, rebuilt: String, basis: Basis| {
            if persisted == rebuilt {
                return;
            }
            let divergence = Divergence {
                story_no: story.story_no,
                field: field.to_string(),
                persisted,
                rebuilt,
            };
            if vouched || basis == Basis::Events {
                diff.divergences.push(divergence);
            } else {
                diff.unvouched.push(divergence);
            }
        };

        for (field, persisted, rebuilt, basis) in
            column_comparisons(row, expected, story.head_seq, story.head_global_seq)?
        {
            report(field, persisted, rebuilt, basis);
        }

        let persisted_edges: BTreeSet<String> = tx
            .relations_from(project, story.story_no)?
            .into_iter()
            .map(|edge| format!("{}:{}", edge.relation, edge.other_no))
            .collect();
        report(
            "relations",
            persisted_edges.into_iter().collect::<Vec<_>>().join(" "),
            expected_edges
                .get(&story.story_no)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .collect::<Vec<_>>()
                .join(" "),
            // The expected edge set is the closure of the *folded* snapshots'
            // claims, so an undecoded event that adds a relation is missing
            // from it.
            Basis::Fold,
        );
    }

    diff.extra_rows = persisted
        .keys()
        .filter(|no| !seen.contains(no))
        .copied()
        .collect();
    Ok(diff)
}

/// Every indexed column of a [`StoryRow`], paired with the value the story's
/// events say it should hold.
///
/// `(field, persisted, rebuilt)` triples, rendered as strings and *unfiltered*:
/// deciding which pairs disagree is [`diff_rebuilt`]'s job, and keeping that
/// decision out of here is what makes "every column appears exactly once" a
/// property you can read off the function.
///
/// # The pattern below has no `..`, and must never grow one
///
/// That is the whole of SH-365. `StoryRow`'s columns used to be reached through
/// `row.field` accessors, so a column added to the struct and forgotten here was
/// simply never compared — `story doctor` went quiet about it forever, and the
/// only reason anyone noticed the first time (SH-211, `hidden_at` and `draft`)
/// was a reader happening to look. Destructuring without a rest pattern moves
/// that from vigilance to a compile error: a new field fails `cargo check` here,
/// and because clippy runs under `-D warnings`, binding one and never using it
/// fails the gate too.
///
/// What the compiler cannot do is check that a field is compared against the
/// *right* thing, and `store::sqlite::read`'s `hydrate` is the proof — a
/// rest-free struct literal over this same type, sitting on top of a
/// `raw_story_from_row` that fills it by position. `tests/read_model_column_
/// coverage.rs` is the other half: it damages each column underneath the store
/// and demands the oracle name it.
///
/// The two arguments carrying the rebuilt head are separate rather than a whole
/// `RebuiltStory` because a `RebuiltStory`'s snapshot is a `Result` the caller
/// has already unwrapped — passing it again would mean unwrapping it twice, in
/// two places, with only one of them able to report the failure.
fn column_comparisons(
    row: &StoryRow,
    expected: &StorySnapshot,
    rebuilt_head: EventSeq,
    rebuilt_head_global: GlobalSeq,
) -> Result<Vec<(&'static str, String, String, Basis)>, StoreError> {
    let StoryRow {
        // Not compared, and the only field that is not: this is the key the row
        // was looked up by, so any comparison against it holds by construction.
        // A row filed under the wrong number is caught as a missing row and an
        // extra row, which says more than a divergence could.
        story_no: _,
        head_seq,
        head_global_seq,
        title,
        state,
        superstate,
        priority,
        story_type,
        assignee,
        awaiting,
        deleted,
        archived,
        created_at,
        updated_at,
        closed_at,
        description,
        hidden_at,
        draft,
        labels,
        snapshot,
    } = row;

    Ok(vec![
        ("head_seq", head_seq.to_string(), rebuilt_head.to_string(), Basis::Events),
        // `put_story` derives this column from `events` in the same SQL
        // statement that writes it, so it can only diverge from a rebuild if
        // something wrote the row outside `put_story` — a raw migration, a hand
        // edit. Reported beside `head_seq`, the other coordinate of the same
        // event, for the same reason SH-211 gave `hidden_at`/`draft` their own
        // divergence checks.
        (
            "head_global_seq",
            head_global_seq.to_string(),
            rebuilt_head_global.to_string(),
            Basis::Events,
        ),
        ("title", title.clone(), expected.title.clone(), Basis::Fold),
        ("state", state.clone(), expected.state.clone(), Basis::Fold),
        (
            "superstate",
            superstate.as_str().to_string(),
            expected.superstate.as_str().to_string(),
            Basis::Fold,
        ),
        (
            "priority",
            priority.as_str().to_string(),
            expected.priority.as_str().to_string(),
            Basis::Fold,
        ),
        (
            "story_type",
            optional(story_type.as_deref()),
            optional(expected.story_type.as_deref()),
            Basis::Fold,
        ),
        (
            "assignee",
            optional(assignee.as_deref()),
            optional(expected.assignee.as_deref()),
            Basis::Fold,
        ),
        (
            "awaiting",
            optional(awaiting.as_deref()),
            optional(expected.awaiting.as_deref()),
            Basis::Fold,
        ),
        ("deleted", deleted.to_string(), expected.deleted.to_string(), Basis::Fold),
        // `archived` has no counterpart in the snapshot: it is *defined* as
        // "has a close timestamp", and a schema CHECK holds it there. This
        // compares the flag against that definition.
        (
            "archived",
            archived.to_string(),
            expected.closed_at.is_some().to_string(),
            Basis::Fold,
        ),
        (
            "created_at",
            created_at.clone(),
            expected.created_at.clone(),
            Basis::Fold,
        ),
        (
            "updated_at",
            updated_at.clone(),
            expected.updated_at.clone(),
            Basis::Fold,
        ),
        (
            "closed_at",
            optional(closed_at.as_deref()),
            optional(expected.closed_at.as_deref()),
            Basis::Fold,
        ),
        (
            "description",
            optional(description.as_deref()),
            optional(expected.description.as_deref()),
            Basis::Fold,
        ),
        // Neither column has a schema CHECK tying it to anything else (SH-43,
        // SH-175 respectively) — the fold and the service layer are the only
        // things that ever kept `hidden_at` implying CLOSED or `draft` only
        // ever clearing. A write that reaches the column directly (a raw
        // migration, a hand edit) skips both, and nothing else here would catch
        // it: `snapshot` below only sees a divergence if that same write also
        // touched the embedded copy, which is exactly what a column-only write
        // does not do (SH-211).
        (
            "hidden_at",
            optional(hidden_at.as_deref()),
            optional(expected.hidden_at.as_deref()),
            Basis::Fold,
        ),
        ("draft", draft.to_string(), expected.draft.to_string(), Basis::Fold),
        (
            "labels",
            labels.join(","),
            expected
                .labels
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
                .join(","),
            Basis::Fold,
        ),
        // The snapshot column against the snapshot the events produce: this is
        // what catches a change to a field that has no column of its own, such
        // as a comment.
        (
            SNAPSHOT_FIELD,
            serde_json::to_string(snapshot)?,
            serde_json::to_string(expected)?,
            Basis::Fold,
        ),
    ])
}

/// Rewrites a project's read model from its events, atomically.
///
/// The rebuild and the rewrite happen in one transaction, so a repair cannot
/// itself be interrupted into a half-repaired state. Stories whose events do
/// not fold are left alone and reported: overwriting them with a guess would
/// destroy the evidence.
///
/// # A fold that is *incomplete* is a guess in exactly that sense (SH-410)
///
/// A story carrying an event this build cannot decode folds to `Ok` — the fold
/// succeeded on what it could read — and the result silently omits whatever
/// the undecoded events contributed. Writing that over the persisted row
/// destroys the newer storyhook's own fold, under an exit status that says the
/// row was repaired. Those stories are withheld
/// ([`RepairReport::withheld`]) and no `put_story` runs for them.
///
/// The store never asks *why* an event would not decode: an unknown kind and a
/// torn payload of a known kind make the fold equally partial. Which of the two
/// it was is a policy question, and it stays in `service::integrity`, which is
/// the only layer that calls [`crate::domain::is_known_event_kind`].
pub fn repair_read_model<S: Store>(
    store: &S,
    project: ProjectId,
) -> Result<RepairReport, StoreError> {
    store.write(|tx| {
        // One fold, read twice: once to say what was wrong, once to write the
        // rows that put it right. Nothing between them could change the answer
        // — they were always inside this one transaction — so the second fold
        // was pure cost (SH-267).
        let rebuilt = rebuild(tx, project)?;
        let before = diff_rebuilt(tx, project, &rebuilt)?;
        let mut report = RepairReport {
            repaired: before,
            ..RepairReport::default()
        };
        for story in rebuilt {
            let complete = story.is_complete();
            match story.snapshot {
                // Deliberately not written: see this function's own doc
                // comment. `head_seq` is the trap that makes a "helpful"
                // partial write worse than none — `put_story` would stamp the
                // row at the FULL head, taken from the very event that did not
                // decode, so the row would claim to be a fold up to head and
                // the one column that tells *stale* from *wrong* would be
                // spent saying something false.
                Ok(_) if !complete => {
                    report.withheld.push((story.story_no, story.unknown_events));
                }
                Ok(snapshot) => {
                    tx.put_story(project, &snapshot, story.head_seq)?;
                    report.rewritten.push(story.story_no);
                }
                Err(reason) => report.unrepairable.push((story.story_no, reason)),
            }
        }
        Ok(report)
    })
}

/// Renders an optional field so that "absent" and "empty" are distinguishable
/// in a divergence report.
fn optional(value: Option<&str>) -> String {
    value.map_or_else(|| "<none>".to_string(), str::to_string)
}

/// The relation rows a project's histories imply, and the claims only one end
/// makes.
///
/// The expected contents of `story_relations` are the *closure* of every
/// story's claims, not each story's own claims taken separately: the schema
/// materializes the inverse of whatever is written, so a row exists as soon as
/// either end asserts the edge. Comparing against one end alone would report a
/// divergence on every mirror row, which would make the oracle useless exactly
/// where the schema is doing its job.
///
/// The claims only one end makes are reported separately: symmetric in the
/// table, asymmetric in the history, and only an event can fix that.
type EdgeSets = BTreeMap<StoryNo, BTreeSet<String>>;

/// What [`relation_closure`] answers.
///
/// A struct rather than a tuple because [`asymmetric`](Self::asymmetric) and
/// [`unvouched`](Self::unvouched) have the identical type and opposite
/// meanings — one is damage, the other is explicitly not — so a positional
/// return would let a swap compile.
struct RelationClosure {
    /// The edge set each story's history implies, inverses included.
    edges: EdgeSets,
    /// Edges only one end claims, where the other end's history is one this
    /// build read whole.
    asymmetric: Vec<(StoryNo, String, StoryNo)>,
    /// Edges only one end claims, where the *other* end carries an event this
    /// build could not decode — which may be the very event claiming the
    /// inverse (SH-410).
    unvouched: Vec<(StoryNo, String, StoryNo)>,
}
fn relation_closure(rebuilt: &[RebuiltStory], prefix: &str) -> RelationClosure {
    let mut claims: BTreeSet<(StoryNo, String, StoryNo)> = BTreeSet::new();
    // The ends whose histories this build read only part of. An edge is
    // asymmetric because one end's history does not claim the inverse — and a
    // history with an undecoded event in it may claim exactly that, in the
    // event this build could not read (SH-410).
    let mut incomplete: BTreeSet<StoryNo> = BTreeSet::new();
    for story in rebuilt {
        if !story.is_complete() {
            incomplete.insert(story.story_no);
        }
        let Ok(snapshot) = &story.snapshot else {
            continue;
        };
        for relation in &snapshot.relationships {
            if let Ok(other) = StoryNo::parse_id(prefix, &relation.other_id) {
                claims.insert((story.story_no, relation.relation.clone(), other));
            }
        }
    }

    let mut edges: EdgeSets = BTreeMap::new();
    let mut asymmetric = Vec::new();
    let mut unvouched = Vec::new();
    for (story_no, relation, other) in &claims {
        edges
            .entry(*story_no)
            .or_default()
            .insert(format!("{relation}:{other}"));
        let Some(inverse) = crate::domain::inverse_relation(relation) else {
            continue;
        };
        edges
            .entry(*other)
            .or_default()
            .insert(format!("{inverse}:{story_no}"));
        if !claims.contains(&(*other, inverse.to_string(), *story_no)) {
            // Keyed on the end that is *missing* its half, not on the claimant:
            // a claimant whose own fold is incomplete still made the claim this
            // build can see, and an end that decodes cleanly and does not claim
            // the inverse genuinely does not claim it.
            if incomplete.contains(other) {
                unvouched.push((*story_no, relation.clone(), *other));
            } else {
                asymmetric.push((*story_no, relation.clone(), *other));
            }
        }
    }
    RelationClosure {
        edges,
        asymmetric,
        unvouched,
    }
}
