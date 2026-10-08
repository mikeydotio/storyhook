-- Single-submission integration ownership is distinct from batching. One live
-- owner retains the story across changed heads, restart and uncertain effects.
CREATE TABLE integration_recoveries (
    id TEXT PRIMARY KEY NOT NULL CHECK(length(id) > 0),
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    story_no INTEGER NOT NULL,
    generation INTEGER NOT NULL CHECK(generation > 0),
    revision INTEGER NOT NULL CHECK(revision >= 0),
    active INTEGER NOT NULL CHECK(active IN (0,1)),
    state TEXT NOT NULL CHECK(json_valid(state) AND json_type(state) = 'object'),
    FOREIGN KEY(project_id,story_no) REFERENCES stories(project_id,story_no) ON DELETE RESTRICT
);
CREATE UNIQUE INDEX one_active_integration_owner
    ON integration_recoveries(project_id,story_no) WHERE active=1;
CREATE TRIGGER integration_recovery_identity_immutable
    BEFORE UPDATE OF id,project_id,story_no,generation ON integration_recoveries
    WHEN NEW.id != OLD.id OR NEW.project_id != OLD.project_id
      OR NEW.story_no != OLD.story_no OR NEW.generation != OLD.generation
    BEGIN SELECT RAISE(ABORT, 'integration recovery identity is immutable'); END;
CREATE TRIGGER integration_recovery_no_resurrection
    BEFORE UPDATE OF active ON integration_recoveries
    WHEN OLD.active=0 AND NEW.active=1
    BEGIN SELECT RAISE(ABORT, 'retired integration recovery cannot reactivate'); END;
