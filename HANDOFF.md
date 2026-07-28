# Handoff — end of wave W2b (data-layer rearchitecture)

**Read first:** [`docs/rearch/STATE.md`](docs/rearch/STATE.md) (execution ledger — the W2b step
log carries the service API, the ported-arm roster and every deviation) and
[`docs/spec/data-layer-rearchitecture.md`](docs/spec/data-layer-rearchitecture.md) (the design).
This file is the wave-boundary summary; the ledger is the detail.

## State

- **Wave W2b complete** — `feat(service): project, config, and system services over Store`.
- **Branch:** `rearch/w2b-config`, based on merged `main@67b516a` (W0/W0b/W1/W2a).
- **PR:** opened against `main`. Work happens in a linked worktree, so this wave stops at
  "PR opened" by design; the orchestrator merges it.

## Exit-criteria evidence

- **`make test` green twice consecutively at the tip**, plus once after every commit.
- **1575 → 1716 tests** (+141): 19 `service_project`, 49 `service_config`, 18 `service_system`,
  16 `service_grouping`, 39 `differential_config`. 2 ignored, unchanged — still the W4 headline
  REDs in `worktree_truth.rs`.
- **`src/app.rs` is untouched.** `git diff 67b516a..HEAD -- src/app.rs` is empty.
- **Zero snapshot edits and zero behavioural assertion edits.** `tests/snapshots/` is
  byte-unchanged under `INSTA_UPDATE=no`; the only `assert` lines that moved in a pre-existing
  file are the harness block that relocated verbatim to `tests/differential_support/mod.rs`,
  plus this wave's own roster count.
- **Timing: 90.3s and 88.2s warm** against W2a's ~62.5s. **Most of that gap is machine
  contention, not this wave** — a neighbouring project held a core at 100% throughout (load
  average 6–9). The five new binaries contribute 1.5s of actual runtime; the rest is the
  ~1.2s-per-binary link cost every wave has measured. Re-measure idle at the next boundary.

## What landed

Five commits, each green under `make test`:

| SHA | What |
|---|---|
| `4056817` | `ProjectService` — project row, default states, default types and the id counter in one transaction; `service::templates`; the pointer file; `invoke::dispatch_unscoped` |
| `afa2bca` | `ConfigService` — state and type CRUD, occupant migration in the same transaction as the config change, member add |
| `b4e7598` | `SystemService` — scaffold, git hooks, event hooks, plugin; the five text-only arms |
| `1354798` | 31 differential rows for the new families; the shared harness extracted to `tests/differential_support/mod.rs` |
| `ddfe113` | `GroupingService` — phases and epics; `view::sort_story_views` |

Nothing is wired into production: `dispatch` is reachable only from tests, every unported
`Invocation` answers with a loud internal error, and the legacy path still serves every user.

## Ported-arm roster: 13 → 27

**Added:** `init`, `state`, `type`, `member-add`, `scaffold`, `hooks`, `plugin`, `phase`,
`epic`, `help`, `help-topic`, `help-compact`, `help-all`, `version`.

**Remaining 19.** W2c: `list`, `show`, `search`, `next`, `summary`, `report`, `graph`,
`context`, `handoff`, `doctor`. W2d and later: `import`, `import-project`, `export`,
`decompose`, `commit-sync`, `github-sync`, `update`, `web`, `session-start`.

`the_ported_arms_are_exactly_the_ones_this_wave_claims` now also asserts
`ported + probes == 46`, so an arm cannot be ported — or added to `Invocation` — without
landing on one of the two lists.

## Deviations (all recorded in the ledger)

1. **`init`'s Response text is byte-identical to legacy**, `.storyhook/` reference and all,
   rather than differentially normalized. Byte-compat is the governing rule and W4 owns the
   rewrite; identical text makes the differential row a strict equality.
2. **`Phase` and `Epic` were ported here**, though the spec files their list forms under W2c.
   `service::view::story_views` already existed, so the read halves cost one sort helper, and
   splitting an `Invocation` arm across waves would leave a roster that cannot describe itself.
3. **A `GroupingService` module** rather than methods on `StoryService` or `SystemService`.
4. **`state_transition_events` became `pub(crate)`** — a visibility change to W2a's file, so the
   migration path produces the identical batch instead of a second copy of it.
5. **The `uuid` crate was added** — a portable project identity cannot be derived from a path.
6. **The differential harness moved** to `tests/differential_support/mod.rs`, verbatim.

## Behaviour changes W4 must list in the flip notes

1. **A superstate flip re-derives the rows of the archived stories left in the state.** Legacy
   left their stored snapshots claiming the superstate they had when they closed. Reachable
   only with two CLOSED states and archived stories in the one being flipped. The alternative
   is a read model that no longer equals a fold of its own events.
2. **`story scaffold` outside a project.** The legacy arm falls back to prefix `SH` / state
   `done`; the store-backed arm takes a `Ctx`, which implies a project. W4's root resolution
   decides what happens.
3. **A rejected `--state` no longer burns a story number** (W2a's finding, unchanged).

## Next entry criteria (W2c / W2d)

1. Confirm the tip, read the ledger's **W2a and W2b step logs** — together they carry the whole
   service API — then `make test` green before touching anything.
2. **`QueryService` should absorb `service::view`** (`story_map`, `story_views`, `story_view`,
   `sort_story_views`), not grow a third copy. Ten of the nineteen remaining arms are queries.
3. **`Ctx` still has no clock accessor** — the builder occupies the name. A hook-suppressed or
   otherwise derived context is `Ctx::new(ctx.store(), ctx.project(), ctx.cwd())
   .clock(Clock::Fixed(ctx.now()))`.
4. **`ProjectService` is the one service without a `Ctx`**, because `init` has no project. W4's
   root resolution must call `invoke::dispatch_unscoped` *before* it tries to resolve one.
5. **Update the roster test as you port**, and keep `ported + probes == 46` true.
6. W2c still owns the 70 white-box TUI call sites flagged in the W0.5 flip checklist; leaving
   them grows W4's budget by the checklist's largest single chunk.
7. If `web_test` mass-fails, check `uptime` and `ps aux | sort -nrk 3` before hunting a
   regression — that suite's readiness deadlines are wall-clock, and a loaded machine fails it.

## Worktree constraints (unchanged)

No version bumps, no deploys, no force-pushes, no touching `main` from here. Push over HTTPS
(`git -c url."https://github.com/".insteadOf="git@github.com:" push origin <branch>`). Story IDs
belong in commit **bodies**, never subjects. No story minting from this worktree — discovered
defects go to the ledger's Key facts until the flip removes the id-collision hazard.
