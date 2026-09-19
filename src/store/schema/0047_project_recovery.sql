-- Coordination is mutable; observations are immutable attempt evidence.
CREATE TABLE project_recoveries (
    id TEXT PRIMARY KEY NOT NULL CHECK(length(id) > 0),
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    code TEXT NOT NULL CHECK(length(code) > 0),
    locus TEXT NOT NULL CHECK(length(locus) > 0),
    revision INTEGER NOT NULL CHECK(revision >= 0),
    active INTEGER NOT NULL CHECK(active IN (0,1)),
    state TEXT NOT NULL CHECK(json_valid(state) AND json_type(state) = 'object'),
    UNIQUE(project_id,id)
);
CREATE UNIQUE INDEX one_active_project_fault
    ON project_recoveries(project_id,code,locus) WHERE active=1;

CREATE TABLE project_recovery_observations (
    project_id INTEGER NOT NULL,
    recovery_id TEXT NOT NULL,
    story_no INTEGER NOT NULL,
    generation INTEGER NOT NULL CHECK(generation > 0),
    attempt_id TEXT NOT NULL CHECK(length(attempt_id) > 0),
    observed_at TEXT NOT NULL CHECK(length(observed_at) > 0),
    evidence TEXT NOT NULL CHECK(json_valid(evidence) AND json_type(evidence) = 'object'),
    PRIMARY KEY(project_id,attempt_id),
    FOREIGN KEY(project_id,recovery_id) REFERENCES project_recoveries(project_id,id) ON DELETE CASCADE,
    FOREIGN KEY(project_id,story_no) REFERENCES stories(project_id,story_no) ON DELETE CASCADE
);
CREATE INDEX project_recovery_evidence
    ON project_recovery_observations(project_id,recovery_id);
CREATE TRIGGER project_recovery_observations_immutable
    BEFORE UPDATE ON project_recovery_observations
    BEGIN SELECT RAISE(ABORT, 'project recovery observations are immutable'); END;
-- Match the event-store purge boundary: explicit parent deletion may cascade,
-- but ordinary writes cannot erase evidence while its owning records exist.
CREATE TRIGGER project_recovery_observations_reject_delete
    BEFORE DELETE ON project_recovery_observations
    WHEN EXISTS(SELECT 1 FROM projects WHERE id=OLD.project_id)
      AND EXISTS(SELECT 1 FROM stories WHERE project_id=OLD.project_id AND story_no=OLD.story_no)
      AND EXISTS(SELECT 1 FROM project_recoveries WHERE project_id=OLD.project_id AND id=OLD.recovery_id)
    BEGIN SELECT RAISE(ABORT, 'project recovery observations are append-only'); END;

CREATE TRIGGER project_recovery_identity_immutable
    BEFORE UPDATE OF id,project_id,code,locus ON project_recoveries
    WHEN NEW.id != OLD.id OR NEW.project_id != OLD.project_id OR NEW.code != OLD.code OR NEW.locus != OLD.locus
    BEGIN SELECT RAISE(ABORT, 'project recovery identity is immutable'); END;
