-- SH-663: abandonment is `dropped`; CLOSED remains the superstate.
-- Validate every project before mutating any. The Rust function shares the
-- import path's collision policy and includes the project in its diagnostics.
SELECT storyhook_validate_dropped_catalog(p.slug, (
    SELECT json_group_array(json_object('slug', slug, 'super', superstate,
                                       'role', role, 'description', description))
      FROM project_states WHERE project_id = p.id
)) FROM projects p;

-- Compute occupants before renaming. Legacy soft-deletion remains active
-- until an OPEN StoryStateChanged retracts it; match the fold, bounded by the
-- read model's head. This also covers old custom closed/OPEN projects whose
-- soft-deleted stories rested in done before dropped became available.
CREATE TEMP TABLE sh663_rows AS
SELECT s.project_id, s.story_no FROM stories s
WHERE s.superstate = 'CLOSED' AND (
    s.state = 'closed'
    OR EXISTS (
        SELECT 1 FROM events deleted
        WHERE deleted.project_id = s.project_id AND deleted.story_no = s.story_no
          AND deleted.seq <= s.head_seq AND deleted.kind = 'StoryDeleted'
          AND NOT EXISTS (
              SELECT 1 FROM events reopened
              JOIN project_states ps ON ps.project_id = reopened.project_id
                  AND ps.slug = json_extract(reopened.payload, '$.state')
              WHERE reopened.project_id = s.project_id AND reopened.story_no = s.story_no
                AND reopened.seq > deleted.seq AND reopened.seq <= s.head_seq
                AND reopened.kind = 'StoryStateChanged' AND ps.superstate = 'OPEN'
          )
    )
);

-- The story/catalog composite foreign key is deferred until commit. No table
-- rebuild, deletion or disabled foreign-key enforcement is necessary.
UPDATE project_states SET slug = 'dropped' WHERE slug = 'closed' AND superstate = 'CLOSED';
INSERT INTO project_states (project_id, position, slug, superstate, role, description)
SELECT p.id, (SELECT COALESCE(MAX(position), -1) + 1 FROM project_states WHERE project_id = p.id),
       'dropped', 'CLOSED', NULL, NULL
FROM projects p
WHERE NOT EXISTS (SELECT 1 FROM project_states WHERE project_id = p.id AND slug = 'dropped');

-- A semantic rename is not a new story event or user edit. Preserve heads,
-- timestamps, visibility, and every other coordinate of the read model.
UPDATE stories SET state = 'dropped', snapshot = json_set(snapshot, '$.state', 'dropped')
WHERE EXISTS (SELECT 1 FROM sh663_rows r WHERE r.project_id = stories.project_id AND r.story_no = stories.story_no);
DROP TABLE sh663_rows;
