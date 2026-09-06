-- storyhook store — schema version 31: one durable machine-wide verifier incident (SH-573).
-- The centralized verifier is serialized across every project, so at most one
-- infrastructure failure can own the queue. This is operational state, not a
-- story event: comments remain the human-readable audit record.

CREATE TABLE verification_incident (
  singleton          INTEGER PRIMARY KEY CHECK (singleton = 1),
  incident_id        TEXT NOT NULL UNIQUE,
  project_id         INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  story_no           INTEGER NOT NULL,
  generation         INTEGER NOT NULL,
  disposition        TEXT NOT NULL CHECK (disposition IN ('retryable','permanent')),
  state              TEXT NOT NULL CHECK (state IN ('retrying','halted')),
  attempts           INTEGER NOT NULL CHECK (attempts >= 1),
  detail             TEXT NOT NULL,
  first_failed_at    TEXT NOT NULL,
  last_failed_at     TEXT NOT NULL,
  FOREIGN KEY (project_id, story_no) REFERENCES stories(project_id, story_no) ON DELETE CASCADE
);
