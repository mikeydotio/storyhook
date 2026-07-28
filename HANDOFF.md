# Handoff — end of wave W0 (data-layer rearchitecture)

**Read first:** [`docs/rearch/STATE.md`](docs/rearch/STATE.md) (execution ledger) and
[`docs/spec/data-layer-rearchitecture.md`](docs/spec/data-layer-rearchitecture.md) (the design).
This file is the wave boundary summary; the ledger is the detail.

## State

- **Wave W0 complete** — `test: repair the quality gate and unify the integration harness`.
- **Branch:** `rearch/w0-gate-repair`, based on `main@838d68a`.
- **PR:** [#60](https://github.com/mikeydotio/storyhook/pull/60).
- **Merge state: MERGED 2026-07-28.** Work happened in a linked worktree, so the wave stopped
  at "PR opened" by design. W0b was built **stacked on this branch** rather than blocked on it,
  and merged into `main` behind it (`rearch/w0b-envelope`).

## Exit-criteria evidence

- **Flake census 10/10 green.** Ten consecutive `make test` runs; the verdict is per-*test*, not
  per-run — every one of the 1170 `ok` lines parsed from each run's log (1171 Rust tests, minus
  the 2 ignored, plus 1 doctest) was clean in all ten, and all ten reported identical counts
  (rust 1170/0/2, bash 16/0). No count drift hiding behind a green verdict.
- **Gate median 36.4s** over those ten runs (36.14–37.66, ±2%) on Psamathe (M1 Max, rustc
  1.97.1). Pre-W0 warm baseline was ~29s; W0 added ~7s, of which 5s is one unavoidable real
  `LockTimeout` deadline in `error_contract.rs` and 2.3s is the golden corpus.
- **Inventory: 1171 Rust tests / 52 binaries / 2 ignored**, 1 doctest, 16 bash files.
- **The two ignored tests are the program's headline REDs** — `worktree_truth.rs:118` and
  `:158`, both `#[ignore = "SH-46: …goes green at the W4 flip"]`. Their captured red evidence is
  in STATE.md. `scripts/capture-baseline.sh` asserts the ignored count against
  `docs/rearch/baseline/known-red.md`, so a new `#[ignore]` cannot enter quietly.
- **Baseline committed** to `docs/rearch/baseline/` (regenerate + diff at any wave boundary):
  test-name inventory, error-code table (all 10 `AppError` variants, extracted not transcribed),
  golden CLI corpus (177 invocations / 27 snapshots, `INSTA_UPDATE=no` in the gate), this repo's
  live `.storyhook/` tarball + its golden export (61 stories / 486 events / 44 archived),
  archive.db schema as version 0, timings.
- **The flake mechanism was proven, not guessed** — two orphaned `web_test-*` processes holding
  ~100 LISTEN sockets. Fixed at the source (kernel-assigned ports, real readiness handshake,
  bounded tailnet probe, orphan-check bracketing `make test`).

## Entry criteria for the next waves

**W1 is entry-ready** now that this PR has merged. W0b, the other parallel wave, is already
done — see below.

- **W0b is DONE** — branch `rearch/w0b-envelope`, stacked on this branch rather than waiting for
  the merge (`d272a7b` the export fix, `2db8310` the envelope, `ef717f2` the seam). It also
  shipped the `story export --json` double-encoding fix. See STATE.md's W0b step-log entry for
  the `Invoker`/`InvokeRequest` API and the `WireError` design rationale.
- **W1** starts from `docs/rearch/baseline/archive-schema.sql` (legacy = schema version 0) and
  must commit old-schema fixture DBs while the legacy code still exists to generate them.
- Resumption checklist (from the spec): confirm tip → read this file → **`make test` green
  before touching anything** → `story list` → `gh pr list` → reconfirm worktree constraints.
- If `web_test` mass-fails on resume, check for orphaned `web_test-*` listeners in 19xxx first.

## Deviations from the plan

None in scope or sequencing. Three recorded additions, all inside W0's mandate:

1. **A fifth `fix:` commit** (`e8d4cf8`) beyond the four planned — the readiness fix exposed a
   production defect: `tailnet_identity()` shelled out to `tailscale status --json` with an
   unbounded `Command::output()` *after* the loopback listener binds, so a wedged `tailscaled`
   left the server bound-and-silent forever. That is the orphan-maker, and it affects the real
   `story web start` daemon on :3456, not just tests.
2. **Test-support became a workspace crate** (`crates/storyhook-test-support/`) rather than the
   plan's earlier `tests/support/mod.rs` sketch — the plan's own architect ruling; the spec now
   states only the crate form.
3. **The W4 flip checklist found more than the plan estimated** (see below).

## Scope-creep ledger

Six defects were discovered en route and **deliberately left unfixed** — W0 ships `fix:` commits
only for the four absorbed stories plus the orphan-maker. Full detail, with file:line evidence,
is in **STATE.md's "Key facts discovered"** section:

1. `tailnet_identity()` unbounded probe (fixed here, but needs its own story — production).
2. Positional-taking verbs still swallow unknown `--flags` as data (`story new --typo x`).
3. **`story next` is nondeterministic** — `priority ASC, created_at ASC` with second-precision
   timestamps; same-second ties fall back to file-read order. Production-visible.
4. Id ordering is inconsistent across commands (numeric in `list`/`search`, lexicographic in
   `graph`/`handoff`/`context`/`summary`).
5. `story export --json` double-encodes — **fixed in W0b** (`d272a7b`); still needs a story
   for the record, as does its unfixed sibling `context --format json`.
6. `AppError::SyncConflict` is a dead variant, constructed nowhere in `src/`.

**None have story IDs yet, on purpose:** minting an ID from this worktree collides with IDs
minted elsewhere — which is the exact corruption `worktree_truth.rs` proves. File them after the
W4 flip.

## Story disposition

- **Fixed in this PR, close on merge:** SH-51 (readiness handshake + OS-assigned ports), SH-52
  (`--help` no longer creates a story), SH-53 (Spotlight/`$TMPDIR` fixtures), SH-59 (errors to
  stderr). These are storyhook stories, not GitHub issues — they close via story-sync at merge,
  not via a `Fixes #N` keyword.
- **Blocked on the rearchitecture** (marked in `be1601c` so concurrent sessions don't pick them
  up): SH-42, SH-43, SH-44 (web UI — deferred past W5; SH-43's "archived state" must not leak
  into the W1 schema), SH-49, SH-50 (want the daemon — re-spec after W5).
- **Goes green at W4:** SH-46, whose acceptance test is already in the tree and ignored.
