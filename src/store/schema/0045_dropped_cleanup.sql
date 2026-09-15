-- A dropped story has no verification generation; its drop event owns cleanup.
CREATE TABLE dropped_cleanups (
    project_id INTEGER NOT NULL,
    story_no INTEGER NOT NULL,
    token TEXT NOT NULL UNIQUE,
    record_json TEXT NOT NULL CHECK (json_valid(record_json)),
    PRIMARY KEY (project_id, story_no),
    FOREIGN KEY (project_id, story_no) REFERENCES stories(project_id, story_no) ON DELETE CASCADE
);
