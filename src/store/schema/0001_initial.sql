-- storyhook store — schema version 1.
--
-- Version 0 is the legacy world: per-repository `.storyhook/` trees whose only
-- database was `archive/archive.db`, captured at
-- `docs/rearch/baseline/archive-schema.sql`. Nothing here migrates from it;
-- the legacy importer (W3) reads that world through its own reader and writes
-- through this schema's ordinary write path.
--
-- Portability rules observed throughout, so that a Postgres implementation
-- stays possible: no AUTOINCREMENT, no WITHOUT ROWID, RFC3339 TEXT timestamps,
-- `ON CONFLICT DO UPDATE` rather than `INSERT OR REPLACE`, `RETURNING` (SQLite
-- 3.35+, Postgres forever). The two SQLite-specific constructs are the mirror
-- triggers and the partial unique index, and both have direct Postgres
-- equivalents.

CREATE TABLE schema_migrations (
    version    INTEGER PRIMARY KEY,
    name       TEXT NOT NULL,
    applied_at TEXT NOT NULL
);

-- ---------------------------------------------------------------------------
-- Projects
-- ---------------------------------------------------------------------------

-- `next_story_no` and `next_global_seq` are the whole ID-collision fix. They
-- are allocated by `UPDATE projects SET next_story_no = next_story_no + 1
-- WHERE id = ? RETURNING next_story_no - 1` *inside the write transaction that
-- uses the number*, so two checkouts of one repository cannot mint the same
-- story twice — the corruption that forced a hand-reconciled merge (2ba5eec).
CREATE TABLE projects (
    id              INTEGER PRIMARY KEY,
    uuid            TEXT NOT NULL UNIQUE,
    slug            TEXT NOT NULL UNIQUE,
    name            TEXT NOT NULL,
    prefix          TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    next_story_no   INTEGER NOT NULL DEFAULT 1,
    next_global_seq INTEGER NOT NULL DEFAULT 1,
    CHECK (length(prefix) > 0),
    CHECK (next_story_no >= 1),
    CHECK (next_global_seq >= 1)
);

-- A project has MANY checkouts: a main working tree and any number of linked
-- worktrees, all resolving to one project. That plurality is what ends SH-46 —
-- under the old design each directory *was* the tracker, so a worktree was a
-- separate, silently divergent database.
--
-- The unique index on `path` alone (not just the composite primary key) is the
-- load-bearing half: it makes "two projects claim the same directory"
-- unrepresentable, so path resolution can never be ambiguous.
CREATE TABLE project_paths (
    project_id   INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    path         TEXT NOT NULL,
    kind         TEXT NOT NULL CHECK (kind IN ('main', 'worktree')),
    last_seen_at TEXT NOT NULL,
    PRIMARY KEY (project_id, path)
);

CREATE UNIQUE INDEX idx_project_paths_path ON project_paths(path);

-- ---------------------------------------------------------------------------
-- Events — the source of truth
-- ---------------------------------------------------------------------------

-- `payload` holds the event's JSON exactly as it was written, and `kind` is
-- that payload's discriminant lifted into a column. The denormalization is
-- deliberate: it lets a storyhook that has never heard of an event kind read
-- the row, report it, and leave it alone, instead of failing the whole read on
-- a serde error (SH-54).
--
-- `seq` is per story and is the compare-and-swap target. `global_seq` is per
-- project and is the change feed a daemon pages through.
CREATE TABLE events (
    project_id INTEGER NOT NULL REFERENCES projects(id),
    story_no   INTEGER NOT NULL,
    seq        INTEGER NOT NULL,
    global_seq INTEGER NOT NULL,
    kind       TEXT NOT NULL,
    at         TEXT NOT NULL,
    payload    TEXT NOT NULL,
    PRIMARY KEY (project_id, story_no, seq),
    UNIQUE (project_id, global_seq),
    CHECK (seq >= 1),
    CHECK (global_seq >= 1),
    CHECK (story_no >= 1)
);

-- Append-only, enforced rather than merely intended. The event log is the
-- source of truth; a layer that can rewrite it can rewrite history and leave
-- every folded read model right about a past that never happened.
--
-- Note the deliberate absence of `ON DELETE CASCADE` on `events.project_id`:
-- deleting a project that has events must fail loudly. A future
-- `delete_project` operation has to drop these guards inside its own migration
-- and say so — it cannot happen by accident.
CREATE TRIGGER events_reject_update
BEFORE UPDATE ON events
BEGIN
    SELECT RAISE(ABORT, 'events are append-only: UPDATE is not permitted');
