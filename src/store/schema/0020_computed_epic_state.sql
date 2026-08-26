-- storyhook store — schema version 20: structural epics have computed state
-- and a story may belong to several parent epics (SH-446).
--
-- The event log remains the authority. Every story that already has a
-- `parent-of` edge receives one real StoryStateCleared event; its snapshot is
-- marked `state_computed`, while the existing non-null state/superstate
-- columns remain dormant compatibility fallbacks. Supported reads recursively
-- overlay their effective values. New first-child edges append the same event
-- through RelationService, and removing the last child appends the inverse
-- StoryStateChanged with the final computed value.
--
-- Multiple parents are now intentional. The mirror trigger and foreign keys
-- still make half/dangling edges unrepresentable, and the service/domain cycle
-- guard remains; only the old one-child-of-row unique index is removed.

DROP INDEX idx_story_relations_single_parent;

CREATE TEMP TABLE sh446_now AS
SELECT strftime('%Y-%m-%dT%H:%M:%SZ', 'now') AS ts;

CREATE TEMP TABLE sh446_epics AS
SELECT DISTINCT s.project_id, s.story_no
  FROM stories s
  JOIN story_relations r
    ON r.project_id = s.project_id
   AND r.story_no = s.story_no
   AND r.relation = 'parent-of'
 WHERE COALESCE(json_extract(s.snapshot, '$.state_computed'), 0) = 0;

INSERT INTO events (project_id, story_no, seq, global_seq, kind, at, payload)
SELECT
    x.project_id,
    x.story_no,
    (
        SELECT COALESCE(MAX(e.seq), 0) + 1
          FROM events e
         WHERE e.project_id = x.project_id AND e.story_no = x.story_no
    ),
    p.next_global_seq - 1 + ROW_NUMBER() OVER (
        PARTITION BY x.project_id ORDER BY x.story_no
    ),
    'StoryStateCleared',
    (SELECT ts FROM sh446_now),
    json_object('kind', 'StoryStateCleared', 'at', (SELECT ts FROM sh446_now))
FROM sh446_epics x
JOIN projects p ON p.id = x.project_id;

UPDATE projects
   SET next_global_seq = next_global_seq + (
       SELECT COUNT(*) FROM sh446_epics x
        WHERE x.project_id = projects.id
   )
 WHERE EXISTS (
       SELECT 1 FROM sh446_epics x
        WHERE x.project_id = projects.id
   );

UPDATE stories
   SET head_seq = (
           SELECT MAX(e.seq) FROM events e
            WHERE e.project_id = stories.project_id
              AND e.story_no = stories.story_no
       ),
       head_global_seq = (
           SELECT MAX(e.global_seq) FROM events e
            WHERE e.project_id = stories.project_id
              AND e.story_no = stories.story_no
       ),
       updated_at = (SELECT ts FROM sh446_now),
       snapshot = json_set(
           json_set(snapshot, '$.state_computed', json('true')),
           '$.updated_at', (SELECT ts FROM sh446_now)
       )
 WHERE EXISTS (
       SELECT 1 FROM sh446_epics x
        WHERE x.project_id = stories.project_id
          AND x.story_no = stories.story_no
   );

DROP TABLE sh446_epics;
DROP TABLE sh446_now;
