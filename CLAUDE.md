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
| W7 | this repo's own tracker migrated; `.storyhook/` retired; the defect ledger filed | **complete — PR open** |
| W8 | crash, concurrency, and corruption hardening — **the last wave** | next |

Standing rules for every wave:

- Every commit passes `make test`; history stays bisectable and two-hats clean.
- **`make test` runs the suite in this process** (`STORYHOOK_INVOKER=local`);
  **`make test-daemon` runs the identical suite over `/api/v1/invoke`.** They stay separate:
  two thousand tests each taking a network hop through one daemon would be slower and would
  couple every test to one process's health, and the in-process-vs-RPC byte-comparison test is
  what proves the two modes agree. `--test-threads=4` in the daemon leg is a **bound on how
  many daemons exist at once**, not tuning.
- **`make test` must keep its isolated `STORYHOOK_DATA_DIR`** (`scripts/run-tests.sh`).
  ~45 test files still build fixtures with `tempfile::tempdir()` and inherit the process
  environment; without the override a test run writes into the developer's real store.
- **`app::run`, `storage.rs`'s write half, `lock.rs` and `registry.rs` are quarantined and
  now genuinely dead** — the dashboard was their last caller and it runs on the services.
  Deleting them is mechanical; HANDOFF.md carries the measured blast radius. Do not add
  callers: `invoker_seam.rs::the_legacy_path_is_reachable_only_from_the_web_dashboard` fails
  on any.
- **Nothing in any test may bind port 3456 or outlive its run.** Four places export
  `STORYHOOK_DAEMON_ADDR=127.0.0.1:0` and `STORYHOOK_PARENT_PID`: `scripts/run-tests.sh`,
  `TestEnv`, and *both* `plugin/claude-code/tests/{lib.sh,run-tests.sh}` — the last two
  because `run-tests.sh` sets `STORYHOOK_TEST_HOME`, which makes `lib.sh` skip its block.
- Story IDs belong in commit **bodies**, never subjects — a subject reference makes the
  post-commit hook re-dirty the tree.
- A wave's implementing session ends at "PR opened" and never merges its own PR. The
  **orchestrator** merges the wave PR (merge commit), verifies it landed, and deletes the
  branch, escalating only what genuinely warrants attention. Work happens in a linked worktree:
  no version bumps, no deploys, no direct pushes to `main`, no force-pushes.
- Deviations from the spec get recorded in STATE.md rather than edited into the spec.