END;

CREATE TRIGGER events_reject_delete
BEFORE DELETE ON events
BEGIN
    SELECT RAISE(ABORT, 'events are append-only: DELETE is not permitted');
END;

-- ---------------------------------------------------------------------------
-- Stories — the read model
-- ---------------------------------------------------------------------------

-- A column exists here if and only if something filters, sorts, or joins on
-- it; everything else lives in `snapshot`, which holds the folded
-- `StorySnapshot` verbatim (comments included). `head_seq` records the event
-- this row was folded from, which is what lets the rebuild oracle tell a stale
-- row from a wrong one.
--
-- `archived` replaces the legacy `open/stories/*.jsonl` versus
-- `archive/archive.db` split entirely — one flag on one row instead of two
-- storage media that could, and did, disagree (SH-20). The CHECK ties it to
-- `closed_at` so the two can never drift: a story is archived exactly when it
-- has a close timestamp.
--
-- `priority_rank` is `priority` in sortable form, tied to it by a CHECK. The
-- CHECK also validates the slug: an unknown priority makes the CASE yield NULL
-- and the constraint fail.
CREATE TABLE stories (
    project_id    INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    story_no      INTEGER NOT NULL,
    head_seq      INTEGER NOT NULL,
    title         TEXT NOT NULL,
    state         TEXT NOT NULL,
    superstate    TEXT NOT NULL CHECK (superstate IN ('OPEN', 'CLOSED')),
    priority      TEXT NOT NULL,
    priority_rank INTEGER NOT NULL,
    story_type    TEXT,
    assignee      TEXT,
    awaiting      TEXT,
    deleted       INTEGER NOT NULL DEFAULT 0 CHECK (deleted IN (0, 1)),
    archived      INTEGER NOT NULL DEFAULT 0 CHECK (archived IN (0, 1)),
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    closed_at     TEXT,
    description   TEXT,
    snapshot      TEXT NOT NULL,
    PRIMARY KEY (project_id, story_no),
    CHECK (story_no >= 1),
    CHECK (head_seq >= 0),
    CHECK (archived = (closed_at IS NOT NULL)),
    CHECK (priority_rank = CASE priority
        WHEN 'critical' THEN 0
        WHEN 'high'     THEN 1
        WHEN 'medium'   THEN 2
        WHEN 'low'      THEN 3
        WHEN 'none'     THEN 4
    END)
);

CREATE INDEX idx_stories_state    ON stories(project_id, superstate, state);
CREATE INDEX idx_stories_priority ON stories(project_id, priority_rank, story_no);
CREATE INDEX idx_stories_assignee ON stories(project_id, assignee);
CREATE INDEX idx_stories_updated  ON stories(project_id, updated_at);

CREATE TABLE story_labels (
    project_id INTEGER NOT NULL,
    story_no   INTEGER NOT NULL,
    label      TEXT NOT NULL,
    PRIMARY KEY (project_id, story_no, label),
    FOREIGN KEY (project_id, story_no)
        REFERENCES stories(project_id, story_no) ON DELETE CASCADE
);

CREATE INDEX idx_story_labels_label ON story_labels(project_id, label);

-- ---------------------------------------------------------------------------
-- Relations — symmetric by construction
-- ---------------------------------------------------------------------------

-- The relation vocabulary as data, so the mirror triggers can look an inverse
-- up rather than encode one in a CASE that would then have two homes. The
-- foreign key from `story_relations.relation` makes an unrecognised relation a
-- rejected write rather than a one-sided edge nothing can mirror.
--
-- Mirrors `domain::inverse_relation`; `relation_vocabulary_matches_domain` in
-- the store's unit tests fails if the two ever disagree.
CREATE TABLE relation_inverses (
    relation TEXT PRIMARY KEY,
    inverse  TEXT NOT NULL
);

INSERT INTO relation_inverses (relation, inverse) VALUES
    ('relates-to',   'relates-to'),
    ('blocks',       'blocked-by'),
    ('blocked-by',   'blocks'),
    ('parent-of',    'child-of'),
    ('child-of',     'parent-of'),
    ('duplicate-of', 'duplicate-of'),
    ('obviates',     'obviated-by'),
    ('obviated-by',  'obviates');

