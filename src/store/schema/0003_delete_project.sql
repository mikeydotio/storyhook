-- storyhook store — schema version 3: a project can be deleted.
--
-- Schema 1 made this impossible on purpose. It withheld `ON DELETE CASCADE`
-- from `events.project_id` and guarded `events` with an unconditional
-- `events_reject_delete`, and said so in as many words:
--
--     A future `delete_project` operation has to drop these guards inside its
--     own migration and say so — it cannot happen by accident.
--
-- This is that migration, and this is it saying so. `story project deinit`
-- removes a project and everything recorded against it, which means removing
-- its event log — and an unconditional trigger cannot tell that teardown from
-- the history rewrite it exists to prevent.
--
-- The guard is narrowed rather than dropped: an event may be deleted only once
-- its project no longer exists. That is not a flag somebody sets, and it is not
-- a permission decoupled from the act — to satisfy it you must already have
-- deleted the project row, which is `delete_project`'s own second-to-last
-- statement and nothing else's. Every other DELETE against `events` still
-- aborts, because the project it belongs to is still there.
--
-- Three alternatives were measured against the bundled SQLite (3.46) and
-- rejected:
--
--   * **`ON DELETE CASCADE` on `events.project_id`.** A cascade *does* fire the
--     child table's BEFORE DELETE trigger — verified, not assumed — so the
--     cascade would trip the very guard it was meant to route around. Adding
--     the clause would also mean a full 12-step rebuild of the append-only
--     table, since SQLite has no `ALTER TABLE ADD CONSTRAINT`.
--   * **A gate row the trigger consults.** A persistent one leaves the guard
--     silently lowered if a transaction dies between opening and closing it,
--     and a `temp` one is impossible: SQLite refuses to compile a `main`-schema
--     trigger that references `temp`.
--   * **Dropping `events_reject_delete` outright.** It is the only thing
--     standing between a bug and a rewritten history.
--
-- `events_reject_update` is deliberately untouched. An event is never
-- rewritten, under any circumstance, including this one.

DROP TRIGGER events_reject_delete;

-- `WHEN EXISTS (...)` is evaluated per row, before the delete. During a project
-- teardown the parent row is already gone, so the predicate is false and the
-- event goes; at every other time it is true and the delete aborts.
--
-- The message is unchanged from schema 1: the overwhelmingly common way to
-- reach this trigger is still an ordinary attempt to erase history, and that
-- caller should read the same sentence it always did.
CREATE TRIGGER events_reject_delete
BEFORE DELETE ON events
WHEN EXISTS (SELECT 1 FROM projects WHERE id = OLD.project_id)
BEGIN
    SELECT RAISE(ABORT, 'events are append-only: DELETE is not permitted');
END;
