-- storyhook store — schema version 21: `closed` becomes a required state, and
-- every soft-deleted story comes to rest in it, archived (SH-505).
--
-- Design of record: `docs/spec/deletion-and-closure.md`.
--
-- # What this is not
--
-- It is NOT repairing an illegal `(state, superstate)` pair. That was migration
-- 4's job, and it succeeded: `stories.superstate` is a pure function of the slug
-- and the catalog, held there by the composite foreign key, so a soft-deleted
-- story is already sitting in the project's resting CLOSED state — `done` for
-- every conforming project — with `superstate = 'CLOSED'`, `archived = 1` and
-- `closed_at` set. This moves a legal row from `done` to `closed` and stamps
-- `hidden_at`. Nothing else about the row changes.
--
-- So `superstate`, `archived`, `closed_at`, `head_seq`, `head_global_seq` and
-- `updated_at` are all left alone, and **no event is appended** — which is why
-- `projects.next_global_seq` is untouched too, unlike migrations 19 and 20.
-- That is the single biggest reason this migration is cheap, and it is written
-- here so the next author does not reach for migration 4's or 19's rebuild
-- reflexively.
--
-- The `deleted` column survives this migration on purpose. It still describes
-- something real until `story delete` becomes permanent (SH-498); dropping it
-- here would leave `story delete`, `story purge` and the undelete path with no
-- flag to read and nothing to do.
--
-- # Order is required, not merely tidy
--
-- The foreign key `(project_id, state, superstate) -> project_states(project_id,
-- slug, superstate)` is DEFERRABLE INITIALLY DEFERRED, so repointing the stories
-- before the state exists survives all the way to COMMIT and *then* fails with
-- `FOREIGN KEY constraint failed`, naming neither table nor row and rolling the
-- whole migration back. Insert the state first.
--
-- # A project that already defines a state named `closed`
--
-- `story state add closed --super OPEN` is legal today, and
-- `domain::with_required_states` refuses to reclassify an existing state under a
-- new superstate — "storyhook will add a missing state but will not reclassify
-- the stories in an existing one". This must not flip it either, so the INSERT
-- skips any project that already owns the slug, and the UPDATE is guarded on the
-- slug existing *and being CLOSED*.
--
-- Those projects keep their soft-deleted stories in `done`, and
-- `domain::resting_state_for_closure` agrees with them: its second rung answers
-- `done` for exactly the catalogs whose `closed` is unusable. So a fresh fold and
-- the stored row still match, `story doctor` reports no divergence, and the real
-- problem — a catalog below the required-states floor — is reported as the
-- `RequiredStates` finding it is, which `--fix` correctly declines to repair by
-- reclassification.
--
-- # `hidden_at` mirrors the fold exactly, and cannot be read off `closed_at`
--
-- `closed_at` is the EARLIER of the two whenever a story was closed before being
-- deleted — a case `fold_story_deleted_while_closed_keeps_original_closed_at`
-- already pins — so using it would manufacture a divergence on precisely those
-- rows.
--
-- The fold stamps `hidden_at` inside its `StoryDeleted` arm, where a later
-- `StoryHidden` overwrites it and a later `StoryUnhidden` clears it by ordinary
-- replay order. The predicate below is that rule stated in SQL: the last of the
-- three hidden-affecting kinds wins, and `StoryUnhidden` winning means NULL.
-- `events.at` is a real column, so no `json_extract` is needed.
--
-- The post-loop `superstate = OPEN` retraction cannot apply here: `deleted = 1`
-- forces CLOSED.
--
-- # `seq <= head_seq`
--
-- Migration 15's bound, for migration 15's reason. `stories.head_seq` names the
-- event this row was folded from; reading past it would make one column fresher
-- than the row around it, so a stale row would become stale in one coordinate
-- and current in another — harder to diagnose than one that is simply, and
-- consistently, behind.
--
-- # The snapshot patch: which half the oracle depends on
--
-- `$.state` and `$.hidden_at` are **correctness**. `store::sqlite::read::hydrate`
-- deserializes the blob and never re-folds, so an unrepaired document would show
-- the wrong state in `story show` and make `story doctor`'s `diff_rebuilt`
-- report every such story as divergent.
--
-- `hidden_at` is `skip_serializing_if = "Option::is_none"`, so the NULL case
-- removes the key rather than writing JSON null — matching what `put_story`
-- writes next. Key ORDER is irrelevant: `diff_rebuilt` compares the deserialized
-- struct, not the raw text.

-- 1. The state itself, at the end of each project's ordering, with no role and
--    no description so that a migrated project and a `doctor --fix`ed one
--    cannot disagree (`domain::with_required_states` writes exactly this).
INSERT INTO project_states (project_id, position, slug, superstate, role, description)
SELECT
    p.id,
    (SELECT COALESCE(MAX(s.position), -1) + 1
       FROM project_states s
      WHERE s.project_id = p.id),
    'closed',
    'CLOSED',
    NULL,
    NULL
FROM projects p
WHERE NOT EXISTS (
    SELECT 1 FROM project_states s
     WHERE s.project_id = p.id AND s.slug = 'closed'
);

-- 2. Repoint every soft-deleted story, keyed on the `deleted` COLUMN.
--
--    Never on `EXISTS (… kind = 'StoryDeleted')`: migration 16's lesson on a
--    different fact. `[StoryDeleted, StoryStateChanged(todo)]` is a live,
--    reachable log — `story delete` then `story reopen --force` — whose story is
--    NOT deleted, and an EXISTS predicate would archive it. The column is
--    already the head-bounded, fold-authoritative answer.
CREATE TEMP TABLE sh505_rows AS
SELECT
    st.project_id AS project_id,
    st.story_no   AS story_no,
    (
        SELECT CASE WHEN e.kind = 'StoryUnhidden' THEN NULL ELSE e.at END
          FROM events e
         WHERE e.project_id = st.project_id
           AND e.story_no   = st.story_no
           AND e.seq       <= st.head_seq
           AND e.kind IN ('StoryDeleted', 'StoryHidden', 'StoryUnhidden')
         ORDER BY e.seq DESC
         LIMIT 1
    ) AS hidden_at
FROM stories st
WHERE st.deleted = 1
  AND EXISTS (
      SELECT 1 FROM project_states ps
       WHERE ps.project_id = st.project_id
         AND ps.slug = 'closed'
         AND ps.superstate = 'CLOSED'
  );

UPDATE stories
   SET state = 'closed',
       hidden_at = (SELECT r.hidden_at FROM sh505_rows r
                     WHERE r.project_id = stories.project_id
                       AND r.story_no = stories.story_no),
       snapshot = (
           SELECT CASE
               WHEN r.hidden_at IS NULL
                   THEN json_remove(json_set(stories.snapshot, '$.state', 'closed'), '$.hidden_at')
               ELSE json_set(json_set(stories.snapshot, '$.state', 'closed'), '$.hidden_at', r.hidden_at)
           END
             FROM sh505_rows r
            WHERE r.project_id = stories.project_id
              AND r.story_no = stories.story_no
       )
 WHERE EXISTS (
       SELECT 1 FROM sh505_rows r
        WHERE r.project_id = stories.project_id
          AND r.story_no = stories.story_no
   );

DROP TABLE sh505_rows;
