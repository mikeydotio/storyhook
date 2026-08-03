-- storyhook store — schema version 6: a project is known by its origins.
--
-- This is the identity half of the move to explicit, server-owned project
-- selection (SH-115, a child of SH-112). Until now "which project is this?" was
-- answered by matching the working directory against `project_paths`. A
-- recorded path rots, and it had already rotted badly: when this migration was
-- written, seven of the thirteen real projects in the author's store were
-- reported as having "no checkout on this machine" while every one of them had
-- a checkout on that machine at that moment. Two of the seven had simply been
-- renamed on disk — `openGrid-SCAD` for `opengrid-scad`, `Ourdio` for `ourdio`.
--
-- A repository's origin URL does not rot that way, so a project registers its
-- origins here and a checkout resolves by normalizing its origin and looking the
-- result up.
--
-- **Corrected by SH-116, which built the resolution half.** This header
-- originally said the origin comes from `git remote get-url origin`. It does
-- not: it comes from `git config --get remote.origin.url`, and the difference is
-- load-bearing rather than stylistic. `get-url` applies the caller's
-- `url.<base>.insteadOf` rewrites, which are machine-local — the author's own
-- machine carries a global one, so the two commands already disagree there with
-- no `-c` flag. An identity that moves with a rewrite is not an identity: two
-- clones of one repository would key differently on two machines whose users
-- push over different protocols, and the failure is silent, because the checkout
-- simply stops resolving and the refusal tells the user to register an origin
-- they already registered. See `service::project::origin_of`.
--
-- `src/github/sync_state.rs` still uses `get-url`, deliberately and correctly:
-- it is finding a GitHub API endpoint to talk to, where the rewritten url is the
-- right answer. Different purpose, different correct command.
--
-- ---------------------------------------------------------------------------
-- The shape, and which half of it is load-bearing
-- ---------------------------------------------------------------------------
--
-- A project holds MANY origins — a repository that moved, a second canonical
-- remote — and an origin belongs to AT MOST ONE project. That is the same
-- plurality-with-a-uniqueness-guard shape `project_paths` already has, and the
-- two constraints do different jobs:
--
--   * `PRIMARY KEY (project_id, normalized)` is the re-registration target. A
--     project registering an origin it already holds updates the row rather
--     than failing, which is what makes `link origin` safely re-runnable.
--   * `idx_project_remotes_normalized` is the **cross-project guard**, and it
--     is the load-bearing half. It makes "two projects claim the same origin"
--     unrepresentable, so resolution can never be ambiguous — exactly what
--     `idx_project_paths_path` does for checkout directories, on a better key.
--
-- `link_remote` looks the current holder up *before* inserting, so that a
-- refusal can name the project that already holds the origin; SQLite's own
-- constraint error names a column and cannot name a project. That lookup is
-- safe from a race because a write transaction runs under `BEGIN IMMEDIATE`,
-- which is exclusive among writers — the same reasoning `create_project` uses
-- for its uuid and slug pre-checks. The index remains the backstop for anything
-- that reaches this table without going through that method.
--
-- ---------------------------------------------------------------------------
-- Why the raw URL is stored beside the normalized one
-- ---------------------------------------------------------------------------
--
-- `normalized` is an identity key produced by `domain::remote::RemoteUrl` — it
-- is lowercased, has its scheme and any userinfo removed, and has one `.git`
-- suffix stripped. It is lossy on purpose. `raw` is what the user actually
-- typed, kept so the normalizer can improve later without any registration
-- losing the information it was made from, and so a UI can show somebody their
-- own URL rather than a folded one. On the author's machine seven of
-- forty-six real remotes carry a mixed-case owner or repository name, so this
-- is a live concern rather than a future-proofing gesture.
--
-- ---------------------------------------------------------------------------
-- What this migration does not touch
-- ---------------------------------------------------------------------------
--
-- Nothing. It creates one table and one index and alters nothing that exists.
-- In particular it does not name or rebuild `stories`, so the warning migration
-- 5 left for its successors — that a migration rebuilding `stories` must drop
-- `events_reject_delete` first and recreate it afterwards — does not apply
-- here. `foreign_keys_off` stays `false`: that flag exists only for a migration
-- that rebuilds a table other tables reference, and this rebuilds nothing.
--
-- `project_paths` is left in place and still resolves. Removing it, and the
-- directory-walking resolution built on it, is SH-119's job; doing both in one
-- migration would mean a store that could no longer answer the old way before
-- anything could answer the new way.

CREATE TABLE project_remotes (
    project_id    INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    normalized    TEXT NOT NULL,
    raw           TEXT NOT NULL,
    registered_at TEXT NOT NULL,
    PRIMARY KEY (project_id, normalized),
    CHECK (length(normalized) > 0),
    CHECK (length(raw) > 0)
);

CREATE UNIQUE INDEX idx_project_remotes_normalized ON project_remotes(normalized);
