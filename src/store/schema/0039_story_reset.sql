-- Operational recovery authority survives removal of the private worktree marker.
CREATE TABLE story_resets (
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    story_no INTEGER NOT NULL,
    reservation TEXT NOT NULL CHECK (json_valid(reservation)),
    PRIMARY KEY (project_id, story_no),
    FOREIGN KEY (project_id, story_no) REFERENCES stories(project_id, story_no) ON DELETE RESTRICT
);
