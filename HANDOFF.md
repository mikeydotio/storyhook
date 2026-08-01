# Handoff — store isolation (SH-113)

**Branch:** `feat/store-isolation`, three commits off `main` (v1.0.0).
**Stories:** SH-113 (this one) — first child of epic **SH-112**. Closes **SH-123**.
**Design of record:** [`docs/spec/store-isolation.md`](docs/spec/store-isolation.md);
its "As built" section records the six decisions taken during implementation.

*(The previous handoff, for `feat/project-lifecycle-verbs`, is superseded — that
branch merged as #78. Its one outstanding item, the major version bump, is folded
into "What remains" below.)*

## What changed and why

Daemon runtime state was keyed on the state home while the store was keyed on the
data home. The two move independently and nothing reconciled them, so a client
pointed at one store was served — for reads *and* writes — by a daemon holding
another, with no diagnostic and exit 0. `STORYHOOK_DATA_DIR` was not an isolation
mechanism; it worked only when hand-paired with `XDG_STATE_HOME`, which four shell
harnesses did and nothing in the binary required.

**A store's identity now determines its daemon's identity.** The portfile, pidfile,
spawn lock and log all hang off `$XDG_STATE_HOME/storyhook/daemons/<key>/`, where the
key is the first 16 hex of the SHA-256 of the store's canonical path. One store has
exactly one daemon by construction; "client and daemon disagree about the store" is
unrepresentable rather than detected.

- `--store-path <file>` on every command, plus `$STORYHOOK_STORE_PATH`
- `story store new <path>` — refuses the default store and an existing path
- `DaemonInfo.store_path`, so a portfile is self-describing
- the default store keeps port 3456; any other store binds port 0

## Five things to know before touching it

1. **`--store-path` becomes `$STORYHOOK_STORE_PATH` in `main`**, canonicalized,
   before anything resolves. That is what makes it reach `story daemon status`, the
   TUI (dispatched before the parser), a git hook, and the daemon this run spawns —
   none of which are threaded. The spawned daemon *also* gets it on its argv, which
   is redundant on purpose.
2. **Canonicalization must not change when the store file appears.**
   `Path::join("")` appends a separator, so the "deepest existing ancestor" walk
   returned `…/store.db/` once the file existed — a different key, a second daemon,
   and an `exists()` of `false`. Pinned by
   `the_same_path_resolves_the_same_before_and_after_it_exists`. Do not loosen it.
3. **`$STORYHOOK_STORE_PATH` outranks `$STORYHOOK_DATA_DIR`.** Every harness
   neutralizes it: `TestEnv`'s `ISOLATED_VARS` (now six) plus `scripts/run-tests.sh`
   and both plugin harnesses. An exported one in a developer's shell would otherwise
   run the whole suite against their own store, and the data-dir guard would not
   notice — it inspects the variable that lost.
4. **Backups are keyed for a named store, unkeyed for the default one.**
   `run_if_due` prunes to seven, so a scratch store's daemon sharing
   `state_home/backups` would delete the real store's backup history. The default
   store keeps the path its snapshots are already at rather than pay a migration.
5. **`check-no-orphan-servers.sh` matches the daemon's argv**, and a flag now sits
   between the binary and the verb. Its pattern was widened — a guard that matches
   nothing passes silently.

## The gates

```sh
make test           # in-process, plus 18/18 bash
make test-daemon    # the identical suite over /api/v1/invoke
make gate           # both
```

Both green on the branch tip. `tests/store_isolation.rs` is the new file: 13 tests,
every one of which failed before the change.

## What remains

1. **The merge decision is yours.** CLAUDE.md's standing rule says an implementing
   session opens the PR and never merges its own, for anything touching the store or
   the seam. This touches both, and there is no orchestrator session any more — so
   the PR is open and waiting on a word.
2. **This is breaking**, so it wants `/semver bump major` **from `main` after
   merge** — together with the still-pending bump for the `story project init`
   rename. One v2.0.0 covers both.
3. **Reinstall the `story` binary** from `main`. The installed one predates the
   flip, the `project init` rename *and* this change, so it still publishes its
   portfile at the old unkeyed path. The upgrade handles that — a live legacy daemon
   is stood down under the spawn lock, and it has its own test — but it is worth
   watching the first time.

## Observed, not a defect, not filed

Running `story --store-path B project init --prefix SCR` in a checkout already
initialized in store A reuses the identity in that checkout's `.storyhook.toml`, so
the new project keeps store A's prefix and `--prefix` is ignored. That is the
portable-project-identity contract working as designed — the two stores each get
their own `AMB-1`, so it is not cross-talk — but it surprises. If the epic's later
children want `--prefix` to win there, it is a separate story.

## Next in the epic

SH-113 unblocks **SH-114**, **SH-115** and **SH-122**. **SH-95** stays open on
purpose: `refuse_temp_project_in_real_store` is still the only thing standing there
until the epic's later children remove path-based project creation. Its second
argument is now the store *file* rather than the data home.
