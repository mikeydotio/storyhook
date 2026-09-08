-- SH-594: per-project policy for daemon-owned workspace cleanup.
-- NULL preserves the code defaults (`true` and `1d`) for every existing
-- project, so migration changes no effective behaviour by inventing stored
-- user choices.
ALTER TABLE project_settings ADD COLUMN cleanup_auto INTEGER
    CHECK (cleanup_auto IN (0, 1));
ALTER TABLE project_settings ADD COLUMN cleanup_interval TEXT;
