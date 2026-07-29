-- storyhook store — schema version 2: git link records become a constraint.
--
-- `story commit-sync` records each commit that names a story. Whether it has
-- already recorded a given one used to be decided by scanning every event on
-- the story for a `StoryCommentAdded` whose text began `[git] <short-hash>:` —
-- O(events) per commit per story, and satisfiable by a user typing a comment
-- that happens to start the same way, which would silently suppress a real
-- link.
--
-- Here it is a primary key. A second link record for the same commit on the
-- same story is not a state the database can hold, whatever any caller does.
--
-- Rows are a projection of the `StoryCommitLinked` events (kind #18), written
-- in the same transaction as the event itself by `append_raw_events`. Both are
-- append-only, so the two cannot drift.

CREATE TABLE story_commit_links (
    project_id INTEGER NOT NULL,
    story_no   INTEGER NOT NULL,
    -- The full 40-character hash for records this binary wrote. Rows backfilled
    -- below hold the *seven-character* abbreviation instead, because that is
    -- all the comment they were recovered from preserved — see
    -- `ReadOps::commit_linked`, which is why the lookup asks for both
    -- spellings.
    sha        TEXT NOT NULL,
    PRIMARY KEY (project_id, story_no, sha),
    CHECK (story_no >= 1),
    CHECK (length(sha) > 0)
);

-- Deliberately **no foreign key to `stories`**, unlike `story_labels` and
-- `story_relations`. Those are projections of a *folded snapshot*, written
-- after the story's row exists. This is a projection of an *event*, and events
-- are legitimately written before a read-model row: `story import-project` and
-- `story migrate` both append a whole history and fold it afterwards. A foreign
-- key here would reject a restore.

-- Backfill: every link this project already recorded as a comment.
--
-- Without this, the first `commit-sync` after an upgrade re-links every commit
-- inside its window — the events say the story is linked, the table does not,
-- and the table is now the authority. The text is `[git] <hash>: <subject>`, so
-- the hash runs from character 7 to the first colon.
--
-- `ON CONFLICT DO NOTHING` rather than `INSERT OR IGNORE`: the schema's
-- portability rules (see 0001) rule out SQLite's `OR` clauses, and one story
-- can carry two comments naming one commit if a human wrote the second by hand.
INSERT INTO story_commit_links (project_id, story_no, sha)
SELECT project_id, story_no, sha
FROM (
    SELECT project_id, story_no, substr(text, 7, instr(text, ':') - 7) AS sha
    FROM (
        SELECT project_id, story_no, json_extract(payload, '$.text') AS text
        FROM events
        WHERE kind = 'StoryCommentAdded'
    )
    WHERE text LIKE '[git] %:%'
      AND instr(text, ':') > 7
)
-- Hexadecimal, or it is not a hash. `[git] rebase: I squashed this branch` is a
-- comment a human wrote, and recording it as a link would suppress a real
-- commit — one whose abbreviation is `rebase`, which no commit can have, so the
-- damage would be invisible rather than absent. SQLite has no regex; `GLOB`
-- has character classes, and "contains no non-hex character" is the negation
-- that expresses this in one.
WHERE sha NOT GLOB '*[^0-9a-fA-F]*'
ON CONFLICT DO NOTHING;
