-- storyhook store — schema version 8: the resolution index is deleted.
--
-- `project_paths` answered "which project is this directory?". It was the
-- mechanism the whole server-owned epic (SH-112) exists to remove, and this is
-- the migration that removes it. What answers that question now is, in order:
-- the `--project` selector, the committed `.storyhook.toml` at or above the
-- working directory, and the repository's registered git origin. None of them
-- is a fact about one machine's filesystem, which is the point.
--
-- ---------------------------------------------------------------------------
-- What happens to the rows
-- ---------------------------------------------------------------------------
--
-- A project's **main** checkout becomes `projects.checkout_path` — the column
-- migration 7 added for exactly this, and which until now was written
-- redundantly beside the index. Worktree rows are dropped without replacement,
-- and that is deliberate rather than lossy: `checkout_path` answers "where does
-- this project's repo-side work run", and a linked worktree is a branch
-- somebody is working on. It was never the right answer, and choosing it when
-- it sorted first was a defect (`preferred_checkout`, deleted with this).
--
-- The `UPDATE` only fills a column that is still NULL, so a project already
-- pointed somewhere by `story project link checkout` keeps what a user chose
-- over what the index remembered. `MIN(path)` breaks a tie between two rows
-- both marked `main`, which the unique index made impossible for one project
-- but which a hand-edited database could hold.
--
-- ---------------------------------------------------------------------------
-- What this does not touch
-- ---------------------------------------------------------------------------
--
-- No table is rebuilt. `project_paths` is referenced by no trigger and no other
-- table's foreign key — it is a leaf — so `DROP TABLE` needs no guard, and
-- migration 5's warning to its successors (a migration that rebuilds `stories`
-- must drop `events_reject_delete` first and recreate it afterwards) does not
-- apply here. `foreign_keys_off` stays `false`.
--
-- Dropping the table takes `idx_project_paths_path` with it; the explicit
-- `DROP INDEX` above it is not required by SQLite and is written anyway,
-- because that index is named in `store/error.rs`'s constraint-message mapping
-- and a reader deleting one should see the other go in the same place.

UPDATE projects
   SET checkout_path = (
       SELECT MIN(pp.path)
         FROM project_paths pp
        WHERE pp.project_id = projects.id
          AND pp.kind = 'main'
   )
 WHERE checkout_path IS NULL
   AND EXISTS (
       SELECT 1
         FROM project_paths pp
        WHERE pp.project_id = projects.id
          AND pp.kind = 'main'
   );

DROP INDEX IF EXISTS idx_project_paths_path;
DROP TABLE project_paths;
