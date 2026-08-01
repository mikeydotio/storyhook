<!-- semver:start -->
## Semantic Versioning

This project uses semantic versioning managed by the `/semver` plugin.

### Version Awareness
- Read the `VERSION` file at the start of each conversation to know the current version.
- Read `.semver/config.yaml` to understand the versioning configuration.
- When discussing releases, deployments, or changes, reference the current version.

### Commit Discipline
- Write meaningful, descriptive commit messages. Each commit message may appear in an auto-generated changelog.
- Use conventional-commit-style prefixes when they fit naturally: `feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:`.
- The first line of the commit message should be a concise summary (under 72 characters). Add detail in the body if needed.

### Version Bump Guidance
When recommending or performing a version bump:
- **patch** (0.0.x): Bug fixes, documentation corrections, minor refactors with no behavior change.
- **minor** (0.x.0): New features, new capabilities, non-breaking additions to the public API or user-facing behavior.
- **major** (x.0.0): Breaking changes — removed features, changed interfaces, incompatible API modifications, behavior changes that require consumers to update.

When you notice the user has completed a logical unit of work, suggest running `/semver bump` with the appropriate level.

### Configuration
Versioning settings are in `.semver/config.yaml`. Do not modify this file unless the user explicitly asks to change semver settings.
<!-- semver:end -->

## Rearchitecture roadmap

Story data has moved out of per-repo `.storyhook/` directories into one global SQLite store
behind a local daemon. Design of record: [`docs/spec/data-layer-rearchitecture.md`](docs/spec/data-layer-rearchitecture.md).
Execution state — wave status, step log, discovered defects — lives in
[`docs/rearch/STATE.md`](docs/rearch/STATE.md); read it before resuming this program.

| Wave | Scope | Status |
|---|---|---|
| W0 | quality-gate repair, shared test harness, baseline capture | **complete — merged** |
| W0b | wire-serializable envelope + the `Invoker` seam | **complete — merged** |
| W1 | `Store` trait, SQLite engine, migrations, rebuild-diff | **complete — merged** |
| W2a | story lifecycle + relation services, differential harness | **complete — merged** |
| W2b | project, config, system and grouping services; 27 of 46 arms ported | **complete — merged** |
| W2c | query + integrity services; TUI onto the seam; 38 of 48 arms ported | **complete — merged** |
| W2d | git/GitHub/transfer services; **all 48 arms ported**; the store test leg | **complete — merged** |
| W3 | `src/legacy/` reader, `story migrate`, the round-trip rollback gate | **complete — merged** |
| W4 | **the flip**: the global store is the default; `worktree_truth` green | **complete — merged**. Revert only while `migrate_round_trip` is 4/4 green — procedure in the flip checklist's section D2 |
| W5 | daemon promotion + `/api/v1/invoke` transport | **complete — merged**, except the quarantine deletion, which W6 carried out |
| W6 | the quarantine deleted (10,849 lines); full commit-body scanning; link idempotency as a DB constraint | **complete — merged** |
| W7 | this repo's own tracker migrated; `.storyhook/` retired; the defect ledger filed | **complete — merged** |
| W8 | crash, concurrency, and corruption hardening — **the last wave** | **complete — merged** |

**The program is complete and merged.** Follow-on work now lands as ordinary
branches; the rules below still apply to anything touching the store or the seam.

**In flight:** `feat/store-isolation` — SH-113, the first child of the
server-owned epic (SH-112), and the origin-fix for SH-123. Daemon runtime state
is derived from the **canonical store path**, so one store has exactly one
daemon by construction; `--store-path`/`$STORYHOOK_STORE_PATH` name a store on
every command, and `story store new` creates one. Design of record:
[`docs/spec/store-isolation.md`](docs/spec/store-isolation.md), whose "As built"
section records the six decisions taken during implementation.

Three things to know before touching it:

- **`--store-path` becomes `$STORYHOOK_STORE_PATH`** in `main`, before anything
  resolves. That is what makes it reach `story daemon status`, the TUI, a git
  hook, and the daemon this run spawns — none of which are threaded.
- **Canonicalization must not change when the store file appears.**
  `Path::join("")` appends a separator; that turned one store into two daemons
  the moment its file existed, and is pinned by
  `the_same_path_resolves_the_same_before_and_after_it_exists`.
- **`$STORYHOOK_STORE_PATH` outranks `$STORYHOOK_DATA_DIR`**, so every harness
  neutralizes it — `TestEnv`'s `ISOLATED_VARS` plus the three shell harnesses.
  An exported one in a developer's shell would otherwise run the whole suite
  against their own store, and the data-dir guard would not notice.

Release-from-`main`, once the project-lifecycle rename lands: reinstall the
`story` binary — the installed one predates both the flip *and* that rename, so
`story init` still works there and `story project init` does not — then
`/semver bump major`.

Standing rules for every wave:

- Every commit passes `make test`; history stays bisectable and two-hats clean.
- **`make test` runs the suite in this process** (`STORYHOOK_INVOKER=local`);
  **`make test-daemon` runs the identical suite over `/api/v1/invoke`**; **`make gate` runs
  both** and is what a wave ends with and what a change to the tests should run. They stay
  separate because `test` is 114s against a 180s ceiling and the leg is another 60s — and
  because what the leg catches (a fixture that is only correct when nothing holds the store)
  is introduced when a test is *written*. `--test-threads=4` there is a **bound on how many
  daemons exist at once**, kept as a measured decision; the arithmetic is on the target.
- **`make test` must keep its isolated `STORYHOOK_DATA_DIR`** (`scripts/run-tests.sh`).
  ~45 test files still build fixtures with `tempfile::tempdir()` and inherit the process
  environment; without the override a test run writes into the developer's real store.
- **`app::run`, `lock.rs` and `registry.rs` are gone** (W6, 10,849 lines). `storage.rs`
  survives on purpose as the **rollback path** — `store -> export -> a legacy tree`, which
  `tests/migrate_round_trip.rs` runs end to end and the W4 revert policy is conditional on.
  Nothing under `src/` may call it: `invoker_seam.rs::the_legacy_write_path_is_gone` fails if
  a `src/` file so much as names `crate::storage`.
- **A test that asks about bytes on disk must not have a daemon** — use
  `ProjectBuilder::local()`. A daemon holds the store open with its own page cache and log
  handle, so it answers reads from memory and does not notice the file being replaced.
- **A test build refuses to resolve a real data home** (`storyhook::env::is_test_build`), which
  is what makes a bare `cargo test` safe. Consequence: the binary `cargo test` leaves in
  `target/debug` will not touch a real store; `cargo build` produces one that will.
- **Nothing in any test may bind port 3456 or outlive its run.** Four places export
  `STORYHOOK_DAEMON_ADDR=127.0.0.1:0` and `STORYHOOK_PARENT_PID`: `scripts/run-tests.sh`,
  `TestEnv`, and *both* `plugin/claude-code/tests/{lib.sh,run-tests.sh}` — the last two
  because `run-tests.sh` sets `STORYHOOK_TEST_HOME`, which makes `lib.sh` skip its block.
- Story IDs belong in commit **bodies**, never subjects — a subject reference makes the
  post-commit hook re-dirty the tree.
- Land your own work: merge commit, verify it landed, delete the branch. No direct pushes
  to `main`, no force-pushes, and no version bumps or deploys from a linked worktree.
- Deviations from the spec get recorded in the spec's own "As built" section — one
  document to open rather than two. (During the rearchitecture they went to
  `docs/rearch/STATE.md`, which stays the record for those nine waves.)
