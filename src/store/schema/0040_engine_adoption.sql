-- NULL preserves the lifecycle of all existing engine-dispatched lanes.
ALTER TABLE engine_lanes ADD COLUMN adopted_identity_json TEXT
    CHECK (adopted_identity_json IS NULL OR (json_valid(adopted_identity_json)
        AND state IN ('working', 'quarantined')
        AND story_id IS NOT NULL AND pane_id IS NOT NULL
        AND cleanup_lease_json IS NOT NULL AND worktree_path IS NOT NULL));
