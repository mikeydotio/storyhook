-- Diagnosis remains separate from gate certification and its receipts.
CREATE TABLE verification_attributions (
    id TEXT PRIMARY KEY CHECK(length(id) > 0),
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    story_id TEXT NOT NULL CHECK(length(story_id) > 0),
    generation INTEGER,
    attempt_id TEXT NOT NULL CHECK(length(attempt_id) > 0),
    revision INTEGER NOT NULL CHECK(revision >= 0),
    payload TEXT NOT NULL CHECK(json_valid(payload)),
    UNIQUE(project_id, story_id, generation, attempt_id),
    CHECK(json_extract(payload, '$.version') IS 1),
    CHECK(json_extract(payload, '$.id') IS id),
    CHECK(json_extract(payload, '$.submission.project') IS project_id),
    CHECK(json_extract(payload, '$.submission.story_id') IS story_id),
    CHECK(json_extract(payload, '$.submission.generation') IS generation),
    CHECK(json_extract(payload, '$.attempt') IS attempt_id),
    CHECK(json_extract(payload, '$.revision') IS revision)
);
CREATE INDEX verification_attributions_submission ON verification_attributions(project_id, story_id, generation);
