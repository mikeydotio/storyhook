-- storyhook store — schema version 5: one story can be purged.
--
-- `story delete` is a soft delete: it appends a `StoryDeleted` event, the story
-- comes to rest in a CLOSED state, and every byte of it is still there. That is
-- the right default and it is not changing. What did not exist was any way to
-- remove a story *at all* — SH-20 in this project's own tracker was created in
-- error, soft-deleted, and could not be got rid of without a hand-written
-- sqlite transaction against a production store, which SH-92 argued no user
-- should ever be asked to run.
--
-- `story purge <ID>` is that operation, and this migration is the schema half
-- of it. Soft delete keeps its meaning and gains a second one: it is the
-- reversible tombstone *and* the required antechamber to the irreversible act.
-- A story that has not been soft-deleted cannot be purged.
--
-- ---------------------------------------------------------------------------
-- What this narrows, and how narrowly
-- ---------------------------------------------------------------------------
--
-- Schema 1 made `events` append-only with an unconditional trigger and said
-- that a future operation needing to delete events "has to drop these guards
-- inside its own migration and say so — it cannot happen by accident". Schema 3
-- was the first such migration, for `story project delete`, and narrowed the
-- guard to abstain only for an event whose *project* row is already gone.
--
-- This is the second, and it **composes with** schema 3 rather than replacing
-- it. The predicate gains a conjunct; it does not lose one:
--
--     WHEN <the project still exists>        -- schema 3
--      AND <the story still exists>          -- this migration
--
--   * a project teardown fails the first conjunct — `delete_project` removes
--     the `projects` row before it touches `events`;
--   * a purge fails the second — `purge_story` removes the `stories` row
--     before it touches `events`;
--   * every other DELETE against `events` satisfies both and still aborts.
--
-- Neither operation can satisfy the other's escape. A purge leaves the project
-- row alone, so it can only ever reach the events of the one story it deleted
-- the row for; a teardown never consults `stories` at all.
--
-- `events_reject_update` is untouched, as it was in schema 3. An event is never
-- rewritten, under any circumstance, including this one.
--
-- ---------------------------------------------------------------------------
-- The residual, stated rather than glossed
-- ---------------------------------------------------------------------------
--
-- The new conjunct is false for any event whose story has no read-model row —
-- and that shape exists outside a purge, as corruption: `rebuild.rs` reports it
-- as `missing_rows` and `story doctor` prints it. For those events the guard is
-- now lower than it was.
--
-- It is accepted, for three reasons and not merely because it is small. The
-- guard only ever stopped a DELETE, and nothing in `src/` issues a raw DELETE
-- against `events` (the one caller is `store::test_support::forget_events`,
-- behind the `fault-injection` feature, which drops the trigger outright and
-- would be unaffected by any predicate). The rows it stops protecting are rows
-- whose read model has *already* been lost, so the history they hold is
-- already not being served to anyone. And the alternative — a gate row, a
-- session flag, a pragma — is the shape schema 3 measured and rejected: a
-- permission decoupled from the act stays lowered when a transaction dies
-- halfway through. To satisfy this predicate you must already have deleted the
-- story row, which is `purge_story`'s own second-to-last statement and nothing
-- else's.
--
-- ---------------------------------------------------------------------------
-- READ THIS BEFORE REBUILDING `stories` AGAIN
-- ---------------------------------------------------------------------------
--
-- This trigger names `stories`, so from here on **a migration that rebuilds
-- `stories` must drop this trigger first and recreate it afterwards.**
--
-- `ALTER TABLE … RENAME TO` re-parses every trigger and view in the schema in
-- order to rewrite references to the table being renamed. Between step 6
-- (`DROP TABLE stories`) and step 7 (`ALTER TABLE stories_new RENAME TO
-- stories`) of SQLite's twelve-step rebuild there is no `stories` for this
-- trigger's `WHEN` clause to resolve, and the rename fails with
--
--     error in trigger events_reject_delete: no such table: main.stories
--
-- That is steps 3 and 8 of the same procedure — "remember the format of all
-- triggers associated with table X … reconstruct them" — reaching a trigger
-- that merely *mentions* `stories` rather than sitting on it. Migration 4 did
-- not have to do this because this trigger did not name `stories` yet.
--
-- The cost is accepted rather than overlooked: the failure is immediate, names
-- both the trigger and the table, and rolls the migration back untouched.
-- `tests/store_migrations.rs` measures it in both directions — one test proves
-- a rebuild that forgets fails, and `REBUILD_STORIES` shows the two lines that
-- fix it.

DROP TRIGGER events_reject_delete;

-- The message is unchanged from schema 1 and schema 3. The overwhelmingly
-- common way to reach this trigger is still an ordinary attempt to erase
-- history, and that caller should read the same sentence it always did.
CREATE TRIGGER events_reject_delete
BEFORE DELETE ON events
WHEN EXISTS (SELECT 1 FROM projects WHERE id = OLD.project_id)
 AND EXISTS (SELECT 1 FROM stories
             WHERE project_id = OLD.project_id AND story_no = OLD.story_no)
BEGIN
    SELECT RAISE(ABORT, 'events are append-only: DELETE is not permitted');
END;
