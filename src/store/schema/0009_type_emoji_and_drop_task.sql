-- storyhook store — schema version 9: a type carries an emoji, `story` is
-- renamed to `normal`, and `task` is retired (SH-157).
--
-- ---------------------------------------------------------------------------
-- The new column
-- ---------------------------------------------------------------------------
--
-- `project_types.emoji` is the glyph the web dashboard renders beside a story
-- of that type. The four defaults that survive this migration are backfilled
-- (`WHERE emoji IS NULL`, so a project that already redescribed one of them
-- keeps whatever it holds — this only fills a gap, never overwrites a
-- choice). A project's custom types are left with `emoji IS NULL`; the
-- dashboard's fallback for that case is a generic tag glyph, not blank.
--
-- No rebuild: `ALTER TABLE ... ADD COLUMN` is the whole schema change, so this
-- migration runs with `foreign_keys_off: false` and migration 5's
-- `events_reject_delete` warning to rebuild migrations does not apply.
--
-- ---------------------------------------------------------------------------
-- Renaming `story` to `normal`
-- ---------------------------------------------------------------------------
--
-- `story` was the original name for "an ordinary, unclassified story" — but
-- it is also this whole tool's own name, so a badge or a picker rendering it
-- reads as "a story of type story" everywhere it appears. `default`, the
-- obvious alternative, was rejected for a sharper reason than awkwardness: it
-- collides with the literal word `add_type` already refuses as a reserved
-- slug (`none` and `default` are how the CLI *unsets* a story's type), and
-- with the fallback text an untyped story's `type:` line already renders
-- (`output.rs`'s `"Default"`) — a type actually named `default` would be
-- visually indistinguishable from having no type at all, exactly the
-- ambiguity SH-157 set out to remove by making the web picker say `none`
-- instead. `normal` has neither problem.
--
-- ---------------------------------------------------------------------------
-- Retiring `task`
-- ---------------------------------------------------------------------------
--
-- "Retired" means removed from the defaults and from every existing
-- project's catalog — not reserved. `story type add task` still works after
-- this runs; nothing here makes the slug unrepresentable.
--
-- What is NOT done, for either rename: a bare `UPDATE stories SET
-- story_type = 'normal' WHERE story_type IN ('story', 'task')`. The event log
-- is this store's source of truth, and `story doctor` / `store::rebuild::diff`
-- compare every row against the fold of its own events — a row edited
-- without a matching event is permanent, self-inflicted drift,
-- indistinguishable from corruption the next time either of those runs.
-- Migration 4 solved its equivalent problem by changing the *fold* instead;
-- that tool does not fit here, because a fold change would make the slugs
-- `story` and `task` unrepresentable forever — the opposite of "removed, not
-- reserved" for `task`, and simply wrong for `story`, which is not being
-- retired, only renamed.
--
-- So every story presently typed `story` or `task` gets one real, appended
-- `StoryTypeSet { story_type: "normal" }` event (legal: the append-only
-- triggers on `events` block UPDATE and DELETE, not INSERT), landing on the
-- final slug directly rather than hopping through an intermediate rename —
-- and the row is then brought forward to agree with it, the same shape
-- `put_story` uses after any other event append, just written as SQL because
-- a migration has no `fold_story` to call.
--
-- A single timestamp is computed once, in a temp table, and reused by every
-- statement below. `strftime('now')` calls SQLite's clock fresh each time
-- it's invoked; separate calls across separate statements could theoretically
-- straddle a second boundary and disagree, which would make `events.at` and
-- `stories.updated_at` diverge for no reason a reader could see. Fixing it
-- once removes that possibility rather than hoping the migration runs fast
-- enough to avoid it. The temp table lives in SQLite's per-connection temp
-- database, not in `sqlite_master`, so it is invisible to
-- `tests/store_schema_fixture.rs`'s schema comparison and needs no cleanup
-- beyond the `DROP` below (it would vanish with the connection regardless).

ALTER TABLE project_types ADD COLUMN emoji TEXT;

-- The catalog rename. Ahead of the emoji backfill below, so that backfill can
-- key on the final slug rather than needing to know the old one too.
UPDATE project_types SET slug = 'normal' WHERE slug = 'story';

UPDATE project_types
   SET emoji = CASE slug
       WHEN 'normal' THEN '📙'
       WHEN 'epic'   THEN '📚'
       WHEN 'bug'    THEN '🐞'
       WHEN 'chore'  THEN '🧺'
   END
 WHERE emoji IS NULL
   AND slug IN ('normal', 'epic', 'bug', 'chore');

CREATE TEMP TABLE sh157_now AS
SELECT strftime('%Y-%m-%dT%H:%M:%SZ', 'now') AS ts;

-- One `StoryTypeSet` event per story presently typed `story` or `task`,
-- appended after that story's current head. `global_seq` is allocated from
-- the project's own counter, offset by this story's rank among its project's
-- affected stories (`ROW_NUMBER() ... - 1`), so several stories in the same
-- project land on several consecutive, non-colliding values — exactly what
-- `append_events` would hand out one call at a time.
INSERT INTO events (project_id, story_no, seq, global_seq, kind, at, payload)
SELECT
    s.project_id,
    s.story_no,
    (
        SELECT COALESCE(MAX(e.seq), 0) + 1
          FROM events e
         WHERE e.project_id = s.project_id AND e.story_no = s.story_no
    ),
    p.next_global_seq - 1 + ROW_NUMBER() OVER (
        PARTITION BY s.project_id ORDER BY s.story_no
    ),
    'StoryTypeSet',
    (SELECT ts FROM sh157_now),
    json_object(
        'kind', 'StoryTypeSet',
        'at', (SELECT ts FROM sh157_now),
        'story_type', 'normal'
    )
FROM stories s
JOIN projects p ON p.id = s.project_id
WHERE s.story_type IN ('story', 'task');

-- Advances each affected project's counter past every `global_seq` the
-- INSERT above just allocated from it.
UPDATE projects
   SET next_global_seq = next_global_seq + (
       SELECT COUNT(*) FROM stories s
        WHERE s.project_id = projects.id AND s.story_type IN ('story', 'task')
   )
 WHERE EXISTS (
       SELECT 1 FROM stories s
        WHERE s.project_id = projects.id AND s.story_type IN ('story', 'task')
   );

-- Brings the read model forward to agree with the event just appended for
-- it: the column, `head_seq`, `updated_at` (the fold sets it from the
-- event's `at`, so leaving it behind would itself be drift), and the
-- snapshot document `story show`/`story export` read.
UPDATE stories
   SET story_type = 'normal',
       head_seq = (
           SELECT MAX(e.seq) FROM events e
            WHERE e.project_id = stories.project_id AND e.story_no = stories.story_no
       ),
       updated_at = (SELECT ts FROM sh157_now),
       snapshot = json_set(
           json_set(snapshot, '$.story_type', 'normal'),
           '$.updated_at', (SELECT ts FROM sh157_now)
       )
 WHERE story_type IN ('story', 'task');

DROP TABLE sh157_now;

-- The catalog row itself, and the gap its removal leaves in `position`.
-- `put_types` rewrites every position on the next ordinary edit regardless,
-- but nothing about this migration should depend on one happening.
DELETE FROM project_types WHERE slug = 'task';

UPDATE project_types
   SET position = renumbered.new_position
  FROM (
       SELECT project_id, slug,
              ROW_NUMBER() OVER (PARTITION BY project_id ORDER BY position) - 1
                  AS new_position
         FROM project_types
      ) AS renumbered
 WHERE project_types.project_id = renumbered.project_id
   AND project_types.slug = renumbered.slug;
