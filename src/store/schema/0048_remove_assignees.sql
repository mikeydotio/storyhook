-- SH-752: remove structured assignment data. The migration runner backs up
-- the store and encloses this entire purge in one transaction.
CREATE TEMP TABLE sh752_affected AS
SELECT DISTINCT project_id, story_no FROM events
WHERE kind IN ('StoryAssigned', 'StoryAssigneeCleared');

DROP TRIGGER events_reject_delete;
DELETE FROM events WHERE kind IN ('StoryAssigned', 'StoryAssigneeCleared');
CREATE TRIGGER events_reject_delete
BEFORE DELETE ON events
WHEN EXISTS (SELECT 1 FROM projects WHERE id = OLD.project_id)
 AND EXISTS (SELECT 1 FROM stories
             WHERE project_id = OLD.project_id AND story_no = OLD.story_no)
BEGIN
    SELECT RAISE(ABORT, 'events are append-only: DELETE is not permitted');
END;

UPDATE stories AS s SET
    head_seq = (SELECT COALESCE(MAX(seq), 0) FROM events e
                WHERE e.project_id = s.project_id AND e.story_no = s.story_no),
    head_global_seq = COALESCE((SELECT global_seq FROM events e
                WHERE e.project_id = s.project_id AND e.story_no = s.story_no
                ORDER BY seq DESC LIMIT 1), 0),
    updated_at = storyhook_assignment_purge_activity(
        (SELECT prefix FROM projects WHERE id = s.project_id) || '-' || s.story_no,
        (SELECT json_group_array(json(payload)) FROM
            (SELECT payload FROM events e WHERE e.project_id = s.project_id
             AND e.story_no = s.story_no ORDER BY seq)),
        (SELECT json_group_array(json_object('slug', slug, 'super', superstate,
                    'role', role, 'description', description))
         FROM project_states WHERE project_id = s.project_id))
WHERE (project_id, story_no) IN (SELECT project_id, story_no FROM sh752_affected);

UPDATE stories SET snapshot = json_remove(snapshot, '$.assignee');
UPDATE stories SET snapshot = json_set(snapshot, '$.updated_at', updated_at)
WHERE (project_id, story_no) IN (SELECT project_id, story_no FROM sh752_affected);
DROP TABLE sh752_affected;
DROP INDEX idx_stories_assignee;
ALTER TABLE stories DROP COLUMN assignee;
DROP TABLE project_members;
