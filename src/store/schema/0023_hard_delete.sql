-- storyhook store — schema version 23: deletion becomes permanent (SH-498).
--
-- `stories.deleted` described the old soft-delete tombstone. Current
-- `story delete` removes the story row, its events, and every story-scoped
-- projection, so no surviving row can truthfully carry that flag.
--
-- Historical `StoryDeleted` events remain readable. Migration 21 already
-- moved their surviving stories into the `closed` state and gave them the
-- correct `hidden_at`; this migration changes no state or event. It only
-- removes the obsolete materialized flag and its JSON twins.
--
-- Patch the JSON before dropping the column. Deserialization would ignore the
-- old keys after the Rust fields disappear, but leaving them would make export
-- and direct store inspection claim a concept the current schema no longer
-- has.
--
-- `ALTER TABLE ... DROP COLUMN` is supported by the bundled SQLite and does
-- not rebuild `stories`, so foreign keys stay enabled and migration 5's
-- `events_reject_delete` trigger needs no drop/recreate bracket.

UPDATE stories
   SET snapshot = json_remove(snapshot, '$.deleted', '$.deleted_reason');

ALTER TABLE stories DROP COLUMN deleted;
