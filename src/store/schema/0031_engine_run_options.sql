-- SH-566: one Full Auto run carries one immutable provider configuration.
-- Existing runs predate selection and therefore receive NULL, never an
-- invented provider default. Model and effort are open provider vocabularies;
-- speed is Storyhook's closed vocabulary and is constrained here.

ALTER TABLE engine_runs ADD COLUMN model TEXT;
ALTER TABLE engine_runs ADD COLUMN effort TEXT;
ALTER TABLE engine_runs ADD COLUMN speed TEXT
  CHECK (speed IN ('standard', 'fast'));
