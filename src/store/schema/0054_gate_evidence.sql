-- Non-authoritative verifier cost observations survive restarts and retries.
CREATE TABLE gate_attempts (
    id TEXT PRIMARY KEY CHECK(length(id) > 0),
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    story_id TEXT NOT NULL CHECK(length(story_id) > 0),
    revision INTEGER NOT NULL CHECK(revision >= 0),
    payload TEXT NOT NULL CHECK(json_valid(payload)),
    CHECK(json_extract(payload, '$.version') IS 1),
    CHECK(json_extract(payload, '$.id') IS id),
    CHECK(json_extract(payload, '$.submission.project') IS project_id),
    CHECK(json_extract(payload, '$.submission.story_id') IS story_id),
    CHECK(json_extract(payload, '$.revision') IS revision)
);
CREATE INDEX gate_attempts_project ON gate_attempts(project_id, story_id);
