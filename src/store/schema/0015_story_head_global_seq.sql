-- storyhook store — schema version 15: `stories.head_global_seq`, the exact
-- recency tiebreak SH-336's council verdict adopted.
--
-- # What this column is
--
-- The change-feed position (`events.global_seq`) of the event this row's
-- `head_seq` names — the same event, in the other coordinate. `head_seq`
-- (migration 1) says which event *within the story*; `head_global_seq` says
-- where that same event sits in the *project's* write order. Every storyhook
-- timestamp is RFC3339 at one-second precision (`service::Clock::System`), so
-- two stories written inside one second tie on `updated_at`/`created_at` and
-- every ordering built on those timestamps was blind to which write actually
-- happened last (SH-336, following on from SH-329's flake). `global_seq` is
-- allocated once per event, inside the write transaction, by an
-- `UPDATE projects SET next_global_seq = next_global_seq + ?2 ... RETURNING`
-- (`allocate_global_seqs`) — writes are already serialized behind one
-- process-wide write mutex, so this key structurally cannot tie the way a
-- timestamp can.
--
-- # Why `seq = head_seq`, not `MAX(global_seq)`
--
-- Both read the same value on a healthy row — they only differ on a row whose
-- `head_seq` has fallen behind its story's true head, which is precisely the
-- staleness `head_seq` itself already exists to name (see its own doc
-- comment, `StoryRow::head_seq`). Joining on `seq = head_seq` means a stale
-- row is stale in *both* coordinates and `story doctor`'s `diff_rebuilt`
-- reports `head_seq` and `head_global_seq` together as one fact about one
-- event, rather than `head_global_seq` silently reporting the project's
-- current high-water mark for a row that is not, in fact, at the head.
--
-- # Why a column, and why derived rather than passed in
--
-- `put_story`'s own upsert derives it with a scalar subquery over `events` in
-- both the `VALUES` list and `DO UPDATE SET`, the same way `archived` is
-- already derived from `snapshot.closed_at` there rather than passed as a
-- parameter a caller could get wrong. This is what lets `src/store/rebuild.rs`
-- keep working with no change (it already calls `put_story`), and what lets
-- `refold_story` — which re-derives a row from events already on disk, with
-- no new event appended — write a correct value with no special case. The
-- precondition this places on `put_story`, verified true on every call site:
-- the events it derives from must already be committed in the same
-- transaction, which every caller (`append_and_fold`, `refold_story`,
-- `rebuild`, `transfer::import_project`) already satisfies.
--
-- # `NOT NULL DEFAULT 0`, not nullable
--
-- Mirrors `head_seq`'s own `0` — "before any event" / "no event backs this
-- row" (`CHECK (head_seq >= 0)`, migration 1). A row with `head_global_seq = 0`
-- after this backfill is exactly `ReadOps::rebuild`'s `extra_rows` case: a
-- story row with no corresponding event, which `story doctor` already names.
--
-- # No CHECK
--
-- SQLite's `ALTER TABLE ... ADD COLUMN` cannot add one that references
-- another table (`events`), matching the accommodation migrations 10 and 12
-- already document for their own columns.
--
-- # The index
--
-- `idx_stories_updated` is rebuilt to carry both keys in the same direction,
-- so `ORDER BY updated_at DESC, head_global_seq DESC` is a reverse index scan
-- rather than a sort. `StorySort::UpdatedAt` has no production caller today
-- (only the store conformance suite exercises it) — the index is widened
-- anyway, because an index that names a sort without covering it is the trap
-- the next author trips on, not this one.
--
-- # No rebuild, no event appended
--
-- One `ALTER TABLE ... ADD COLUMN`, one `UPDATE` that reads `events` and
-- writes one `stories` column, and an index swap. No table is rebuilt, so
-- there is no `DROP TABLE` for a trigger to re-parse and migration 5's
-- `events_reject_delete` warning does not apply; `events` is only read here,
-- and its append-only triggers fire on UPDATE/DELETE, neither of which this
-- migration performs against that table. `projects.next_global_seq` is
-- untouched — no new event is written.

ALTER TABLE stories ADD COLUMN head_global_seq INTEGER NOT NULL DEFAULT 0;

UPDATE stories
SET head_global_seq = COALESCE(
        (SELECT e.global_seq
           FROM events e
          WHERE e.project_id = stories.project_id
            AND e.story_no   = stories.story_no
            AND e.seq        = stories.head_seq),
        0
    );

DROP INDEX idx_stories_updated;
CREATE INDEX idx_stories_updated ON stories(project_id, updated_at, head_global_seq);
