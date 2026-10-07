-- A stop/start cannot renew an older diagnosis's authority.
ALTER TABLE verification_control ADD COLUMN revision INTEGER NOT NULL DEFAULT 0
    CHECK (typeof(revision) = 'integer' AND revision >= 0);
