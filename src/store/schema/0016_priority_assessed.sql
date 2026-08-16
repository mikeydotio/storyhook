-- storyhook store — schema version 16: backfill `snapshot.priority_assessed`,
-- SH-359's "did anyone actually choose this priority" fact.
--
-- # No `ALTER TABLE` — the first migration here with no DDL at all
--
-- `priority_assessed` gets **no column**. `0001_initial.sql`'s own rule is that
-- "a column exists here if and only if something filters, sorts, or joins on
-- it", and nothing does: `story list` loads every row through
-- `story_views` and applies all ten of its filters in Rust with
-- `views.retain(...)` (`service/query.rs`), the TUI filters in Rust, the web
-- dashboard filters in JavaScript, and `story-triage` filters in jq over
-- `--json`. `StoryQuery::limit` — the one thing that would make a Rust-side
-- retain wrong — has no production caller at all.
--
-- So this migration exists solely to repair the embedded documents, which is
-- migration 4's job description too:
--
--     `snapshot` is patched in the same statement. It holds the folded
--     `StorySnapshot` verbatim and is what `story show` reads; a repair that
--     moved the column and left the document behind would fix the query
--     surface and leave the displayed story wrong.
--
-- Here there is no column to move, so the document is the whole of the repair.
-- It is load-bearing rather than cosmetic: `store::sqlite::read` hydrates a
-- `StoryRow` with a plain `serde_json::from_str(&raw.snapshot)` and never
-- re-folds, so an unrepaired row reads `priority_assessed: false` — a story
-- that somebody *did* prioritise, reporting that nobody ever did — until its
-- next write happens to rewrite the blob. It would also make `story doctor`'s
-- `diff_rebuilt` report every such story as divergent, since a fresh fold
-- answers `true` while the stored document says nothing at all.
--
-- # Why the key is written only when true
--
-- `StorySnapshot::priority_assessed` is
-- `#[serde(default, skip_serializing_if = "is_false")]`, so `false` is *absent*
-- from a freshly serialized document rather than present-and-false. A
-- never-assessed row therefore needs no repair: the missing key already
-- deserializes to `false` and already matches a fresh fold byte for byte.
-- Writing `json('false')` into those rows would move bytes for no change in
-- meaning and make every one of them differ from what `put_story` writes next.
--
-- # Why last-event-wins, and not `EXISTS`
--
-- `EXISTS (… kind = 'StoryPrioritySet')` is the obvious predicate and it is
-- wrong, because it can never *un*-assert. A story whose priority was set and
-- then cleared by `StoryPriorityCleared` (SH-359's undo repair) is
-- unassessed, and `EXISTS` would mark it assessed forever.
--
-- The objection that no v15 store can contain a `StoryPriorityCleared` — the
-- event ships *with* this migration, and `migrate` runs before anything can
-- write one — is true for the local path and false for the one that matters.
-- `story import` carries events it cannot decode verbatim:
-- `ExportedEvent::Unknown(RawEvent)` keeps the `kind` string as written,
-- `to_raw()` hands it straight to the append, and the append inserts
-- `event.kind` unmodified. So an **older** binary restoring an export taken
-- from a v16+ store writes `kind = 'StoryPriorityCleared'` into a store still
-- at 15 — which a v16 binary then migrates, right here. That is the documented
-- backup-and-rollback path this project treats as sanctioned, working as
-- designed.
--
-- Ordering by `seq DESC` also mirrors what the fold itself does — each
-- priority event overwrites the last — rather than restating a weaker rule
-- beside it.
--
-- # Why `seq <= head_seq`
--
-- Migration 15's bound, for migration 15's reason. `stories.head_seq` names
-- the event this row was folded from; a row whose `head_seq` has fallen behind
-- its story's true head is *stale*, and that staleness is a fault
-- `story doctor` already reports. Reading events past `head_seq` would make
-- this one column fresher than the row around it, so a stale row would become
-- stale in one coordinate and current in another — harder to diagnose than a
-- row that is simply, consistently behind.
--
-- # No rebuild, and no event appended
--
-- One `UPDATE` against `stories`, reading `events`. No table is rebuilt, so
-- there is no `DROP TABLE` for a trigger to re-parse and `foreign_keys_off`
-- stays `false`. `events` is only ever read: its append-only guards
-- (`events_reject_update`, `events_reject_delete`) fire on UPDATE and DELETE
-- of that table, neither of which happens here, and
-- `projects.next_global_seq` is untouched because no event is written.

UPDATE stories
SET snapshot = json_set(snapshot, '$.priority_assessed', json('true'))
WHERE (
    SELECT e.kind
      FROM events e
     WHERE e.project_id = stories.project_id
       AND e.story_no   = stories.story_no
       AND e.seq       <= stories.head_seq
       AND e.kind IN ('StoryPrioritySet', 'StoryPriorityCleared')
     ORDER BY e.seq DESC
     LIMIT 1
) = 'StoryPrioritySet';
