# Handoff — end of wave W2c (data-layer rearchitecture)

**Read first:** [`docs/rearch/STATE.md`](docs/rearch/STATE.md) (execution ledger — the W2a, W2b
and W2c step logs together carry the whole service API, the ported-arm roster and every
deviation) and [`docs/spec/data-layer-rearchitecture.md`](docs/spec/data-layer-rearchitecture.md)
(the design). This file is the wave-boundary summary; the ledger is the detail.

## State

- **Wave W2c complete** — query and integrity services, and the TUI on the `Invoker` seam.
- **Branch:** `rearch/w2c-query`, based on merged `main@da446e9` (W0/W0b/W1/W2a/W2b).
- **PR:** opened against `main`. Work happens in a linked worktree, so this wave stops at
  "PR opened" by design; the orchestrator merges it.

## Exit-criteria evidence

- **`make test` green twice consecutively at the tip**, plus once after every commit.
- **1716 → 1775 tests** (+59): 19 `service_query`, 9 `service_integrity`, 28
  `differential_query`, 1 conformance case, 2 wire-envelope corpus rows. 2 ignored, unchanged —
  still the W4 headline REDs in `worktree_truth.rs`.
- **WHITE-BOX CALL SITES IN THE TUI AND ITS TESTS: 0.** `grep -rn
  'storyhook::storage\|storyhook::lock\|storyhook::registry\|ProjectPaths'
  tests/tui_integration.rs tests/tui_undo.rs` returns nothing. Workspace-wide the flip
  checklist's category C is **85 → 14**, and none of the 14 are the TUI's. `src/tui/` keeps
  exactly one white-box reference: `event.rs`'s notify watcher, retained by the wave brief and
  deleted in W5.
- **`src/app.rs` gains two additive arms and changes no existing one** (+42/-2). This is the
  wave's headline deviation; see below.
- **Zero snapshot edits.** `tests/snapshots/` is byte-unchanged under `INSTA_UPDATE=no`.
- **Timing: 88.8s and 88.4s warm**, against W2b's 90.3s/88.2s — no measurable cost. Load
  average was 5–7 throughout (the neighbouring Swift suite W2b noted), so both sides of that
  comparison are contended numbers.

## What landed

| SHA | What |
|---|---|
| `e9e170e` | `QueryService` over `&impl ReadOps` — `list` (every filter), `show`, `search`, `next`, `summary`, `report`, `graph`, `context`, `handoff`; `service::view` absorbed and deleted |
| `d3ee37c` | `fix(store)`: roll back a transaction whose COMMIT failed — a defect this wave's own tests exposed |
| `eb6aa63` | `IntegrityService` — every legacy doctor check, plus the rebuild-diff as a first-class dimension |
| `b937e8c` | `Invocation::{ProjectSnapshot, History}` and their `Response` variants |
| `8bf7eeb` | `src/tui/` onto the seam; 30 white-box tests reconstructed |

Nothing is wired into production: `dispatch` is reachable only from tests, every unported
`Invocation` answers with a loud internal error, and the TUI runs on `LegacyInvoker`.

## Ported-arm roster: 27 → 38

**Added:** `list`, `show`, `search`, `next`, `summary`, `report`, `graph`, `context`, `handoff`,
`doctor`, `project-snapshot`.

**Remaining 10**, all in `unported_probes()`: `export`, `session-start`, `history`, `import`,
`import-project`, `decompose`, `commit-sync`, `github-sync`, `update`, `web`.

`ported + probes == 48` (was 46 — the two new variants). `wire_envelope.rs` pins 48
independently, so an arm cannot be ported *or added* without landing on both lists.

## Two defects found

1. **`story doctor --fix` destroys relationships to archived stories.** The legacy repair asks
   "does the other end exist?" of the *open* stories only, so relating two stories and deleting
   one makes the survivor's edges look dangling; the repair retracts them and then reports the
   asymmetry it created. Exit 5, permanently, data gone. **Not reproduced** — the store leg asks
   the question of every story — and pinned by
   `differential_query.rs::doctor_fix_retracts_edges_to_deleted_stories_in_the_legacy_leg_only`.
   **Needs a story after the flip. W4 must list it in the behaviour-change notes.**
2. **A failed COMMIT poisoned a pooled connection** — fixed in-wave (`d3ee37c`) with a
   conformance-suite regression test. `SqliteWriteTx::commit` cleared its teardown flag before
   the COMMIT it describes. See the ledger for the generalization.

## Deviations (all recorded in the ledger)

1. **Two additive `src/app.rs` arms, not the one the brief sanctions.** `app::run`'s match is
   exhaustive over `Invocation`, so a variant cannot exist without an arm — and "zero white-box
   storage calls in the TUI" is unreachable while undo needs to read *and* rewrite a story's
   log. Grouping `Read`/`Restore` under one `History { action }` variant is what holds it to two.
2. **The differential rows landed with their features** rather than as a separate commit.
3. **`IntegrityService::fix` diverges from legacy** — defect 1 above.
4. **Two small TUI behaviour changes**: labels set from the detail editor arrive sorted, and two
   statuses-editor notifications now show the seam's message.

## Behaviour changes W4 must list in the flip notes

Carried forward from W2a/W2b, plus this wave's:

1. A rejected `--state` no longer burns a story number (W2a).
2. A superstate flip re-derives the rows of archived stories left in the state (W2b).
3. `story scaffold` outside a project (W2b) — root resolution decides.
4. **`doctor --fix` no longer retracts edges to archived stories** (W2c) — it stops losing data.
5. **Three doctor findings become unrepresentable** (W2c): dangling relations, second parents,
   and rows with no events are refused by the schema instead of diagnosed afterwards.

## Next entry criteria (W2d)

1. Confirm the tip, read the ledger's **W2a, W2b and W2c step logs**, then `make test` green
   before touching anything.
2. **`QueryService` takes a transaction, not a store**, and the generic `query()` helper in
   `invoke.rs` is what makes the lifetimes work. Copy it; do not try to move it onto `Ctx` —
   the higher-ranked bound it would need cannot be expressed.
3. **`service::view` no longer exists.** Its functions are `service::query::{story_map,
   story_views, story_view, sort_story_views}`.
4. **`Invocation::History::Restore` is the one seam operation with no store implementation.**
   The store is append-only with no truncate. Whoever designs it owns the TUI's undo after the
   flip; until then undo works on the legacy path only.
5. **Update the roster test as you port**, and keep `ported + probes == 48` true.
6. Ten arms remain. W2d owns `commit-sync`, `github-sync`; W3 owns `import`, `import-project`,
   `export`, `decompose`; W5 owns `web`, `session-start`, `history`; `update` needs no store.
7. If `web_test` mass-fails, check `uptime` and `ps aux | sort -nrk 3` before hunting a
   regression — that suite's readiness deadlines are wall-clock.

## Worktree constraints (unchanged)

No version bumps, no deploys, no force-pushes, no touching `main` from here. Push over HTTPS
(`git -c url."https://github.com/".insteadOf="git@github.com:" push origin <branch>`). Story IDs
belong in commit **bodies**, never subjects. No story minting from this worktree — discovered
defects go to the ledger's Key facts until the flip removes the id-collision hazard.
