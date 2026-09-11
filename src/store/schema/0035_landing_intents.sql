-- SH-656: external merge authority must survive the connection that admitted it.
-- Restrict deletion: erasing a project/story must not erase an unresolved fence.
CREATE TABLE landing_intents (
    id TEXT PRIMARY KEY CHECK (length(id) > 0),
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE RESTRICT,
    story_no INTEGER NOT NULL,
    payload TEXT NOT NULL CHECK (json_valid(payload)),
    UNIQUE (project_id, story_no),
    CHECK (json_extract(payload, '$.id') IS id),
    CHECK (json_extract(payload, '$.project') IS project_id),
    CHECK (json_extract(payload, '$.story') IS story_no),
    FOREIGN KEY (project_id, story_no) REFERENCES stories(project_id, story_no) ON DELETE RESTRICT
);
