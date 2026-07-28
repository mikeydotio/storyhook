# Handoff — end of wave W2a (data-layer rearchitecture)

**Read first:** [`docs/rearch/STATE.md`](docs/rearch/STATE.md) (execution ledger — the W2a step
log carries the service API and every deviation) and
[`docs/spec/data-layer-rearchitecture.md`](docs/spec/data-layer-rearchitecture.md) (the design).
This file is the wave-boundary summary; the ledger is the detail.

## State

- **Wave W2a complete** — `feat(service): story lifecycle and relation services over Store`.
- **Branch:** `rearch/w2a-lifecycle`, based on merged `main@271c7cf` (which contains W0/W0b/W1).
- **PR:** opened against `main`. Work happens in a linked worktree, so this wave stops at
  "PR opened" by design; the orchestrator merges it.
- **W1 merged** as [PR #62](https://github.com/mikeydotio/storyhook/pull/62).

## Exit-criteria evidence

- **`make test` green twice consecutively at the tip**, plus once after every commit.
- **1422 → 1575 tests** (+153): 86 `service_story`, 24 `service_relations`,
  39 `differential_lifecycle`, 4 `domain` fold units. 2 ignored, unchanged — still the W4
  headline REDs in `worktree_truth.rs`.
- **`src/app.rs` is untouched.** `git diff 271c7cf..HEAD -- src/app.rs` is empty.
- **Zero assertion or snapshot edits.** `git diff --stat 271c7cf..HEAD -- tests/` reports only
  the three new files; the golden corpus is byte-unchanged under `INSTA_UPDATE=no`.
- **The differential harness works** — it caught a real legacy defect on its first full run
  (below), and deleting the `progress` rollup from the view builder fails five of its tests.
- **The drift guard bites** — two `should_panic` tests damage a read model through a second
  connection to prove it is not vacuously green.
- **Timing: 2:07 and 2:04 wall**, against W1's ~56s. The jump is **build, not tests**: the three
  new binaries contribute 2.0s of runtime; the rest is linking them plus recompiling every
  downstream binary because `src/domain.rs` changed. `cargo test --workspace` alone is ~62s warm.

## What landed

Four commits, each green under `make test`:

| SHA | What |
|---|---|
| `845afb9` | `fold_story` retracts `closed_at`/`deleted`/`deleted_reason` on a move into an OPEN state |
| `c67642b` | `Ctx`/`Clock`, `invoke::dispatch`, `service::view`, `StoryService`, `ServiceFixture` |
| `fa4d610` | `RelationService` — both ends' events, folds and rows in one transaction |
| `757bc7b` | the differential harness, and the legacy defect it found |

Nothing is wired into production: `dispatch` is reachable only from tests, every unported
`Invocation` answers with a loud internal error, and the legacy path still serves every user.

## Deviations (all recorded in the ledger)

1. **The dispatch skeleton is not its own commit** — its helpers exist only for the services,
   so a skeleton-only commit fails `clippy -D warnings` on dead code.
2. **BulkUpdate stays best-effort per item**, against the brief's "one failure → whole batch
   rolled back". The legacy output has a per-item `error —` line; making the batch atomic
   changes user-visible behaviour, and byte-compat is the higher rule. Each item *is* now
   atomic on its own, which is the property the old three-filesystem-op sequence lacked.
3. **`fold_story` changed** — outside `src/service/`, but reopen is not expressible
   append-only without it. Own commit, own tests, zero assertion edits.
4. **`StoreError` gained `Rejected(Box<AppError>)`** — the service→store error channel, without
   which `Usage` and `StateConflict` cannot survive a rollback intact.
5. **A `Clock` lives on `Ctx`** three waves before W5's `Environment` absorbs it.

## Defect found, unfixed, needs a story

**A rejected `--state` burns a story number.** `storage::create_story_with_events` increments
the on-disk counter *before* validating the requested state, so `story new --state nonsense`
exits 2 and still consumes the number, leaving a permanent gap. Only `--state` shows it; type,
priority and assignee are validated earlier, in `app.rs`. The store does not have the defect
(the allocation is inside the transaction that uses it), so this is a deliberate behaviour
change at the flip rather than a regression — **W4 should list it in the flip's
behaviour-change notes.** Pinned by
`differential_lifecycle.rs::a_rejected_initial_state_burns_a_story_number_in_the_legacy_leg_only`.

## Next entry criteria (W2b / W2c / W2d)

1. Confirm the tip, read the ledger's **W2a step log** — it carries the full service API and the
   traps below — then `make test` green before touching anything.
2. **Use `tx.states()`, not `tx.state_map()`, wherever order is part of the contract.** The map
   is a `BTreeMap`; iterating it is alphabetical, and the default open state taken off it is
   `in-progress` rather than `todo`.
3. **Call `append_and_fold` inside your own `store.write(|tx| …)`** so the one-transaction
   invariant stays visible at your call site.
4. **Fire hooks after the commit, never inside it** — a hook shells out to `story` and needs its
   own connection.
5. **W2c's QueryService should absorb `service::view`**, not duplicate it.
6. **Update `the_ported_arms_are_exactly_the_ones_this_wave_claims`** as you port arms; an arm
   ported without a roster update fails there. Dispatch completeness is W2d's exit gate.
7. W2c still owns the 70 white-box TUI call sites flagged in the W0.5 flip checklist; leaving
   them grows W4's budget by the checklist's largest single chunk.

## Worktree constraints (unchanged)

No version bumps, no deploys, no force-pushes, no touching `main` from here. Push over HTTPS
(`git -c url."https://github.com/".insteadOf="git@github.com:" push origin <branch>`). Story IDs
belong in commit **bodies**, never subjects. No story minting from this worktree — discovered
defects go to the ledger's Key facts until the flip removes the id-collision hazard.
