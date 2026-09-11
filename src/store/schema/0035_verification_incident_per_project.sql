-- storyhook store — schema version 35: one verifier incident per PROJECT (SH-648).
-- Version 31 admitted one machine-wide row (`singleton = 1`) because the
-- centralized verifier was one worker over every project. D-B of epic SH-645
-- runs one worker per project, each with its own incident halt, so the key
-- becomes the project. The existing row, if any, is carried forward under its
-- own project; every other column and CHECK is verbatim from version 31.
--
-- A rebuild rather than an ALTER because SQLite cannot change a primary key
-- in place. `foreign_keys_off` stays false in the registry: this table is a
-- leaf — nothing references it — so dropping and recreating it under live
-- foreign-key enforcement cannot orphan anything.

CREATE TABLE verification_incident_v35 (
  project_id         INTEGER PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
  incident_id        TEXT NOT NULL UNIQUE,
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

INSERT INTO verification_incident_v35
  (project_id, incident_id, story_no, generation, disposition, state, attempts,
   detail, first_failed_at, last_failed_at)
SELECT project_id, incident_id, story_no, generation, disposition, state, attempts,
       detail, first_failed_at, last_failed_at
FROM verification_incident;

DROP TABLE verification_incident;

ALTER TABLE verification_incident_v35 RENAME TO verification_incident;