-- Both directions of an edge are stored, but only one is ever written: the
-- triggers below materialize the other. Half of a bidirectional relation is
-- therefore not a state this table can hold — which retires the largest family
-- of the 15 live `story doctor` violations recorded as SH-60, and retires the
-- code that used to write the two halves as two separate operations.
--
-- Both ends are foreign keys into `stories`, so a relation to a story that does
-- not exist is likewise unrepresentable (the "dangling relation" doctor check).
CREATE TABLE story_relations (
    project_id INTEGER NOT NULL,
    story_no   INTEGER NOT NULL,
    relation   TEXT NOT NULL REFERENCES relation_inverses(relation),
    other_no   INTEGER NOT NULL,
    PRIMARY KEY (project_id, story_no, relation, other_no),
    FOREIGN KEY (project_id, story_no)
        REFERENCES stories(project_id, story_no) ON DELETE CASCADE,
    FOREIGN KEY (project_id, other_no)
        REFERENCES stories(project_id, story_no) ON DELETE CASCADE,
    CHECK (story_no <> other_no)
);

CREATE INDEX idx_story_relations_reverse
    ON story_relations(project_id, other_no, relation);

-- At most one inbound `parent-of`, expressed as: at most one `child-of` per
-- story. A partial unique index, not a validation pass — SH-60's second family
-- was stories with multiple parents that survived indefinitely because nothing
-- rejected the write that created them.
CREATE UNIQUE INDEX idx_story_relations_single_parent
    ON story_relations(project_id, story_no)
    WHERE relation = 'child-of';

-- The mirror. `PRAGMA recursive_triggers` is OFF (SQLite's default, and set
-- explicitly at open) so the insert below does not re-fire this trigger; the
-- statement is a plain INSERT rather than INSERT OR IGNORE because OR IGNORE
-- would swallow the single-parent violation and leave exactly the one-sided
-- edge this design exists to prevent.
CREATE TRIGGER story_relations_mirror_insert
AFTER INSERT ON story_relations
BEGIN
    INSERT INTO story_relations (project_id, story_no, relation, other_no)
    SELECT NEW.project_id, NEW.other_no, relation_inverses.inverse, NEW.story_no
    FROM relation_inverses
    WHERE relation_inverses.relation = NEW.relation;
END;

CREATE TRIGGER story_relations_mirror_delete
AFTER DELETE ON story_relations
BEGIN
    DELETE FROM story_relations
    WHERE project_id = OLD.project_id
      AND story_no   = OLD.other_no
      AND other_no   = OLD.story_no
      AND relation   = (SELECT inverse FROM relation_inverses
                        WHERE relation = OLD.relation);
END;

-- ---------------------------------------------------------------------------
-- Per-project configuration — columns, not blobs
-- ---------------------------------------------------------------------------

-- Every configured field gets a column. Reading a document into a struct,
-- mutating one field and writing the whole thing back is how SH-49 destroyed a
-- state's `description`: the struct in memory did not have the field, so the
-- write did not have it either.

CREATE TABLE project_states (
    project_id  INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    position    INTEGER NOT NULL,
    slug        TEXT NOT NULL,
    superstate  TEXT NOT NULL CHECK (superstate IN ('OPEN', 'CLOSED')),
    role        TEXT,
    description TEXT,
    PRIMARY KEY (project_id, slug),
    UNIQUE (project_id, position)
);

CREATE TABLE project_types (
    project_id  INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    position    INTEGER NOT NULL,
    slug        TEXT NOT NULL,
    description TEXT,
    PRIMARY KEY (project_id, slug),
    UNIQUE (project_id, position)
);

CREATE TABLE project_members (
    project_id   INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    member_id    TEXT NOT NULL,
    display_name TEXT NOT NULL,
    email        TEXT,
    github       TEXT,
    created_at   TEXT NOT NULL,
    PRIMARY KEY (project_id, member_id)
);

-- `github_sync` is the schema's one deliberate JSON column: a nested, optional
-- document owned by the feature-gated `src/github/` module. Modelling it as
-- columns would make the store depend on the `github-sync` cargo feature.
CREATE TABLE project_settings (
    project_id             INTEGER PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    sync_auto_transition   INTEGER CHECK (sync_auto_transition IN (0, 1)),
    doctor_stale_threshold TEXT,
    github_sync            TEXT
);

-- The last-synced snapshot github-sync three-way-merges against.
CREATE TABLE github_bases (
    project_id INTEGER NOT NULL,
    story_no   INTEGER NOT NULL,
    snapshot   TEXT NOT NULL,
    PRIMARY KEY (project_id, story_no),
    FOREIGN KEY (project_id, story_no)
        REFERENCES stories(project_id, story_no) ON DELETE CASCADE
);
