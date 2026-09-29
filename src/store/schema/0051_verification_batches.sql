-- One verification batch per row (SH-831): history, plus the evidence a
-- restarted verifier needs to abandon a batch it left live. A batch grants no
-- authority over its member stories, so the row names no story key and is not
-- an ownership-fence owner; members live in the payload. `live` is derived from
-- the payload's phase and backs the one-live-batch-per-project index.
CREATE TABLE verification_batches (
    id TEXT PRIMARY KEY CHECK (length(id) = 12),
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    revision INTEGER NOT NULL CHECK (revision >= 0),
    live INTEGER NOT NULL CHECK (live IN (0, 1)),
    payload TEXT NOT NULL CHECK (json_valid(payload)),
    CHECK (json_extract(payload, '$.id') IS id),
    CHECK (json_extract(payload, '$.project') IS project_id),
    CHECK (json_extract(payload, '$.revision') IS revision),
    CHECK (json_extract(payload, '$.phase')
        IN ('assembled', 'submitted', 'gating', 'released', 'abandoned')),
    CHECK (live IS (json_extract(payload, '$.phase') IN ('assembled', 'submitted', 'gating')))
);
CREATE UNIQUE INDEX verification_batch_live ON verification_batches(project_id) WHERE live = 1;
