-- Defaults preserve every existing project. A watermark invalidates stale submissions.
ALTER TABLE project_settings ADD COLUMN automations_enabled INTEGER CHECK(automations_enabled IN (0,1));
ALTER TABLE project_settings ADD COLUMN automations_after INTEGER CHECK(automations_after >= 0);
