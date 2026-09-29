-- storyhook store — schema version 52: a verification batch can land (SH-832).
-- Version 51 allowed the five SH-831 phases. Landing adds `landing` (live: the
-- member landing intents are written and the batch merge is requested or
-- uncertain) and `landed` (ended: the merge is confirmed and its members are
-- done). `live` stays derived from the phase and still backs the
-- one-live-batch-per-project index.
--
-- A rebuild rather than an ALTER because SQLite cannot change a CHECK in
-- place. `foreign_keys_off` stays false in the registry: this table is a leaf
-- (nothing references it), so dropping and recreating it under live
-- foreign-key enforcement cannot orphan anything. Rows keep their rowid,
-- because batch listing and retention order by it.

CREATE TABLE verification_batches_v52 (
    id TEXT PRIMARY KEY CHECK (length(id) = 12),
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    revision INTEGER NOT NULL CHECK (revision >= 0),
    live INTEGER NOT NULL CHECK (live IN (0, 1)),
    payload TEXT NOT NULL CHECK (json_valid(payload)),
    CHECK (json_extract(payload, '$.id') IS id),
    CHECK (json_extract(payload, '$.project') IS project_id),
    CHECK (json_extract(payload, '$.revision') IS revision),
    CHECK (json_extract(payload, '$.phase')
        IN ('assembled', 'submitted', 'gating', 'landing', 'landed', 'released', 'abandoned')),
    CHECK (live IS (json_extract(payload, '$.phase') IN ('assembled', 'submitted', 'gating', 'landing')))
);

INSERT INTO verification_batches_v52 (rowid, id, project_id, revision, live, payload)
SELECT rowid, id, project_id, revision, live, payload
FROM verification_batches;

DROP TABLE verification_batches;

ALTER TABLE verification_batches_v52 RENAME TO verification_batches;

CREATE UNIQUE INDEX verification_batch_live ON verification_batches(project_id) WHERE live = 1;
