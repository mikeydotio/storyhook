-- Operational intent survives provider/daemon restarts without changing story states.
CREATE TABLE continuations (
 id TEXT PRIMARY KEY,
 project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
 story_no INTEGER NOT NULL,
 revision INTEGER NOT NULL CHECK(revision >= 0),
 record TEXT NOT NULL CHECK(json_valid(record)),
 FOREIGN KEY(project_id,story_no) REFERENCES stories(project_id,story_no) ON DELETE CASCADE
);
CREATE INDEX continuations_story ON continuations(project_id,story_no);
CREATE UNIQUE INDEX continuation_origin ON continuations(project_id,story_no,
 json_extract(record,'$.generation.provider'),json_extract(record,'$.generation.session_id'),json_extract(record,'$.generation.turn_id'),json_extract(record,'$.handoff.kind'));
CREATE UNIQUE INDEX continuation_outstanding ON continuations(project_id,story_no)
 WHERE json_extract(record,'$.status') NOT IN ('acknowledged','superseded');
