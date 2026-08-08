-- storyhook store — schema version 11: `story_pr_links`, the SH-49 linked-PR
-- projection.
--
-- Rows are a projection of `StoryPrLinked`/`StoryPrUnlinked`/`StoryPrMerged`/
-- `StoryPrClosed` events, written in the same transaction as the event itself
-- by `write::project_pr_link` — see `story_commit_links` (0002) for the
-- precedent this follows.
--
-- Unlike `story_commit_links`, which is a pure insert rejected on duplicate,
-- this table needs three different write shapes for its four event kinds:
-- `StoryPrLinked` upserts (a user may re-link the same PR to toggle
-- `close_on_merge`), `StoryPrUnlinked` deletes by `url`, and
-- `StoryPrMerged`/`StoryPrClosed` update `status` and `last_checked_at` on the
-- existing row.

CREATE TABLE story_pr_links (
    project_id      INTEGER NOT NULL,
    story_no        INTEGER NOT NULL,
    owner           TEXT NOT NULL,
    repo            TEXT NOT NULL,
    number          INTEGER NOT NULL,
    url             TEXT NOT NULL,
    close_on_merge  INTEGER NOT NULL,
    status          TEXT NOT NULL,
    linked_at       TEXT NOT NULL,
    last_checked_at TEXT,
    PRIMARY KEY (project_id, story_no, owner, repo, number),
    CHECK (story_no >= 1),
    CHECK (length(owner) > 0),
    CHECK (length(repo) > 0),
    CHECK (number >= 1),
    CHECK (close_on_merge IN (0, 1)),
    CHECK (status IN ('open', 'merged', 'closed'))
);

-- Deliberately **no foreign key to `stories`**, for the same reason
-- `story_commit_links` has none (see 0002): this is a projection of an
-- *event*, and events are legitimately written before the read-model row
-- exists — `story import-project` and `story migrate` both append a whole
-- history and fold it afterward. A foreign key here would reject a restore.
