-- storyhook store — schema version 17: `story_attachment_blobs`, SH-315's
-- attachment bytes.
--
-- # Why bytes get their own table rather than riding the event payload
--
-- `append_and_fold` re-reads and re-parses a story's *entire* event history
-- on every write to it (`events_for` -> `fold_story`, called from every
-- `append_and_fold` call site) — so a multi-megabyte image sitting inside one
-- event's JSON payload would tax every later comment, move, or relate on that
-- story, forever. Keeping the bytes in a row of their own, read only by
-- `story attachment save` and the doctor's integrity pass, is what keeps
-- every other write on the story cheap.
--
-- # Why bytes get their own table rather than a directory beside store.db
--
-- Nothing under `src/` writes user bytes to a directory beside the store —
-- `story help storage` names exactly what lives there (`store.db`,
-- `-wal`/`-shm`, and the daemon's own runtime state under the *state* home,
-- never the data home) — and a second location would need its own answer for
-- `Store::snapshot`/`VACUUM INTO`, the daily and maintenance backup
-- schedules, and `delete_project`/`purge_story`'s sweep, all four of which
-- this table gets for free by being an ordinary row in the one file
-- `story store backup` already verifies.
--
-- # Why this is a projection, not folded metadata
--
-- `StorySnapshot.attachments` (SH-315, `src/domain.rs`) already carries every
-- attachment's id, name, media type, byte length and sha256 — folded from
-- `StoryAttachmentAdded`/`StoryAttachmentRemoved` the ordinary way, so
-- `story doctor`'s `diff_rebuilt` already covers that half through the
-- `snapshot` column comparison it already runs. This table exists only to
-- hold what the snapshot cannot: the bytes themselves, which are too large to
-- embed in a folded document that is read on every `story show`.
--
-- Written directly by `AttachmentService::add`/`remove`
-- (`src/service/attachment.rs`) in the same transaction as the event that
-- names it, not by a `write::append`-loop projection keyed on `event.kind`
-- the way `story_pr_links` and `story_commit_links` are (`write::
-- project_pr_link`, `write::project_commit_link`): those read the field they
-- project straight out of the event's own JSON payload, and an attachment's
-- bytes are never in the payload to read.
--
-- `byte_len` and `sha256` are recorded here **as well as** in the folded
-- snapshot, deliberately redundant: `story doctor` compares the two
-- independently, so a row whose bytes were corrupted or truncated on disk is
-- caught even though the snapshot's own copy of those two fields is
-- untouched by that corruption.

CREATE TABLE story_attachment_blobs (
    project_id     INTEGER NOT NULL,
    story_no       INTEGER NOT NULL,
    attachment_id  INTEGER NOT NULL,
    bytes          BLOB NOT NULL,
    byte_len       INTEGER NOT NULL,
    sha256         TEXT NOT NULL,
    added_at       TEXT NOT NULL,
    PRIMARY KEY (project_id, story_no, attachment_id),
    CHECK (story_no >= 1),
    CHECK (attachment_id >= 1),
    CHECK (byte_len >= 1),
    CHECK (byte_len = length(bytes)),
    CHECK (length(sha256) = 64)
);

-- Deliberately **no foreign key to `stories`**, for the same reason
-- `story_pr_links` has none (see 0011): this is a projection of an *event*,
-- and events are legitimately written before the read-model row exists —
-- `story import-project` and `story migrate` both append a whole history and
-- fold it afterward. A foreign key here would reject a restore.
