-- storyhook store — schema version 25: `verifying` becomes the required
-- centralized release-gate handoff state (SH-521).
--
-- Existing catalogs keep their relative order. A missing `verifying` is
-- inserted immediately before required `blocked` when it exists, otherwise
-- after the last OPEN state. This is the same repair rule
-- `domain::with_required_states` uses. The two-step offset avoids transient
-- collisions with UNIQUE(project_id, position); SQLite does not promise an
-- UPDATE order that could safely shift adjacent positions in place.
--
-- A project that already owns the slug is untouched, even if it classified it
-- differently. Reclassifying its stories is a separate migration decision;
-- required-state validation reports that catalog honestly instead.

CREATE TEMP TABLE sh521_missing_verifying AS
SELECT
    p.id AS project_id,
    COALESCE(
        (SELECT s.position
           FROM project_states s
          WHERE s.project_id = p.id
            AND s.slug = 'blocked'),
        (SELECT MAX(s.position) + 1
           FROM project_states s
          WHERE s.project_id = p.id
            AND s.superstate = 'OPEN'),
        0
    ) AS insert_position
FROM projects p
WHERE NOT EXISTS (
    SELECT 1
      FROM project_states s
     WHERE s.project_id = p.id
       AND s.slug = 'verifying'
);

UPDATE project_states
   SET position = position + 1000000
 WHERE EXISTS (
    SELECT 1
      FROM sh521_missing_verifying m
     WHERE m.project_id = project_states.project_id
       AND project_states.position >= m.insert_position
);

UPDATE project_states
   SET position = position - 999999
 WHERE EXISTS (
    SELECT 1
      FROM sh521_missing_verifying m
     WHERE m.project_id = project_states.project_id
       AND project_states.position >= m.insert_position + 1000000
);

INSERT INTO project_states (project_id, position, slug, superstate, role, description)
SELECT project_id, insert_position, 'verifying', 'OPEN', NULL, NULL
  FROM sh521_missing_verifying;

DROP TABLE sh521_missing_verifying;
