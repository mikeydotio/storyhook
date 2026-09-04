-- storyhook store — schema version 27: Full Auto lanes retain the immutable
-- resource identity returned by dispatch (SH-539).
--
-- Existing rows stay NULL. Absence is not inferred: an upgraded daemon must
-- report cleanup required instead of rebuilding ownership from mutable project
-- checkout or provider configuration.

ALTER TABLE engine_lanes ADD COLUMN cleanup_lease_json TEXT;
