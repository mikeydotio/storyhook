-- storyhook store — schema version 22: a CLOSED blocker owns no live task
-- dependency (SH-500).
--
-- Before this migration, readiness re-tested a `blocked-by` target's
-- superstate and ignored the edge while that target was CLOSED. The event
-- histories and `story_relations` table kept both halves, however, so reopening
-- the blocker silently activated the old dependency again. New closure writes
-- append compensating removals through the service layer. This migration gives
-- every existing store the same invariant.
--
-- The event log remains authoritative. No relationship-add event is edited or
-- deleted: one `StoryRelationshipRemoved` is appended to each history that
-- currently asserts a half. The materialized snapshots and relation index are
-- then advanced to exactly the state a fresh fold reaches. Half-edges inherited
-- from before SH-60 are intentional input: the relation table still identifies
-- the pair, and only the history that actually asserts a half gets a removal.

CREATE TEMP TABLE sh500_now AS
SELECT strftime('%Y-%m-%dT%H:%M:%SZ', 'now') AS ts;

-- One canonical row per dependency whose blocker has entered the persisted
-- lifecycle CLOSED state. `archived`, not a display-time computed superstate,
-- is the service layer's own closed-story guard.
CREATE TEMP TABLE sh500_edges AS
SELECT r.project_id, r.story_no AS blocker_no, r.other_no AS dependent_no
  FROM story_relations r
  JOIN stories blocker
    ON blocker.project_id = r.project_id
   AND blocker.story_no = r.story_no
 WHERE r.relation = 'blocks'
   AND blocker.archived = 1;

-- Build only the compensations each history can honestly claim. Sequence
-- allocation is deterministic in both domains and starts after the real event
-- head / project counter, matching migrations 19 and 20.
CREATE TEMP TABLE sh500_repairs AS
WITH claims AS (
    SELECT
        edge.project_id,
        edge.blocker_no AS story_no,
        edge.dependent_no AS other_no,
        'blocks' AS relation
      FROM sh500_edges edge
      JOIN stories story
        ON story.project_id = edge.project_id
       AND story.story_no = edge.blocker_no
     WHERE EXISTS (
           SELECT 1
             FROM json_each(story.snapshot, '$.relationships') item
            WHERE json_extract(item.value, '$.relation') = 'blocks'
              AND json_extract(item.value, '$.other_id') =
                  (SELECT prefix FROM projects WHERE id = edge.project_id)
                  || '-' || edge.dependent_no
       )

    UNION ALL

    SELECT
        edge.project_id,
        edge.dependent_no AS story_no,
        edge.blocker_no AS other_no,
        'blocked-by' AS relation
      FROM sh500_edges edge
      JOIN stories story
        ON story.project_id = edge.project_id
       AND story.story_no = edge.dependent_no
     WHERE EXISTS (
           SELECT 1
             FROM json_each(story.snapshot, '$.relationships') item
            WHERE json_extract(item.value, '$.relation') = 'blocked-by'
              AND json_extract(item.value, '$.other_id') =
                  (SELECT prefix FROM projects WHERE id = edge.project_id)
                  || '-' || edge.blocker_no
       )
), ranked AS (
    SELECT
        claim.*,
        COALESCE((
            SELECT MAX(event.seq)
              FROM events event
             WHERE event.project_id = claim.project_id
               AND event.story_no = claim.story_no
        ), 0) + ROW_NUMBER() OVER (
            PARTITION BY claim.project_id, claim.story_no
            ORDER BY claim.relation, claim.other_no
        ) AS seq,
        project.next_global_seq - 1 + ROW_NUMBER() OVER (
            PARTITION BY claim.project_id
            ORDER BY claim.story_no, claim.relation, claim.other_no
        ) AS global_seq
      FROM claims claim
      JOIN projects project ON project.id = claim.project_id
)
SELECT * FROM ranked;

INSERT INTO events (project_id, story_no, seq, global_seq, kind, at, payload)
SELECT
    repair.project_id,
    repair.story_no,
    repair.seq,
    repair.global_seq,
    'StoryRelationshipRemoved',
    (SELECT ts FROM sh500_now),
    json_object(
        'kind', 'StoryRelationshipRemoved',
        'at', (SELECT ts FROM sh500_now),
        'other_id', project.prefix || '-' || repair.other_no,
        'relation', repair.relation
    )
  FROM sh500_repairs repair
  JOIN projects project ON project.id = repair.project_id
 ORDER BY repair.project_id, repair.global_seq;

UPDATE projects
   SET next_global_seq = next_global_seq + (
       SELECT COUNT(*)
         FROM sh500_repairs repair
        WHERE repair.project_id = projects.id
   )
 WHERE EXISTS (
       SELECT 1 FROM sh500_repairs repair
        WHERE repair.project_id = projects.id
   );

-- Patch the two fields the compensating events change, then advance both head
-- coordinates to the final repair for this story. `json_group_array` returns a
-- JSON value, and the outer `json()` keeps json_set from quoting the array.
UPDATE stories
   SET head_seq = (
           SELECT MAX(repair.seq)
             FROM sh500_repairs repair
            WHERE repair.project_id = stories.project_id
              AND repair.story_no = stories.story_no
       ),
       head_global_seq = (
           SELECT MAX(repair.global_seq)
             FROM sh500_repairs repair
            WHERE repair.project_id = stories.project_id
              AND repair.story_no = stories.story_no
       ),
       updated_at = (SELECT ts FROM sh500_now),
       snapshot = json_set(
           snapshot,
           '$.relationships',
           json((
               SELECT COALESCE(json_group_array(json(item.value)), '[]')
                 FROM json_each(stories.snapshot, '$.relationships') item
                WHERE NOT EXISTS (
                      SELECT 1
                        FROM sh500_repairs repair
                        JOIN projects project ON project.id = repair.project_id
                       WHERE repair.project_id = stories.project_id
                         AND repair.story_no = stories.story_no
                         AND repair.relation = json_extract(item.value, '$.relation')
                         AND project.prefix || '-' || repair.other_no =
                             json_extract(item.value, '$.other_id')
                  )
           )),
           '$.updated_at', (SELECT ts FROM sh500_now)
       )
 WHERE EXISTS (
       SELECT 1
         FROM sh500_repairs repair
        WHERE repair.project_id = stories.project_id
          AND repair.story_no = stories.story_no
   );

-- Deleting one canonical `blocks` row fires the mirror-delete trigger and
-- removes `blocked-by`. Snapshots no longer assert either half, so the index
-- now says exactly what a fresh put_story would say.
DELETE FROM story_relations
 WHERE relation = 'blocks'
   AND EXISTS (
       SELECT 1
         FROM sh500_edges edge
        WHERE edge.project_id = story_relations.project_id
          AND edge.blocker_no = story_relations.story_no
          AND edge.dependent_no = story_relations.other_no
   );

DROP TABLE sh500_repairs;
DROP TABLE sh500_edges;
DROP TABLE sh500_now;
