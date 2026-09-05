-- storyhook store — schema version 27: retain the exact tmux pane identity
-- returned by Full Auto dispatch (SH-542).
--
-- A bare window name is not a stable tmux script target: tmux resolves an
-- unqualified target heuristically and may return success without selecting
-- the intended pane. Pane ids are unique and unchanged for the pane lifetime.

ALTER TABLE engine_lanes ADD COLUMN pane_id TEXT;
