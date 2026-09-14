-- Stable and development operational authority share one canonical schema.
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
CREATE TABLE story_reset_reservations (
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    story_no INTEGER NOT NULL,
    reservation TEXT NOT NULL CHECK (json_valid(reservation)),
    PRIMARY KEY (project_id, story_no),
    FOREIGN KEY (project_id, story_no) REFERENCES stories(project_id, story_no) ON DELETE RESTRICT
);
CREATE TABLE schema_lineage (
    source TEXT PRIMARY KEY,
    source_version INTEGER NOT NULL,
    migrations_json TEXT NOT NULL CHECK (json_valid(migrations_json)),
    bridged_at TEXT NOT NULL
);

-- Older producers did not bind pending effects to a revocable block episode.
-- Preserve history and targets, but never discover a new session for old authority.
UPDATE block_deliveries
SET status = 'superseded',
    detail = 'Legacy pending delivery retired at schema 44; its block episode and session authority cannot be proved.'
WHERE status = 'pending';

-- A legacy attempt has neither immutable scope nor inherited helper exclusion.
-- Its effect may have happened; this row can never authorize automatic replay.
UPDATE block_deliveries
SET status = 'uncertain',
    detail = 'Legacy delivery may have reached an agent; helper quiescence is unknown; no automatic replay.'
WHERE status = 'attempting';
