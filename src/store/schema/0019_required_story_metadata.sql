-- storyhook store — schema version 19: every story has a type and one of the
-- four assignable priorities (SH-449).
--
-- Legacy histories stay decodable. This migration appends the explicit
-- default events the service now writes, then rebuilds `stories` so current
-- read-model rows cannot return to the old nullable/parked states.

CREATE TEMP TABLE sh449_now AS
SELECT strftime('%Y-%m-%dT%H:%M:%SZ', 'now') AS ts;

-- Fix a deterministic order in both sequence domains: project, story, then
-- type before priority. Per-story sequence allocation starts after the event
-- log's actual head; project-global allocation starts at its stored counter.
CREATE TEMP TABLE sh449_repairs AS
WITH candidates AS (
    SELECT
        s.project_id,
        s.story_no,
        1 AS event_order,
        'StoryTypeSet' AS kind,
        (
            SELECT pt.slug
              FROM project_types pt
             WHERE pt.project_id = s.project_id
             ORDER BY pt.position, pt.slug
             LIMIT 1
        ) AS event_value
      FROM stories s
     WHERE s.story_type IS NULL

    UNION ALL

    SELECT
        s.project_id,
        s.story_no,
        2 AS event_order,
        'StoryPrioritySet' AS kind,
        'low' AS event_value
      FROM stories s
     WHERE s.priority = 'none'
        OR COALESCE(json_extract(s.snapshot, '$.priority_assessed'), 0) = 0
),
ranked AS (
    SELECT
        c.*,
        COALESCE((
            SELECT MAX(e.seq)
              FROM events e
             WHERE e.project_id = c.project_id
               AND e.story_no = c.story_no
        ), 0) + ROW_NUMBER() OVER (
            PARTITION BY c.project_id, c.story_no
            ORDER BY c.event_order
        ) AS seq,
        p.next_global_seq - 1 + ROW_NUMBER() OVER (
            PARTITION BY c.project_id
            ORDER BY c.story_no, c.event_order
        ) AS global_seq
      FROM candidates c
      JOIN projects p ON p.id = c.project_id
)
SELECT * FROM ranked;

-- Append rather than rewrite. The historical representation remains intact,
-- while a fresh fold ends at the exact state stored in the read model below.
INSERT INTO events (project_id, story_no, seq, global_seq, kind, at, payload)
SELECT
    project_id,
    story_no,
    seq,
    global_seq,
    kind,
    (SELECT ts FROM sh449_now),
    CASE kind
        WHEN 'StoryTypeSet' THEN json_object(
            'kind', kind,
            'at', (SELECT ts FROM sh449_now),
            'story_type', event_value
        )
        WHEN 'StoryPrioritySet' THEN json_object(
            'kind', kind,
            'at', (SELECT ts FROM sh449_now),
            'priority', event_value
        )
    END
FROM sh449_repairs
ORDER BY project_id, story_no, event_order;

UPDATE projects
   SET next_global_seq = next_global_seq + (
       SELECT COUNT(*)
         FROM sh449_repairs r
        WHERE r.project_id = projects.id
   )
 WHERE EXISTS (
       SELECT 1 FROM sh449_repairs r WHERE r.project_id = projects.id
   );

-- Patch the fields each event changes. The final update advances the shared
-- head and timestamp once, to the last repair event for that story.
UPDATE stories
   SET story_type = (
           SELECT r.event_value
             FROM sh449_repairs r
            WHERE r.project_id = stories.project_id
              AND r.story_no = stories.story_no
              AND r.kind = 'StoryTypeSet'
       ),
       snapshot = json_set(
           snapshot,
           '$.story_type',
           (
               SELECT r.event_value
                 FROM sh449_repairs r
                WHERE r.project_id = stories.project_id
                  AND r.story_no = stories.story_no
                  AND r.kind = 'StoryTypeSet'
           )
       )
 WHERE EXISTS (
       SELECT 1
         FROM sh449_repairs r
        WHERE r.project_id = stories.project_id
          AND r.story_no = stories.story_no
          AND r.kind = 'StoryTypeSet'
   );

UPDATE stories
   SET priority = 'low',
       priority_rank = 3,
       snapshot = json_set(
           snapshot,
           '$.priority', 'low',
           '$.priority_assessed', json('true')
       )
 WHERE EXISTS (
       SELECT 1
         FROM sh449_repairs r
        WHERE r.project_id = stories.project_id
          AND r.story_no = stories.story_no
          AND r.kind = 'StoryPrioritySet'
   );

