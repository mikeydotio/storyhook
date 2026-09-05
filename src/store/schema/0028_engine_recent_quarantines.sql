-- storyhook store — schema version 28: retain the current Full Auto
-- breaker's bounded hard-stop series after lanes return to service (SH-542).

ALTER TABLE engine_runs ADD COLUMN recent_quarantines_json TEXT NOT NULL DEFAULT '[]'
  CHECK (
    json_valid(recent_quarantines_json)
    AND json_type(recent_quarantines_json) = 'array'
    AND json_array_length(recent_quarantines_json) <= 3
  );
