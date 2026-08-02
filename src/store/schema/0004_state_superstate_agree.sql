-- storyhook store — schema version 4: a story's superstate must agree with the
-- superstate of the state it names.
--
-- Schema 1 stored the two independently:
--
--     stories.state       TEXT NOT NULL
--     stories.superstate  TEXT NOT NULL CHECK (superstate IN ('OPEN','CLOSED'))
--
-- with the vocabulary and *its* superstate in a separate table. Nothing tied
-- them together, so a CLOSED story could sit in an OPEN state. That is not a
-- hypothetical: SH-20 in this project's own tracker read
-- `state: todo (CLOSED, deleted)`, and `story list --state todo` returned a
-- closed, soft-deleted story among the live ones.
--
-- ---------------------------------------------------------------------------
-- The legal tuples
-- ---------------------------------------------------------------------------
--
-- Over (the slug's superstate, `stories.superstate`, `closed`, `deleted`),
-- where `closed` is `archived` — already tied to `closed_at IS NOT NULL` by a
-- CHECK from schema 1, so they are one variable, not two. Sixteen
-- combinations; reachability judged through the public API, since the
-- fault-injection back door can fabricate anything by design.
--
--   #   slug  story   closed deleted  legal  reachable before this migration
--   1   OPEN   OPEN     0      0      YES    yes — an ordinary open story
--   2   OPEN   OPEN     1      0      no     no — the fold clears closed_at
--                                            when a story moves into an OPEN
--                                            slug
--   3   OPEN   OPEN     0      1      no     no — deleted forced superstate
--   4   OPEN   OPEN     1      1      no     no — same
--   5   OPEN  CLOSED    0      0      no     no
--   6   OPEN  CLOSED    1      0      no     no
--   7   OPEN  CLOSED    0      1      no     no — StoryDeleted always stamps
--                                            closed_at
--   8   OPEN  CLOSED    1      1      NO     *** THE DEFECT — see below ***
--   9  CLOSED  OPEN     0      0      no     no — superstate was derived from
--                                            the slug whenever not deleted
--  10  CLOSED  OPEN     1      0      no     no
--  11  CLOSED  OPEN     0      1      no     no
--  12  CLOSED  OPEN     1      1      no     no
--  13  CLOSED CLOSED    0      0      no     no — a CLOSED transition always
--                                            carries StoryClosedAndArchived
--  14  CLOSED CLOSED    1      0      YES    yes — an ordinary closed story
--  15  CLOSED CLOSED    0      1      no     no
--  16  CLOSED CLOSED    1      1      YES    where row 8 now lands
--
-- Three tuples are legal: 1, 14 and 16. One illegal tuple was reachable — row
-- 8 — by **two** routes, and the second was not in the story as filed:
--
--   * `story delete`. It stamped the superstate CLOSED and left the slug
--     alone. Every soft-deleted story in every project carried row 8. In the
--     live store that was 6 rows of 835, all of them sitting in `todo`.
--
--   * Reclassifying a CLOSED state to OPEN while it holds only *archived*
--     stories. `state_usage` counts undeleted stories and splits them into
--     open and archived; `resolve_migration` reads only the open count and
--     returns early when it is zero. So a state whose occupants are all
--     archived looks empty, the flip is allowed with no occupant migration,
--     and every story in it keeps a slug whose superstate just changed
--     underneath it.
--
-- The second route is why this is a foreign key rather than a trigger. That
-- write lands on `project_states`, not on `stories`, so a trigger on `stories`
-- cannot see it — and a `BEFORE DELETE` trigger on `project_states` is not
-- available either, because `put_states` deletes and re-inserts a project's
-- entire catalog on every edit, so such a trigger fires on an ordinary
-- `story state add`.
--
-- ---------------------------------------------------------------------------
-- Why the constraint can be expressed at all
-- ---------------------------------------------------------------------------
--
-- Because the fold no longer half-derives the pair. `fold_story` used to force
-- `superstate = CLOSED` for a deleted story and leave `state` untouched;
-- it now moves the story to the required CLOSED state as well, so
-- `stories.superstate` became a pure function of the slug and the catalog —
-- which is precisely, and only, what a composite foreign key can express.
--
-- That rule lives in the fold rather than in `story delete` on purpose: it has
-- to hold for event logs written years ago, which no service can revisit. A
-- `StoryDeleted` from before this migration folds to the resting state now, so
-- the rebuild oracle, `story doctor` and `story import` of an old export
-- document all agree with the schema without any event being invented.
--
-- ---------------------------------------------------------------------------
-- DEFERRABLE INITIALLY DEFERRED, and no ON DELETE clause
-- ---------------------------------------------------------------------------
--
-- Both are load-bearing.
--
-- **Deferred**, because `put_states` clears a project's whole catalog and
-- rewrites it inside one transaction. Checked immediately, every story in the
-- project would violate the constraint in the instant between the DELETE and
-- the re-INSERT. Write transactions already set `PRAGMA defer_foreign_keys`,
-- but declaring the constraint deferrable makes that a property of the
-- constraint rather than a habit of the caller.
--
-- **No `ON DELETE CASCADE`**, and this is the dangerous one to get wrong. The
-- parent rows are deleted on every catalog edit; a cascade would take every
-- story in the project with them. The default NO ACTION is what makes the
-- constraint mean "at COMMIT, every story names a state that exists with the
-- superstate it claims" — which refuses the reclassification in route 2 while
-- allowing the delete-and-reinsert in normal use.
--
-- The parent needs a UNIQUE index on the exact triple: `project_states`' key is
-- `(project_id, slug)`, and SQLite answers `foreign key mismatch` for a
-- composite reference without one.
--
-- ---------------------------------------------------------------------------
-- The archived CHECK
-- ---------------------------------------------------------------------------
--
-- `(superstate = 'CLOSED') = archived` closes rows 2 and 13 — the same defect
-- class, two columns that must agree, and free to take while the table is
-- being rebuilt anyway. No row in the live store violates it (verified across
-- all 518 projects), and the fold cannot produce one, since only
-- `StoryClosedAndArchived` sets `closed_at` and it is emitted only for a CLOSED
-- target. A store that somehow holds a violation will fail this migration
-- loudly rather than be silently "repaired" by inventing a close timestamp.
--
-- ---------------------------------------------------------------------------
-- This migration runs with foreign keys OFF
-- ---------------------------------------------------------------------------
--
-- Declared by `foreign_keys_off` on the `Migration`, and not optional: with
-- enforcement on, the `DROP TABLE stories` below fires the ON DELETE CASCADE of
-- every child that references it and silently empties `story_labels`,
-- `story_relations` and `github_bases`, then COMMITs successfully. Measured on
-- the bundled SQLite 3.51.0. `PRAGMA foreign_key_check` runs inside this
-- migration's transaction to buy that safety back.

-- ---------------------------------------------------------------------------
-- 1. Repair, before anything is copied
-- ---------------------------------------------------------------------------

-- Every row carrying tuple 8 moves to the state its own events now fold to.
--
-- The destination must match `domain::resting_state_for_deleted` exactly, or a
-- repaired row would disagree with its own rebuild and `story doctor` would
-- report a divergence this migration had just created. That rule is: the
-- required CLOSED state `done` when the project has it, otherwise the
-- alphabetically first CLOSED state — alphabetical because the fold is handed a
-- slug-keyed BTreeMap and has no access to configured order.
--
-- `snapshot` is patched in the same statement. It holds the folded
-- `StorySnapshot` verbatim and is what `story show` reads; a repair that moved
-- the column and left the document behind would fix the query surface and leave
-- the displayed story wrong.
UPDATE stories
SET state = (
        SELECT ps.slug FROM project_states ps
        WHERE ps.project_id = stories.project_id AND ps.superstate = 'CLOSED'
        ORDER BY (ps.slug <> 'done'), ps.slug
        LIMIT 1
    ),
    snapshot = json_set(
        snapshot,
        '$.state',
        (
            SELECT ps.slug FROM project_states ps
            WHERE ps.project_id = stories.project_id AND ps.superstate = 'CLOSED'
            ORDER BY (ps.slug <> 'done'), ps.slug
            LIMIT 1
        )
    )
WHERE superstate = 'CLOSED'
  AND EXISTS (
        SELECT 1 FROM project_states ps
        WHERE ps.project_id = stories.project_id
          AND ps.slug = stories.state
          AND ps.superstate = 'OPEN'
    )
  AND EXISTS (
        SELECT 1 FROM project_states ps
        WHERE ps.project_id = stories.project_id AND ps.superstate = 'CLOSED'
    );

-- ---------------------------------------------------------------------------
-- 2. The parent's unique index
-- ---------------------------------------------------------------------------

CREATE UNIQUE INDEX idx_project_states_slug_superstate
    ON project_states(project_id, slug, superstate);

-- ---------------------------------------------------------------------------
-- 3. The rebuild
-- ---------------------------------------------------------------------------

CREATE TABLE stories_new (
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
    -- SH-130: superstate and archived are the same fact told twice.
    CHECK ((superstate = 'CLOSED') = archived),
    CHECK (priority IN ('critical', 'high', 'medium', 'low', 'none')),
    CHECK (priority_rank = CASE priority
        WHEN 'critical' THEN 0
        WHEN 'high'     THEN 1
        WHEN 'medium'   THEN 2
        WHEN 'low'      THEN 3
        WHEN 'none'     THEN 4
    END),
    -- SH-130: the whole point of this migration.
    FOREIGN KEY (project_id, state, superstate)
        REFERENCES project_states(project_id, slug, superstate)
        DEFERRABLE INITIALLY DEFERRED
);

INSERT INTO stories_new SELECT
    project_id, story_no, head_seq, title, state, superstate, priority,
    priority_rank, story_type, assignee, awaiting, deleted, archived,
    created_at, updated_at, closed_at, description, snapshot
FROM stories;

DROP TABLE stories;
ALTER TABLE stories_new RENAME TO stories;

-- Recreated verbatim from schema 1: a rebuild that dropped an index would turn
-- every list and every board render into a scan, silently.
CREATE INDEX idx_stories_state    ON stories(project_id, superstate, state);
CREATE INDEX idx_stories_priority ON stories(project_id, priority_rank, story_no);
CREATE INDEX idx_stories_assignee ON stories(project_id, assignee);
CREATE INDEX idx_stories_updated  ON stories(project_id, updated_at);
