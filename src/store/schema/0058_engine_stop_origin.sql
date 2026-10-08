-- Stop Now preserves the original caller before any lane teardown is reserved.
ALTER TABLE engine_runs ADD COLUMN stop_origin_json TEXT CHECK(stop_origin_json IS NULL OR json_valid(stop_origin_json));
