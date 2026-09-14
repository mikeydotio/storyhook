-- Reset ownership is operational state, not a story block. RESTRICT keeps
-- deletion from silently discarding a live reservation and its cleanup proof.
CREATE TABLE engine_resets (
    project_id INTEGER NOT NULL REFERENCES projects(id),
    story_no INTEGER NOT NULL,
    run_id TEXT NOT NULL,
    lane_index INTEGER NOT NULL,
    token TEXT NOT NULL UNIQUE,
    record_json TEXT NOT NULL CHECK (json_valid(record_json)),
    PRIMARY KEY (project_id, story_no),
    UNIQUE (run_id, lane_index),
    FOREIGN KEY (project_id, story_no) REFERENCES stories(project_id, story_no),
    FOREIGN KEY (run_id, lane_index) REFERENCES engine_lanes(run_id, lane_index)
);
