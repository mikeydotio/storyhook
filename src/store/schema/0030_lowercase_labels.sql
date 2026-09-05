-- storyhook store — schema version 30: labels have one lowercase identity
-- (SH-204).
--
-- Runtime writes now canonicalize with Rust's Unicode-aware lowercase rules.
-- SQLite's built-in lower() is ASCII-only, so migrate::run registers the
-- deterministic storyhook_normalize_labels_json function used here. It calls
-- the same Rust normalizer as every live producer, including comma splitting,
-- trimming, sorting, and deduplication after case conversion.
--
-- History remains append-only. Each affected story receives one compensating
-- StoryLabelsSet event, including CLOSED stories whose service-layer history
-- cannot ordinarily be edited. The read model is advanced to the same state a
-- fresh fold of that history produces.

CREATE TEMP TABLE sh204_now AS
SELECT strftime('%Y-%m-%dT%H:%M:%SZ', 'now') AS ts;

CREATE TEMP TABLE sh204_repairs AS
WITH candidates AS (
    SELECT
        story.project_id,
        story.story_no,
        storyhook_normalize_labels_json(
            json_extract(story.snapshot, '$.labels')
        ) AS labels
      FROM stories story
), changed AS (
    SELECT *
      FROM candidates
     WHERE labels <> json_extract(
           (SELECT snapshot FROM stories current
             WHERE current.project_id = candidates.project_id
               AND current.story_no = candidates.story_no),
           '$.labels'
       )
), ranked AS (
    SELECT
        changed.*,
        COALESCE((
            SELECT MAX(event.seq)
              FROM events event
             WHERE event.project_id = changed.project_id
               AND event.story_no = changed.story_no
        ), 0) + 1 AS seq,
        project.next_global_seq - 1 + ROW_NUMBER() OVER (
            PARTITION BY changed.project_id
            ORDER BY changed.story_no
        ) AS global_seq
      FROM changed
      JOIN projects project ON project.id = changed.project_id
)
SELECT * FROM ranked;

INSERT INTO events (project_id, story_no, seq, global_seq, kind, at, payload)
SELECT
    repair.project_id,
    repair.story_no,
    repair.seq,
    repair.global_seq,
    'StoryLabelsSet',
    (SELECT ts FROM sh204_now),
    json_object(
        'kind', 'StoryLabelsSet',
        'at', (SELECT ts FROM sh204_now),
        'labels', json(repair.labels)
    )
  FROM sh204_repairs repair
 ORDER BY repair.project_id, repair.story_no;

UPDATE projects
   SET next_global_seq = next_global_seq + (
       SELECT COUNT(*)
         FROM sh204_repairs repair
        WHERE repair.project_id = projects.id
   )
 WHERE EXISTS (
       SELECT 1 FROM sh204_repairs repair
        WHERE repair.project_id = projects.id
   );

UPDATE stories
   SET head_seq = (
           SELECT repair.seq
             FROM sh204_repairs repair
            WHERE repair.project_id = stories.project_id
              AND repair.story_no = stories.story_no
       ),
       head_global_seq = (
           SELECT repair.global_seq
             FROM sh204_repairs repair
            WHERE repair.project_id = stories.project_id
              AND repair.story_no = stories.story_no
       ),
       updated_at = (SELECT ts FROM sh204_now),
       snapshot = json_set(
           snapshot,
           '$.labels', json((
               SELECT repair.labels
                 FROM sh204_repairs repair
                WHERE repair.project_id = stories.project_id
                  AND repair.story_no = stories.story_no
           )),
           '$.updated_at', (SELECT ts FROM sh204_now)
       )
 WHERE EXISTS (
       SELECT 1 FROM sh204_repairs repair
        WHERE repair.project_id = stories.project_id
          AND repair.story_no = stories.story_no
   );

DELETE FROM story_labels
 WHERE EXISTS (
       SELECT 1 FROM sh204_repairs repair
        WHERE repair.project_id = story_labels.project_id
          AND repair.story_no = story_labels.story_no
   );

INSERT INTO story_labels (project_id, story_no, label)
SELECT repair.project_id, repair.story_no, item.value
  FROM sh204_repairs repair, json_each(repair.labels) item
 ORDER BY repair.project_id, repair.story_no, item.value;

DROP TABLE sh204_repairs;
DROP TABLE sh204_now;