UPDATE stories
   SET head_seq = (
           SELECT MAX(r.seq)
             FROM sh449_repairs r
            WHERE r.project_id = stories.project_id
              AND r.story_no = stories.story_no
       ),
       head_global_seq = (
           SELECT r.global_seq
             FROM sh449_repairs r
            WHERE r.project_id = stories.project_id
              AND r.story_no = stories.story_no
            ORDER BY r.event_order DESC
            LIMIT 1
       ),
       updated_at = (SELECT ts FROM sh449_now),
       snapshot = json_set(
           snapshot,
           '$.updated_at', (SELECT ts FROM sh449_now)
       )
 WHERE EXISTS (
       SELECT 1
         FROM sh449_repairs r
        WHERE r.project_id = stories.project_id
          AND r.story_no = stories.story_no
   );

-- This trigger names `stories`; it must be bracketed around the table rebuild
-- so SQLite can re-parse the rename while the referenced table exists.
DROP TRIGGER events_reject_delete;

CREATE TABLE stories_new (
    project_id      INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    story_no        INTEGER NOT NULL,
    head_seq        INTEGER NOT NULL,
    head_global_seq INTEGER NOT NULL DEFAULT 0,
    title           TEXT NOT NULL,
    state           TEXT NOT NULL,
    superstate      TEXT NOT NULL CHECK (superstate IN ('OPEN', 'CLOSED')),
    priority        TEXT NOT NULL,
    priority_rank   INTEGER NOT NULL,
    story_type      TEXT NOT NULL,
    assignee        TEXT,
    awaiting        TEXT,
    deleted         INTEGER NOT NULL DEFAULT 0 CHECK (deleted IN (0, 1)),
    archived        INTEGER NOT NULL DEFAULT 0 CHECK (archived IN (0, 1)),
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    closed_at       TEXT,
    description     TEXT,
    snapshot        TEXT NOT NULL,
    hidden_at       TEXT,
    draft           INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (project_id, story_no),
    CHECK (story_no >= 1),
    CHECK (head_seq >= 0),
    CHECK (archived = (closed_at IS NOT NULL)),
    CHECK ((superstate = 'CLOSED') = archived),
    CHECK (priority IN ('critical', 'high', 'medium', 'low')),
    CHECK (priority_rank = CASE priority
        WHEN 'critical' THEN 0
        WHEN 'high'     THEN 1
        WHEN 'medium'   THEN 2
        WHEN 'low'      THEN 3
    END),
    FOREIGN KEY (project_id, state, superstate)
        REFERENCES project_states(project_id, slug, superstate)
        DEFERRABLE INITIALLY DEFERRED
);

INSERT INTO stories_new (
    project_id, story_no, head_seq, head_global_seq, title, state, superstate,
    priority, priority_rank, story_type, assignee, awaiting, deleted, archived,
    created_at, updated_at, closed_at, description, snapshot, hidden_at, draft
)
SELECT
    project_id, story_no, head_seq, head_global_seq, title, state, superstate,
    priority, priority_rank, story_type, assignee, awaiting, deleted, archived,
    created_at, updated_at, closed_at, description, snapshot, hidden_at, draft
FROM stories;

DROP TABLE stories;
ALTER TABLE stories_new RENAME TO stories;

CREATE INDEX idx_stories_state
    ON stories(project_id, superstate, state);
CREATE INDEX idx_stories_priority
    ON stories(project_id, priority_rank, story_no);
CREATE INDEX idx_stories_assignee
    ON stories(project_id, assignee);
CREATE INDEX idx_stories_updated
    ON stories(project_id, updated_at, head_global_seq);

CREATE TRIGGER events_reject_delete
BEFORE DELETE ON events
WHEN EXISTS (SELECT 1 FROM projects WHERE id = OLD.project_id)
 AND EXISTS (SELECT 1 FROM stories
             WHERE project_id = OLD.project_id AND story_no = OLD.story_no)
BEGIN
    SELECT RAISE(ABORT, 'events are append-only: DELETE is not permitted');
END;

DROP TABLE sh449_repairs;
DROP TABLE sh449_now;
