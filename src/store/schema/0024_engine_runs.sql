-- storyhook store — schema version 24: Full Auto engine runs and lanes
-- become durable operational state (SH-462).
--
-- # Operational state, not another event fold
--
-- These rows are deliberately outside the append-only story event log and
-- `story doctor`'s `diff_rebuilt` comparison. Heartbeats and lane observations
-- are high-frequency operational facts; what an engine run does to a story is
-- already event-sourced on that story. Replaying story history therefore must
-- neither rebuild nor compare either table.
--
-- # The vocabulary fixture rule
--
-- Every finite vocabulary written here is guarded by a schema CHECK. That is
-- why no fixture has to hand-spell a second authoritative list of run states,
-- lane states, scopes, or agents: an invalid spelling is structurally refused.
-- Tests provoke each CHECK rather than merely looking for its text (SH-364).

CREATE TABLE engine_runs (
  id                     TEXT PRIMARY KEY,
  project_slug           TEXT NOT NULL,
  scope_kind             TEXT NOT NULL CHECK (scope_kind IN ('project','epic')),
  scope_story_id         TEXT,
  lanes                  INTEGER NOT NULL CHECK (lanes >= 1),
  agent                  TEXT NOT NULL CHECK (agent IN ('claude','codex')),
  state                  TEXT NOT NULL CHECK (state IN ('running','paused','draining','halted','finished')),
  consecutive_hard_stops INTEGER NOT NULL DEFAULT 0,
  stop_reason            TEXT,
  acknowledged_at        TEXT,
  created_at             TEXT NOT NULL,
  updated_at             TEXT NOT NULL,
  CHECK ((scope_kind = 'epic') = (scope_story_id IS NOT NULL))
);

-- The index is the arbiter for racing starts. A read-before-write check could
-- only make the common refusal friendlier; it could never make it safe.
CREATE UNIQUE INDEX engine_runs_one_live_per_project
  ON engine_runs (project_slug)
  WHERE state IN ('running','paused','draining');

CREATE TABLE engine_lanes (
  run_id            TEXT NOT NULL REFERENCES engine_runs(id) ON DELETE CASCADE,
  lane_index        INTEGER NOT NULL,
  state             TEXT NOT NULL CHECK (state IN ('idle','dispatching','working','quarantined')),
  story_id          TEXT,
  window_name       TEXT,
  worktree_path     TEXT,
  dispatched_at     TEXT,
  last_observed_at  TEXT NOT NULL,
  outcome           TEXT,
  outcome_detail    TEXT,
  PRIMARY KEY (run_id, lane_index),
  CHECK ((state = 'idle') = (story_id IS NULL))
);
