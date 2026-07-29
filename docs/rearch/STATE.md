# Rearchitecture Execution Ledger

> **Purpose:** continuity record for the data-layer rearchitecture. Any session resuming this
> program reads this file first, then the approved plan. Update + commit after EVERY step.
>
> **Spec of record:** [`docs/spec/data-layer-rearchitecture.md`](../spec/data-layer-rearchitecture.md)
> — the design. This file is the execution state; that file is what we agreed to build.
> Companion: [`flip-checklist.md`](flip-checklist.md) (the enumerated W4 work).
> **Worktree:** `/Volumes/Code/mikeyward/storyhook/.claude/worktrees/rearch` (linked worktree —
> NO version bumps, NO deploys, NO force-push, NO touching main).
> **Execution model:** orchestrator (main session) spawns one subagent per step, sequentially.
> Each subagent: does the step TDD-style, commits (story IDs in commit BODIES only, never
> subjects), updates this file's Step Log + status table, includes it in its final commit,
> runs `make test`, reports tersely, and **stops at "PR opened"** — a subagent never merges,
> not even its own PR.
> **Merge policy (changed 2026-07-28, W0b):** the wave no longer stops at "PR opened" waiting on
> Mikey. The **orchestrator** merges each wave PR itself (merge commit — the only method the org
> allows), verifies the merge landed, then deletes the branch; it escalates to Mikey only for
> genuinely attention-worthy items. **This supersedes the spec**, which still says a wave ends at
> "PR opened" at `data-layer-rearchitecture.md` lines 73 and 520 — per this file's own rule,
> process changes are recorded here rather than edited into the design of record.

## Wave status

| Wave | Branch/PR | Status |
|---|---|---|
| W0 gate repair + harness | `rearch/w0-gate-repair` / [PR #60](https://github.com/mikeydotio/storyhook/pull/60) | **MERGED** 2026-07-28 |
| W0b envelope + Invoker seam | `rearch/w0b-envelope` / [PR #61](https://github.com/mikeydotio/storyhook/pull/61) | **MERGED** 2026-07-28 |
| W1 store engine | `rearch/w1-store` / [PR #62](https://github.com/mikeydotio/storyhook/pull/62) | **MERGED** 2026-07-28 |
| W2a services (lifecycle + relations) | `rearch/w2a-lifecycle` / [PR #63](https://github.com/mikeydotio/storyhook/pull/63) | **MERGED** 2026-07-28 |
| W2b services (project + config + system + grouping) | `rearch/w2b-config` / [PR #64](https://github.com/mikeydotio/storyhook/pull/64) | **MERGED** 2026-07-28 |
| W2c services (query + integrity) + TUI on the seam | `rearch/w2c-query` / [PR #65](https://github.com/mikeydotio/storyhook/pull/65) | **MERGED** 2026-07-28 |
| W2d services (git + GitHub + transfer) + the store test leg | `rearch/w2d-git` / [PR #66](https://github.com/mikeydotio/storyhook/pull/66) | **MERGED** 2026-07-28 |
| W3 importer | `rearch/w3-importer` / [PR #67](https://github.com/mikeydotio/storyhook/pull/67) | **MERGED** 2026-07-28 |
| W4 THE FLIP | `rearch/w4-flip` | **PR OPENED** — revert only while `migrate_round_trip` is 4/4 green |
| W5 daemon | `rearch/w5-daemon` | **PR OPENED** — the quarantine deletion deferred, see the step log |
| W6 git features | — | pending |
| W7 repo cutover | — | pending |
| W8 hardening | — | pending |

## W0 step plan

- **W0.0 DONE** — story bookkeeping: SH-42/43/44 deferred, SH-49/50 blocked (commit `be1601c`).
- **W0.1 DONE** — five gate fixes (four planned + one production defect the readiness fix
  exposed), each its own `fix:` commit + regression test: `2316ddf` SH-53, `4c1aed9` SH-51,
  `e8d4cf8` tailnet probe, `da3109d` SH-59, `e2531c4` SH-52. See Step log.
- **W0.2 DONE** — `storyhook-test-support` workspace crate (TestEnv, ProjectBuilder,
  scratch_dir, server/daemon helpers), clippy `$TMPDIR` ban, 3 proof files migrated,
  bash suite data-home isolation: `877c31f`, `a170106`, `c444313`, `f4eefe1`, `ca2b4a7`.
  See Step log for the API surface W0.3 builds on.
- **W0.3 DONE** — RED worktree pair (ignored, W4's exit criterion), insta golden CLI corpus
  (177 invocations / 27 snapshots), error-code table (all 10 variants), byte-identical export
  round-trip: `9aa8df1`, `b7539f3`, plus this commit. See Step log.
- **W0.4 DONE** — baseline capture: `scripts/capture-baseline.sh` (`4179f48`) →
  `docs/rearch/baseline/` (this commit). **Census 10/10 green**, 1171 Rust tests / 52
  binaries / 2 ignored, gate median 36.4s. See Step log.
- **W0.5 DONE** — spec of record at `docs/spec/data-layer-rearchitecture.md`; W4 flip checklist
  at `docs/rearch/flip-checklist.md`; CLAUDE.md mini-roadmap; HANDOFF.md; PR opened. See Step
  log for the checklist's corrections to the plan's estimates.

## W0b step plan

- **W0b.1 DONE** — `story export --json` double-encoding fixed at origin (`Response::RawJson`),
  red→green with an import round-trip regression test: `d272a7b`.
- **W0b.2 DONE** — wire-serializable envelope: `Deserialize` on `Response` + every view type,
  `Serialize + Deserialize` on `Invocation`/`CliOptions`/the six action enums, `WireError`
  mirror for `AppError`, and `tests/wire_envelope.rs` (9 tests): `2db8310`.
- **W0b.3 DONE** — `Invoker`/`InvokeRequest`/`LegacyInvoker` in `src/invoke.rs`, adopted by
  `main.rs` and `web.rs`'s three dispatchers; `tests/invoker_seam.rs` (4 tests): `ef717f2`.

## W1 step plan

- **W1 DONE** — `src/store/`, pure new code, nothing wired. Six commits:
  `ebb7522` (domain `PartialEq` derives), `a7a8cc6` (schema v1 + migrations +
  engine), `33c9454` (rebuild-diff oracle + fault points), `aac3a5c` (priority
  slug constraint — a defect this wave's own tests found), `e792a3e` (the
  162-case conformance suite), `9f7d0e4` (property tests). See Step log for the
  public API W2a builds on and the deviations.

## W2a step plan

- **W2a DONE** — `src/service/` + `invoke::dispatch`, additive; `src/app.rs` untouched.
  Four commits: `845afb9` (the fold fix reopen needs), `c67642b` (context, dispatch
  skeleton, StoryService), `fa4d610` (RelationService), `757bc7b` (the differential
  harness). See Step log for the service API W2b/c/d builds on and the deviations.

## W2b step plan

- **W2b DONE** — `src/service/{project,config,system,grouping,templates}.rs` +
  `invoke::dispatch_unscoped`, additive; `src/app.rs` untouched. Five commits:
  `4056817` (ProjectService + atomic init + the pointer file), `afa2bca` (ConfigService),
  `b4e7598` (SystemService + the text-only arms), `1354798` (differential rows +
  the shared harness extraction), `ddfe113` (GroupingService). See Step log for the
  service API W2c/W2d build on, the ported-arm roster, and the deviations.

## W2d step plan

- **W2d DONE** — the last ten dispatch arms, the store test leg, and five defects
  the leg found. Six commits: `c2b214c` (transfer: export/import/import-project/
  decompose), `1be07fe` (GitService), `1babe06` (github-sync's storage edges behind
  a `SyncStorage` seam), `40ba417` (catalog/session/history/update — roster complete),
  plus the store-leg commit and the docs commit. See Step log for the API, the
  roster, the exclusion list and the five defects.

## W3 step plan

- **W3 DONE** — `src/legacy/` (permanent, read-only, zero coupling to
  `storage.rs`), `story migrate`, and the round-trip guarantee the W4 revert
  policy rests on. Six commits: `fb9b2f9` (the packed-debuginfo chore),
  `75dd9c1` (a harness race two concurrent gates hit), `036c768` (the reader),
  `46e9e13` (a `set -u` bug in the store-leg script), `6f817c3` (`story
  migrate`), `a5871a6` (the round trip), plus this docs commit. See Step log
  for the API, the SH-60 ruling, the dry-run evidence and the deviations.

## W2c step plan

- **W2c DONE** — `src/service/{query,integrity}.rs`, the read surface and `doctor` on
  `dispatch`, `Invocation::{ProjectSnapshot, History}`, and `src/tui/` moved onto the
  `Invoker` seam. Five commits: `e9e170e` (QueryService), `d3ee37c` (a store `fix:` its
  own tests found), `eb6aa63` (IntegrityService), `b937e8c` (the two seam-only
  invocations), `8bf7eeb` (the TUI). See Step log for the API, the roster, the two
  defects found, and the deviations.

## W4 step plan

- **W4 DONE** — the flip. Eleven commits; `make test` green after each.
  `e97ca5f` (the injection API + store fixtures), `ff634f8` (a differential flake),
  `1c512d0` (pointer config + registry adoption), `ab6b2ff` (the ancestor walk),
  `e8c1d1c` (the pre-flip diff harness), `1e70f58` (the exclusion-list burn-down),
  `4b932ed` (**THE SWAP**), `1445291` (the exit criterion), `c50a650` (the guard),
  `ca4f4f7` (compensating undo), `eef531d` (deletions + quarantine), plus this
  docs commit. See Step log for the deviations — the commit *order* changed, and
  the reason matters.

## W5 step plan

- **W5 DONE except the quarantine deletion.** Seven commits; `make test` green
  after each. `5996898` (the injected `Environment`), `a5320d9` (the HTTP
  plumbing and tailnet identity extracted verbatim), `c4365c3` (the dashboard
  over the service layer; the change bus; the notify watcher dies), `74bd28e`
  (daemon lifecycle: portfile, pidfile lock, hello, auto-spawn, launchd, the
  `web` aliases), `32e641e` (`/api/v1/invoke` and `HttpInvoker`, which becomes
  the default), `7288e70` (the TUI off `notify`; the dependency dies), `44c5bf7`
  (`make test-daemon`, daily backups, and the defects the leg found).
  **Deferred: deleting `app.rs`, `lock.rs`, `registry.rs` and `storage.rs`'s
  write half.** See the step log for why and for exactly what the next session
  has to do.

## Key facts discovered (do not re-derive)

- **W5, the finding that matters most for W8: the daemon leg cannot run at the
  default test parallelism.** `make test-daemon` gives each test *binary*'s
  shared environment a daemon and each `TestEnv::isolated()` another, so `cargo
  test`'s default fan-out starts dozens of SQLite-holding processes at once and
  the machine spends its time context-switching rather than testing.
  `move_if_state_under_real_concurrency_yields_exactly_one_winner` passes in
  **1.2s alone** and stalls past 60s in the wide-open run. `--test-threads=4`
  is therefore a *bound on live daemons*, not tuning. W8 owns deciding whether
  the leg's permanent shape is bounded parallelism, one daemon for the whole
  run, or something else.
- **W5: the bash plugin suite was spawning real daemons on the production
  port**, and this is the sharpest instance yet of the class STATE.md has
  recorded twice before. Two `story daemon --serve --port 3456` processes from
  this worktree were found alive after a gate run. `lib.sh` had the isolation
  block and `run-tests.sh` did not — and `run-tests.sh` sets
  `STORYHOOK_TEST_HOME` itself, which is exactly what makes `lib.sh` skip its
  block. **Generalize this: a guard written in the file that is skipped when
  the other file runs is not a guard.** Both now export `STORYHOOK_INVOKER`,
  `STORYHOOK_DAEMON_ADDR=127.0.0.1:0` and `STORYHOOK_PARENT_PID`, duplicated
  the same way the XDG variables already were and for the same reason.
- **W5: `PRAGMA data_version` is per *connection*, not per database.** It counts
  commits made by other connections since *this one* last looked, so asking a
  pooled connection answers a different question every call and two pooled
  connections hold unrelated counters. `SqliteStore` keeps one connection out of
  the pool for `change_token`. Symptom before the fix: the SSE feed silently
  never fired for in-process writes.
- **W5: a `GET` was publishing a change event.** Every successful request under
  `/api/repos/<id>/…` was marked as having changed that project, reads
  included — every client refetches because a client fetched, at the rate the
  browser retries. It surfaced as an SSE test going *quiet*, because the read's
  event coalesced away the real one behind it. Pinned by
  `a_read_reports_no_change_however_well_it_went`.
- **W5: three things cannot cross the wire, and each was a real bug.**
  (1) A **relative path** is relative to the user's shell, not the daemon's
  working directory — `story decompose spec.md` read the wrong file. Resolved
  against the envelope's `cwd`, with the error still naming the path the user
  typed. (2) **Standard input** cannot be reached from the daemon at all;
  `story import` read `/dev/null` and said "no stories to import", which is a
  silent wrong answer. The client reads it and it travels in the envelope.
  (3) **The daemon's own lifecycle** must never route through the daemon:
  `story daemon stop` started one in order to stop one, and hung rather than
  failed.
- **W5: `story daemon start` reused a daemon from another build**, because it
  short-circuited on "something is running" before the version check. `start` is
  `ensure` plus a port override and nothing else now. Sibling: **a port asked
  for on the command line was lost across the spawn** — the client held the
  override in its own `Environment` and the child built a fresh one from the
  process environment. The port travels on the argv.
- **W5: `DaemonInfo::is_this_binary` asks about the *calling* process**, which is
  right in production (the caller is `story`) and wrong in a test binary. A test
  that wants the question asked about the binary under test has to compare
  against `story_binary()` itself — `daemon_lifecycle.rs::is_the_binary_under_test`.
- **W5: a hook's `story` must be an absolute path in a test.** A bare `story` in
  a `hooks.toml` resolves through `PATH`, which in a test run is the developer's
  *installed* storyhook — a different build, which the daemon then refuses and
  tries to restart. The reentrancy tests spell the binary out.
- **W5, deliberate deviations from the wave brief, all four recorded in the
  code that makes them:**
  1. **The TUI polls the store's change token rather than subscribing to SSE.**
     It holds its own store handle — it is a `--local` client by construction —
     so subscribing would make a TUI that works today stop updating on a machine
     where the daemon is down, and would learn the same fact one layer further
     away over a connection that can drop.
  2. **Backup age is reported by `story daemon status`, not by `story doctor`.**
     `doctor`'s bytes are pinned by the golden corpus and its exit code means a
     project's integrity; a backup's age is a fact about the machine.
  3. **The change-token poller attributes changes per project** rather than
     publishing a bare `resync`, because `repo-changed` with a slug is the SSE
     contract the dashboard already had. Only a change it cannot attribute — a
     state definition edited by a `--local` writer, which appends no story event
     — becomes a `resync`.
  4. **`story daemon` gained `install`/`uninstall` in the lifecycle commit**
     rather than a separate one: the `web` aliases and the launchd agent are
     both "commands about the daemon", and splitting them would have left an
     intermediate commit with two lifecycles in it.

- **W4, THE most important structural finding: the brief's commit order could not
  be green, and inverting two slots fixed it.** The plan was swap-then-rewrite-tests
  (slots 4 then 6). But `make test` runs the legacy leg before the swap and the store
  leg after it, so **every file on `make test-store`'s exclusion list is a file the swap
  turns red** — 57 tests across 15 files, measured. A swap commit that leaves 57 tests
  failing is not a bisect atom, it is a broken commit with a small production diff.
  The burn-down therefore has to come *first*: each test rewritten to assert the same
  thing on either storage model, verified green on **both** legs, and only then the
  default flipped. 57 → 16 before the swap; the last 16 are irreducibly leg-specific
  (they fabricate corruption in one model or the other) and rode with the swap.
  **Generalize this: in a strangler, the test suite crosses over before the default
  does, and the exclusion list reaching zero is the swap's precondition rather than its
  consequence.**
- **W4: after the flip, any test that builds a fixture with `tempfile::tempdir()` and
  runs `story` writes into the DEVELOPER'S REAL STORE.** ~45 files are in that shape.
  `make test-store` was safe only because `run-store-leg.sh` exported an isolated
  `STORYHOOK_DATA_DIR` for the whole run; plain `make test` had no such export. Fixed by
  `scripts/run-tests.sh`, which sets one and **refuses to run** if it is not under
  `/private/tmp`. That refusal is not decoration: the consequence of losing the override
  is silent and expensive, and there is no way to detect it after the fact.
- **W4: `is_project_less` is a trap that springs repeatedly, and it sprang twice more.**
  `story web status` and `story update --check` both failed with "story project not
  initialized" — in exactly the situations they exist for. Both answer without a project
  inside `dispatch_unscoped`; both were missing from `is_project_less`, so resolution
  refused them before they were reached. STATE.md already carried a warning about this
  shape from W2d, and it happened anyway. It is now a **test** rather than a warning:
  `invoker_seam.rs::the_project_less_verbs_all_answer_outside_a_project` sweeps all
  fourteen from an empty directory.
- **W4, FIXED in-wave — `story web register --name` silently dropped the name.** The
  legacy registry held a display name per repo; the catalog is the projects table now and
  `register` had nowhere to put one, so the flag was accepted and discarded. Added
  `WriteOps::rename_project`. A flag that is accepted and ignored is worse than one that
  does not exist, and this was a regression the flip would have introduced.
- **W4: the undo redesign needed exactly two new event kinds, and no more.**
  `StoryCommentRetracted` (comments are the one part of a story that only accumulates)
  and `StoryAssigneeCleared` (the sibling of `StoryAwaitingCleared`). `EVENT_KINDS`
  15 → 17. A story's **type** deliberately has no clearing event: nothing in the TUI sets
  a type, so the undo stack cannot produce that case, and `restore` refuses loudly naming
  the field. **Consequence for the rollback, which belongs in D2's table**: an older
  storyhook cannot decode either kind, so `story export` of a project containing one and
  `import-project` on the reverted binary fails *loudly* with serde's unknown-variant
  error. Loud, not silent — but it narrows the revert window for any project whose undo
  has been used.
- **W4: `domain::EVENT_KINDS`'s drift test earned its keep on the first try.** The first
  attempt put `StoryAssigneeCleared` after `StoryAwaitingSet` rather than after
  `StoryAssigned`, and `every_known_kind_is_a_variant_and_every_variant_is_known` failed
  with the two orderings side by side. A hand-maintained list next to a derive is exactly
  the thing that rots; this one cannot.
- **W4, FIXED in-wave — `differential_git`'s empty-window row was a latent flake**, and it
  fired on the wave's first full run. Both legs read one git repository but each resolves
  `--since=0d` against *its own* `now`, and they do not run in the same second: a commit
  made in the current second lands on whichever side of the cutoff each leg falls. Fixed
  in the fixture (`Differential::commit_at` pins the commit's date) because neither leg is
  wrong. **`GIT_AUTHOR_DATE` does not accept relative expressions** — `1 hour ago` fails
  with `fatal: invalid date format` rather than falling back to now, which turned the flake
  into a deterministic 20/20 failure before it was noticed.
- **W4: `story doctor` has no CLI-reachable finding any more, and that is the design
  working.** Every shape it used to report is refused by the schema, and the two that
  remain (histories that disagree about an edge; a type the catalog does not define) need
  `store::test_support` to fabricate. Consequence: the *bash* suite cannot build one, so
  `test-doctor-capture.sh` exercises the shell property it actually owns — that story.sh
  reads exit 5 as a finding rather than as a failure of its own probe — through
  `fakes/story-integrity/story`, with the CLI's real exit code pinned in Rust.
- **W4: the pointer file's `[plugin]`/`[hooks]` tables are `toml::Value`, not typed
  structs, and this is load-bearing.** Resolution must not depend on configuration: a
  typed `[hooks]` table would mean a misspelled `timeout_seconds` failed the whole parse
  and left storyhook unable to say which project it was standing in. Pinned by
  `a_broken_config_table_does_not_make_the_repository_unresolvable`.
- **W4: `story init` must resolve by pointer before path.** On a fresh clone the checkout
  is at a path the store has never seen while its pointer names a project the store may
  well know; answering by path alone mints a second project for a repository that already
  has one, leaving the clone pointing at the old identity and storing into the new.

- **W3, the single most important finding of the wave: this repository's 15
  live SH-60 violations are 10 one-sided relations AND 5 stories with two
  parents, and the second set is CAUSED by naively repairing the first.**
  Ten `StoryRelationshipAdded` claims exist on only one end. Complete all ten
  and SH-32/33/34/35/36 each end up with two parents (SH-31 and SH-40), which
  the store's partial unique index refuses — so the import fails mid-write with
  `a story may have at most one parent`. **Any relation check must therefore run
  over the CLOSURE of both ends' claims, after repairs, never before.** Checking
  each story's own `child-of` claims finds zero violations in this tree and is
  the wrong question: a `parent-of` claimed by the parent alone is mirrored into
  the child's row by the schema trigger.
- **W3's ruling on repair-vs-refuse, so a later wave does not relitigate it:
  agreement beats assertion.** A relation only one end recorded is *completed*
  (the missing half is a missing event; the repair carries the original claim's
  instant and is spliced in in timestamp order, so `updated_at` does not move).
  A *parentage* only one end recorded, where the child has another parent both
  ends recorded, is *retracted* — the add event is still imported verbatim, a
  `StoryRelationshipRemoved` sits beside it, and only the read model changes.
  Where the rule has nothing to weigh (two mutual parents, or several that all
  disagree) the migration refuses. **The alternative — refusing every
  multi-parent tree — was rejected after writing the remedy down**: every story
  in this repository's five conflicts is *archived*, and `story unrelate`
  resolves open stories only (`resolve_open_story`), so the advice would have
  been "reopen five closed stories, unrelate, re-close", which rewrites more
  history than the retraction does and does it through `unarchive_story`, which
  strips closure markers.
- **W3: `ProjectExport` is narrower than a legacy tree, and W4's rollback
  inherits the gap.** The envelope carries schema, prefix, states, types,
  members and stories. A *tree* also carries `project.toml`'s `created_at` and
  its `sync`/`doctor` settings, and `next-id`'s burned numbers. `story migrate`
  carries all three beside the envelope (`put_settings`, the project's real
  `created_at`, `reserve_story_no` from the counter rather than from a story
  count) and prints the settings in its report. **A rollback through `export` +
  `import-project` does not carry them**, which is one of the reasons
  `.storyhook/` stays in the repository until W7. Tabulated in the flip
  checklist's new section D2.
- **W3: `TransferService::export` silently drops unknown-kind events.** It calls
  `partition_known` and discards the diagnostics, so a store holding an event
  kind this binary does not understand exports a document without it. Nothing
  writes such a kind today, so it is latent — but it means the round-trip
  guarantee is conditional on "no unknown kinds in the store", and the flip
  checklist's D2 table says so. **Needs a story.** The migration itself is not
  affected: it writes raw events, and `an_unknown_event_kind_is_imported_
  verbatim_and_named_in_the_report` pins that.
- **W3: the legacy archive database cannot be opened read-only the ordinary
  way.** `archive.db` is in WAL mode (header bytes 18/19 are `2`), and a
  `SQLITE_OPEN_READONLY` connection to a WAL database needs to create the
  `-shm` sidecar — which is a write into the tree the reader has promised not
  to touch, and which fails outright when the directory is read-only
  (`sqlite3 "file:…?mode=ro"` on the frozen fixture: `unable to open database
  file (14)`). `src/legacy/` uses `immutable=1`, which takes no locks and
  creates nothing. **The catch, and it is the whole reason the WAL guard
  exists:** `immutable=1` reads straight *past* a `-wal` file. So the reader
  refuses a non-empty `archive.db-wal` by name rather than silently reporting a
  stale archive. A zero-length one is what SQLite leaves after a checkpoint and
  is fine.
- **W3: a corrupt *known* event kind and an *unknown* event kind are
  indistinguishable to serde**, and only one of them is safe to wave through.
  `serde_json::from_str::<StoryEvent>` returns `Err` for both a `StoryCreated`
  with no `title` and a `StoryPinned` from a future storyhook. The store's own
  `decode` treats every failure as `Unknown`, which is right for a store and
  wrong for an importer: retaining a corrupt `StoryCreated` as an opaque blob
  imports a story with no title and never says so. `domain::EVENT_KINDS` is the
  discriminator, and it cannot drift — its test parses serde's own `unknown
  variant, expected one of …` list back out of the derive and compares.
- **W3: `story migrate` is NOT on the store test leg, and must not be.** It is
  caught by the `service_` prefix, and that is correct twice over: its fixtures
  build a `.storyhook` tree by running `story init`/`story new`, which under
  `STORYHOOK_INVOKER=local` write to the store and leave nothing to migrate.
  `legacy_reader` and `migrate_round_trip` *are* on the leg (38 → 40 targets),
  because they call the library in-process and pass identically either way.
- **W3, FIXED in-wave (`46e9e13`) — `scripts/run-store-leg.sh` aborted when run
  with no arguments.** `set -u` plus bash 3.2 (what macOS ships) treats an empty
  array as unset, so `"${exact[@]}"` was `unbound variable`. Only `make
  test-store` worked, because the Makefile always passes a long exclusion list.
  Three sites, same shape.
- **W3, FIXED in-wave (`75dd9c1`) — two concurrent `make test` runs raced on one
  fixture name.** `the_sweep_only_touches_this_harnesss_own_fixtures` created its
  control directory at a *constant* path under the machine-global
  `/private/tmp/storyhook-tests`, so a gate run in the worktree and one in the
  main checkout both created, asserted on and `remove_dir_all`ed the same
  directory. Seen once as `the sweep must never remove a directory it cannot
  prove it created`, green on re-run. **Generalize this:** this program runs its
  gate from a worktree by design, so any fixture at a fixed path under a shared
  root is a latent flake.
- **W3, build hygiene: `[profile.dev] split-debuginfo = "packed"`.** macOS's
  default leaves one `.o` per codegen unit per rebuild loose in
  `target/debug/deps`; ~200k of them accumulated in 24 hours and stalled
  `FSEventStreamStart` *machine-globally* (registration is serialized, ~5ms per
  stream at that size), which mass-failed `web_test`'s wall-clock readiness
  deadlines while the same tree passed in isolation. Health metric:
  `ls target/debug/deps | wc -l` — trouble starts in the tens of thousands.

- **W2d, the biggest trap in the harness: `TestEnv` isolates CHILD PROCESSES,
  not in-process library calls.** An integration test that calls `storyhook::…`
  directly — which every `service_*` and `differential_*` file does — sees the
  *developer's real* `HOME`, `XDG_DATA_HOME` and `XDG_STATE_HOME`. This wave hit
  it twice, both times writing into Mikey's actual home directory: a
  github-sync backup landed in `~/.local/state/storyhook/`, and a `web register`
  differential row wrote a fixture path into `~/.storyhook/registry.toml`. Both
  are fixed at the source rather than in the test —
  `StoreSyncStorage::backups_dir()` takes the destination as a parameter, and
  the `web` differential rows are deleted with the reason written where they
  were — but **the general hazard is live for every later wave**. The rule:
  *if a service reads a global path from the environment, an in-process test
  cannot redirect it, so the path must be a parameter.* `make test-store` deals
  with it differently and correctly, by exporting `STORYHOOK_DATA_DIR` and
  `XDG_STATE_HOME` for the whole run before cargo starts.
- **W2d, FIXED in-wave — `PRAGMA journal_mode = WAL` is not free on a database
  that is already in WAL.** It takes an exclusive lock to decide it has nothing
  to do, so every process opening the store paid for one, and enough of them at
  once (a parallel test run; a hook shelling out to `story`) turned `story init`
  into `LockTimeout` — exit 4, on a command that did nothing wrong.
  `SqliteStore::new_connection` now reads the mode first and writes only when it
  differs. **Generalize this:** a pragma that "is a no-op after the first time"
  is a no-op in its *effect*, not in its locking.
- **W2d, FIXED in-wave — `migrate()` was not concurrency-safe.** `run` decides
  what is pending outside any transaction, so two processes opening a fresh
  store both saw version 0 and both queued migration 1; the loser's `CREATE
  TABLE schema_migrations` failed with `error: migration 1 (initial) failed:
  table schema_migrations already exists`, exit 5. `apply` now re-reads
  `user_version` **inside** its `BEGIN IMMEDIATE` and returns `false` when it
  has been overtaken, so `MigrationReport::applied` still reports only what this
  call changed. Reachable in production the moment two `story` invocations race
  on a machine whose store has not been created yet — which is every machine,
  once.
- **W2d: root resolution has three tiers, and this is the shape W4 inherits.**
  `StoreInvoker` (1) answers project-*less* invocations before looking for a
  project at all — `init`, `import-project`, the help family, `version`,
  `plugin`, `hooks` except `test`, and `decompose --dry-run`; (2) resolves the
  project by the checkout's pointer file *then* by its path, and never walks
  upwards, because `storage::ensure_project` looks only at `<cwd>/.storyhook`
  and a store leg that resolved a parent's project would answer questions the
  legacy leg refuses; (3) failing that, still answers `session-start` with `{}`
  and `scaffold` with default values, both of which the legacy path did. Each
  tier reproduces something real. **Adding a project-less arm to `dispatch`
  without adding it to `is_project_less` makes that verb fail in an empty
  directory**, which is the failure the function exists to prevent.
- **W2d: `story export`'s `prefix` field is `null` for the default prefix**, and
  that is contract, not a bug. `project.toml` stored the prefix as an *option*
  that `story init` left unset unless `--prefix` was given, and every reader
  defaults an absent one to `SH`. `service::transfer::exported_prefix` reproduces
  it. The one case it cannot reproduce is a project initialized with an explicit
  `--prefix SH`: legacy says `"SH"`, the store says nothing, and both import to
  the same project.
- **W2d: `story import` is deliberately more forgiving than `story new`.** An
  unparseable priority is a rejection in `StoryService::create` and is silently
  dropped by `TransferService::import`. That asymmetry is inherited on purpose —
  a script that has been feeding storyhook `"priority": "urgent"` for a year
  must keep getting the same stories out — and it is why the batch importer
  builds its own events instead of calling `create`. The two batches are
  otherwise identical field for field and in the same order.
- **W2d: `import-project` refuses a project that already holds stories**, where
  the legacy importer overwrote it. An append-only store cannot rewrite a
  story's history, and a restore that half-overwrites a live project is how a
  tracker loses one. Pinned by
  `differential_transfer.rs::import_project_diverges_on_a_project_that_already_has_stories`.
  **W4 should list it in the flip's behaviour-change notes.**
- **W2d: there is no `web` differential row and there cannot be one.** The
  harness runs both legs in this process and the legacy `web` catalog reads and
  writes `$HOME/.storyhook/registry.toml`. See the `TestEnv` trap above. The
  store leg's catalog behaviour is covered by `tests/service_catalog.rs`, which
  never touches `$HOME`.
- **W2d: `commit-sync` still scans commit SUBJECTS only**, reproduced verbatim
  (`git log --format=%H %s`). A commit whose *body* names a story is invisible.
  Widening it changes which stories get comments, which is a behaviour change
  and belongs to the git-features wave, not to a port.

- **W2c: `story doctor --fix` DESTROYS relationships to archived stories.** The legacy
  repair loop asks "does the other end of this edge exist?" of the *open* stories only
  (`app.rs::doctor_fix`, `load_all_open_snapshots`). Relate two stories and delete one —
  an ordinary sequence — and the survivor's edges look dangling, so the repair retracts
  them; it then reports the asymmetry it just created and exits 5, permanently, with the
  data gone. Reproduced by hand on the CLI: `story relate SH-3 blocks SH-4` →
  `story delete SH-3` → `story doctor` clean → `story doctor --fix` → exit 5, SH-4's
  relationships empty, and every later `story doctor` fails. **The store leg deliberately
  does not have it** (`IntegrityService::fix` asks the question of every story; appends
  still go to open ones only), pinned by
  `differential_query.rs::doctor_fix_retracts_edges_to_deleted_stories_in_the_legacy_leg_only`.
  Same call as W2a's burnt story number — a user-visible improvement that belongs in the
  flip's behaviour-change notes rather than arriving as a surprise. **Needs a story.**
- **W2c, FIXED in-wave (`d3ee37c`) — a failed COMMIT poisoned a pooled connection.**
  `SqliteWriteTx::commit` cleared its `open` flag *before* running COMMIT, so the drop
  guard's ROLLBACK was skipped exactly when it was needed. SQLite leaves the transaction
  open when COMMIT fails, and the connection went back to the pool mid-transaction: the
  next caller's `BEGIN IMMEDIATE` failed with `cannot start a transaction within a
  transaction`, describing the *previous* caller's mistake. Reachable in production, not
  only in tests — `defer_foreign_keys = ON` holds referential checks until COMMIT by
  design, so a rejected edge is a commit-time failure and one of them poisoned a
  connection for the life of the process. **Generalize this:** any teardown flag must be
  cleared *after* the operation it describes succeeds, not before. Regression test is a
  conformance case (`a_write_rejected_at_commit_does_not_poison_the_store`), so a second
  engine inherits it; 162 → 163 cases.
- **W2c: three of `doctor`'s legacy findings are UNREPRESENTABLE in the store.** A
  dangling relation needs a foreign key violated; a second parent needs the unique
  `child-of` index violated; a read-model row with no events needs the append-only
  trigger bypassed. The schema refuses each rather than the doctor detecting it
  afterwards — the defect *class* is gone. The checks stay in `compute_integrity_issues`
  because `story show` and `list --flagged` compute the same flags, and because W3's
  importer must be able to report them about *legacy* data. Pinned by
  `service_integrity.rs::the_shapes_doctor_used_to_find_are_now_refused_by_the_schema`.
- **W2c: the doctor's two dimensions must not overlap.** `diff_read_model` also notices a
  relation only one end's history claims, and so does `compute_integrity_issues` (from the
  snapshots, in the legacy wording). Printing both says the same thing twice in two
  vocabularies *and* moves `doctor`'s bytes for a project the legacy path already
  diagnosed. `IntegrityService` therefore builds its drift lines itself rather than
  calling `ReadModelDiff::describe`, covering only `missing_rows`, `extra_rows`,
  `fold_failures` and `divergences` — the question the legacy read model could not ask.
- **W2c: `summary` and `report` compute the ready count two different ways and must
  agree.** `summary` counts every `is_ready` story; `report` counts only the *open* ones.
  `is_ready` is false for a closed story, so the two are equal — but `report_data`'s
  `ready_count` has to be filled in from its own ready set, because the shared `rollup`
  deliberately leaves it at zero. Getting that wrong is invisible in the human rendering
  and visible in `report --html`; the differential row is what caught it.

- **Flake mechanism PROVEN 2026-07-28:** two orphaned `web_test-*` processes (from prior runs)
  held ~100 LISTEN sockets across 19000–19095 on loopback + tailnet IP. Fresh runs restart
  PORT_COUNTER at 19000 → bind conflicts / `wait_for_addr` connect-success hits the ORPHAN's
  server (stale registry) → mass failures (78/139 observed). Orphans killed; clean run =
  139/139 green in 2.97s. Fixed-base counter + connect-as-readiness + no orphan reaping is
  the defect triad. Spotlight marker exists (`$TMPDIR/.metadata_never_index`, Jul 25) and
  mds_stores idle — SH-53 stall NOT active today, but plausibly the original orphan-maker.
- Live dashboard daemon PID on :3456 (loopback + tailnet) is PRODUCTION — never kill it in
  tests; tests must never bind 3456.
- **Orphan-maker identified (W0.1):** `tailnet_identity()` shelled out to `tailscale status
  --json` with an unbounded `Command::output()`, *after* the loopback listener binds. When
  `tailscaled` wedges (repeatedly observed; probes stuck for minutes, orphaned by exited
  servers) the server sits bound-and-silent forever — which is how test binaries survived
  their runs holding ports. Now bounded to 3s + process-group kill (`e8d4cf8`). This is a
  PRODUCTION defect too (`story web start`'s daemon on :3456 would listen and never answer);
  it needs its own story, NOT filed from this worktree because minting an id here collides
  with ids minted in parallel worktrees (see W0.3's `two_worktrees_mint_colliding_ids`).
- **Second finding, unfixed, needs a story:** positional-taking verbs still swallow unknown
  `--flags` as data (`story new --typo x` creates a story titled `--typo x`). Same shape as
  SH-52, different defect; SH-52's fix covers help flags only.
- **W0.3 finding #1, unfixed, needs a story — `story next` is NONDETERMINISTIC.** The
  ready-list comparator is `priority ASC, then created_at ASC` (`src/app.rs:302`, repeated at
  335/664/1033/2326) but `created_at` has **second** precision. Stories created within one
  second tie on *both* keys; the stable sort then falls back to the order the story files
  happened to be read in, so `story next`, `story summary`, `story context` and `story handoff`
  return **different orderings for identical input**. Found the hard way: the golden corpus
  flaked ~1 run in 3 with SH-2 and SH-12 (both `high`) swapping places. This is a production
  defect, not a test artifact — an agent asking `story next` twice can get two different
  stories. `tests/golden_cli.rs` works around it with one `>=1s` sleep placed so that every
  same-priority ready PAIR straddles it (no tie can then form within either half); the sleep
  goes away when the comparator gets a total order.
- **W0.3 finding #2, unfixed, needs a story:** id ordering is inconsistent across commands.
  `list`, `search`, `epic list` and `phase show` sort NUMERICALLY (`sort_story_views` →
  `numeric_story_id`, `src/app.rs:2652`); `graph`, `handoff`, `context` and `summary`'s ready
  list sort LEXICOGRAPHICALLY (`SH-1, SH-10, SH-11, SH-12, SH-2, …`). Frozen as-is in the
  corpus with a `// KNOWN-DEFECT:` comment — the 14-story fixture exists to make it visible.
- **W0.4 finding — FIXED in W0b (`d272a7b`), still needs a story for the record.**
  `story export --json` DOUBLE-ENCODED its document.
  `Invocation::Export` returns `Response::Message(json)` (`src/app.rs:983`), and the `--json`
  renderer puts a `Message` into the envelope's `message` field as a plain string
  (`src/output.rs:170`). So `story export --json` emits `{"message":"{\n \"schema\": 1,…",
  "result":"ok"}` — the whole export document as an ESCAPED STRING that a consumer must parse
  twice. **Verified**: feeding that output to `story import-project` fails with exit 5,
  ``error: missing field `schema` ``. `Response::RawJson` already exists for exactly this
  ("Raw JSON output — bypasses normal envelope wrapping", `src/output.rs:96`) and is what
  `Export` should return. Consequence for the baseline: `golden-export.json` freezes the
  **plain** `story export` document, because that is the form the round-trip test and the
  importer actually consume. **W0b owns this** — it is the envelope wave, and the fix is one
  variant swap plus a golden-corpus update.
  **Fix as shipped:** `Response::RawJson`, one variant swap, plus the golden-corpus update.
  Plain `story export` is byte-for-byte unchanged (`export_document` snapshot untouched). One
  deliberate side effect, pinned by `export_is_not_suppressed_by_quiet`: `RawJson` renders
  ahead of the `--quiet` check, so `story export --quiet` now emits the document where it used
  to emit nothing — correct for a command whose whole output is the result, and previously a
  silent empty-backup footgun. No caller anywhere in the repo uses `export --quiet`.
- **W0b sibling finding, unfixed, needs a story — `context`/`load-context --format json` has
  the same shape as the export defect.** `Invocation::Context` returns
  `Response::Message(json_string)` when `--format json` is given (`src/app.rs:1061`), so the
  *global* `--json` on top wraps that JSON as an escaped string in `.message`. Left unfixed
  deliberately: unlike export, no consumer parses it (agents read it as text), and the golden
  corpus's `narrative_json` freezes the wrapped form. Whoever fixes it should expect
  `golden_cli__narrative_json.snap` to move, and should decide the same `--quiet` question the
  export fix answered. `plugin/claude-code/references/cli-reference.md` now documents the two
  commands separately, since their behavior has diverged.
- **W0.3 finding #3, needs a story (or a deletion):** `AppError::SyncConflict` (exit 8) is a
  **dead variant** — constructed nowhere in `src/`; only `web.rs:161` maps it to HTTP 409.
  `tests/error_contract.rs` lists it in `UNREACHABLE` and covers its exit code at the enum
  level; if a CLI path ever raises it, it needs a real row.
- **The harness lives in `crates/storyhook-test-support/` (W0.2).** `storyhook` is both the
  workspace root package and a member, so `src/`/`tests/` paths are unchanged. The crate is a
  dev-dependency of `storyhook` AND depends on `storyhook` — cargo permits a cycle through a
  dev-dependency, and `cargo publish` strips path-only dev-deps. **Consequence to respect:**
  `src/`'s own `#[cfg(test)]` modules deliberately do NOT use the crate. They can (dev-deps are
  in scope there), but doing so links two copies of `storyhook` into the lib-test binary — the
  cfg(test) one and the plain one the harness depends on — whose types are mutually
  incompatible. Fine for `scratch_dir()`; a trap the day someone passes a storyhook type across.
- **45 files still build fixtures in `$TMPDIR`**, each carrying
  `// TODO(rearch): migrate to storyhook_test_support::scratch_dir` +
  `#![allow(clippy::disallowed_methods)]`. `grep -rl 'TODO(rearch)' tests/ src/` is the live
  migration list; it must only ever shrink. Deliberate: bulk-migrating 45 files here would have
  produced an unreviewable diff and collided with every later wave.
- **W1 finding, fixed in-wave (`aac3a5c`) — a CHECK constraint that evaluates to
  NULL PASSES.** The `stories` table tied `priority_rank` to `priority` with
  `CHECK (priority_rank = CASE priority WHEN 'critical' THEN 0 … END)`, and the
  schema comment claimed this also validated the slug, since an unknown slug
  makes the `CASE` yield NULL. It does — and SQL rejects a row only when a
  constraint is **FALSE**, so `UPDATE stories SET priority = 'urgent'` was
  accepted. Fixed with an explicit `CHECK (priority IN (…))`. **Generalize
  this:** every CHECK in this schema that compares against a possibly-NULL
  expression needs the same audit, and a future migration adding one should
  assume NULL means "allowed" until proved otherwise.
- **W1: `story doctor`'s integrity question is now TWO questions**, and W2c
  should surface both. `ReadModelDiff::is_clean()` = "does the read model match
  its events" (everything it covers is fixable by `repair_read_model`, so
  repair-then-diff is clean by construction). `has_integrity_issues()` = "are
  there problems no re-fold can fix" — today: unfoldable stories, and
  **asymmetric relations**, which is SH-60 at the level it actually lives. The
  relations *table* is symmetric by construction, so queries are unaffected; the
  missing half is a missing *event*, and only the layer that writes events can
  add it. A `--repair` must not claim to fix the second kind.
- **W1: the relations table is the CLOSURE of both ends' claims, not one end's.**
  `put_story` removes an edge row only when *neither* end's snapshot claims it
  (`claimed_by_other_end`). Without that rule, rewriting SH-2 from a history
  that never mentioned SH-1 retracted an edge SH-1 still asserted, and the
  surviving state depended on which story was written first —
  `repair_read_model` never converged. W2a's RelationService must keep writing
  both stories' events; the store guarantees the *table* stays symmetric, not
  that the histories agree.
- **W1: `put_story` derives `archived` from `closed_at`** and a schema CHECK
  ties them, so W2/W3 must not try to set it independently — there is no
  parameter for it and the constraint would refuse.
- **W1: `cargo test` gets the `fault-injection` feature; `cargo build` does
  not** — verified by grepping `cargo build -v`/`cargo test -v` for
  `feature="fault-injection"` (0 occurrences vs 2). The mechanism is
  `crates/storyhook-test-support/Cargo.toml` depending on `storyhook` **with**
  the feature; cargo's v2 resolver keeps dev-dependency features out of
  non-test builds. If someone ever removes that `features = [...]`, the store's
  crash tests silently stop compiling into the gate.
- **W2a finding, unfixed, needs a story — a rejected `--state` BURNS a story number.**
  `storage::create_story_with_events` (`src/storage.rs:936`) calls `next_story_id`, which
  increments the on-disk counter, and validates the requested initial state *afterwards*
  (`:937-956`). So `story new --state nonsense` exits 2 and still consumes the number,
  leaving a permanent gap in the numbering. Every *other* enrichment field (type,
  priority, assignee) is validated in `app.rs` before that call, which is why only
  `--state` shows it. Found by the differential harness on its first full run — the two
  legs disagreed on the id of the next story, and nothing else would have surfaced it.
  **The store does not have the defect** (the allocation is inside the transaction that
  uses it, so a rollback returns the number), so this is a deliberate behaviour change at
  the flip, not a regression. Pinned by
  `differential_lifecycle.rs::a_rejected_initial_state_burns_a_story_number_in_the_legacy_leg_only`;
  W4 should list it in the flip's behaviour-change notes.
- **W2a: `fold_story` was not total, and reopen is why.** `closed_at`, `deleted` and
  `deleted_reason` had events that set them and none that cleared them, so the only way to
  reopen a story was `storage::unarchive_story`'s *rewrite* of the event log with the
  closure markers filtered out. An append-only store cannot do that, and the rewrite
  destroys audit history besides. `845afb9` makes `StoryStateChanged` into a state the
  project defines **and** calls OPEN retract the three flags. Deliberately that narrow: an
  unrecognised slug leaves them alone, so a deleted story whose state slug was later
  removed from the catalog keeps folding (its `deleted` flag is what forces its
  superstate) instead of becoming a hard fold failure. **No reachable legacy log contains
  the ordering it reacts to** — every `StoryStateChanged` writer appends to a story's *open*
  log, and a closed story has no open log until `unarchive_story` has already stripped the
  markers; a scan of this repo's own 61 logs (open + archived) found zero occurrences. Full
  suite green with zero assertion or snapshot edits.
- **W2a: `tx.state_map()` is a `BTreeMap`, so iterating it is ALPHABETICAL, not configured
  order.** The default open state is "the first *configured* state that is OPEN"; taken off
  the map it would be `in-progress` rather than `todo`, and every new story would open in
  the wrong state. Use `tx.states()` (ordered `Vec`) wherever order is part of the
  contract — the default open state, and the "Available OPEN states: …" error message —
  and derive the map from that same list for the fold. `service::story::state_map` does
  exactly this. W2b/c must not reintroduce the shortcut.
- **W2a: `StoreError::Rejected(Box<AppError>)` is the service→store error channel.**
  `Store::write` decides commit or rollback from the closure's `Result`, so a service that
  aborts on its own rules has to express that as a `StoreError`. Flattening into
  `StoreError::Validation` costs the error contract two things it is pinned on: `Usage` and
  `Validation` share an exit code but not a wire form, and `StateConflict` carries two state
  slugs no store-level variant has room for. `From<AppError> for StoreError` wraps and
  `From<StoreError> for AppError` unwraps, so the round trip is the identity — which is why
  services can just use `?` on `Result<_, AppError>` inside a write closure.
- **W2a: `story assign` and `story set --assignee` disagree about a missing member ON
  PURPOSE.** The first is `NotFound` (exit 3), the second `Validation` (exit 2). Both are
  pinned by the error contract and by the differential harness; a later wave "unifying"
  them changes user-visible exit codes.
- **W2a: BulkUpdate stays best-effort per item, contradicting the wave brief's
  "one failure → whole batch rolled back".** The legacy output format has a per-item
  `error — …` line, so a caller can already see which items failed, and making the batch
  atomic would silently discard work a script has been told landed. Byte-compat wins.
  What the store *does* add is that each item is atomic **on its own** — the event append,
  the fold, the snapshot write and the archived flag land together or not at all, where the
  legacy path had three separate filesystem operations and a failure between them left the
  SH-20 split-brain shape. Pinned by `service_story.rs::each_bulk_item_is_atomic_on_its_own`.
- **W2b: a state's superstate is a DEFINITION, and changing one invalidates rows no event
  touched.** A story's `superstate` is folded from the definition of the state it sits in, so
  `story state set <slug> --super …` changes the correct answer for every story in that slug
  without appending anything to any story's history. Open occupants migrate out; **archived
  ones do not**, and the legacy path left their stored snapshots claiming the superstate they
  had when they closed. `ConfigService::update_state` re-folds them
  (`service::refold_story`, the only sanctioned fold-without-append). **This is a deliberate
  divergence from legacy** — reachable only with two CLOSED states, archived stories in one of
  them, and a flip of that one to OPEN — and the reason is that the alternative is a read model
  that no longer equals a fold of its own events, which `story doctor` reports and
  `repair_read_model` then silently changes anyway. Pinned by
  `service_config.rs::flipping_a_superstate_re_derives_the_rows_of_the_stories_left_in_it`,
  which fails if the re-fold is removed. **W4 should list it in the flip's behaviour-change
  notes.**
- **W2b: three grouping commands fire no hooks, on purpose.** `story phase create`,
  `story epic create` and `story epic add` fire nothing, where `story new` and `story relate`
  fire `create` and `relationship_change`. The legacy path got this by writing events directly
  instead of going through the shared paths; `GroupingService` gets it by running those three
  against a **hook-suppressed `Ctx`** (`Ctx::new(...).no_hooks(true)`), so the delegation is
  real and the hook behaviour is unchanged. `story phase add` *does* fire `label_change`, with
  the story re-read after the write; `story phase remove` fires nothing. All four directions
  are pinned by `service_grouping.rs`'s marker-file tests. Unifying them is a user-visible
  change and belongs to whoever decides it is worth making.
- **W2b: `story init` cannot go through `dispatch`.** A `Ctx` names a `ProjectId` and `init` is
  the command that creates one, so on a virgin store there is no id for a context to hold.
  `invoke::dispatch_unscoped(store, root, now, invocation)` is the project-less entry point;
  `dispatch` forwards its own project-less variants (`init`, `help`, the three help topics,
  `version`) to it, so the roster of ported arms stays a property of one function. **W4's root
  resolution should call `dispatch_unscoped` before it tries to resolve a project**, not after.
- **W2b: the pointer file is `<root>/.storyhook.toml`, and nothing writes it yet.**
  `ProjectPointer { schema, uuid, prefix }`, with `service::project::{pointer_path,
  read_pointer, write_pointer}`. `InitOptions::pointer` gates the write and the dispatcher
  leaves it `false`: while `.storyhook/` is still the identity of record, a pointer file is a
  second answer to "which project is this" and the two would disagree the moment either moved.
  **W4 turns it on.** The location is beside the legacy directory rather than inside it, so W7
  can delete `.storyhook/` without moving the pointer. Pinned by
  `differential_config.rs::init_leaves_no_pointer_file_before_the_flip`.
- **W2b: `story init` is idempotent and must stay so.** The legacy path skipped every file it
  found; the store path finds the checkout by canonical path, refreshes its registration, and
  leaves the catalog *and the prefix* alone. `story init --prefix ZZ` on a project created with
  `--prefix AB` is a no-op in both legs.
- **W2b: three template/default pairs now exist twice**, because `src/app.rs` is frozen:
  `generate_agents_md`/`generate_claude_md`/`generate_cursor_rules` versus
  `service::templates`, and `storage::init_project`'s inline default states/types versus
  `service::project::{default_states,default_types}`. **The differential rows are the drift
  guard** — `scaffolding_agrees_byte_for_byte` and `init_agrees_on_the_catalog_it_creates` /
  `init_agrees_on_the_agents_md_it_generates`. W4 deletes the `app.rs` copies.
- **W2b: `story scaffold` in an uninitialized directory diverges after the flip.** The legacy
  arm never calls `ensure_project`, so it falls back to prefix `SH` / state `done`; the
  store-backed arm takes a `Ctx`, which implies a project exists. Not reachable today (nothing
  routes production traffic through `dispatch`); **W4's root resolution decides what
  `story scaffold` does outside a project.**
- **W2b: `git hooks install` is still broken in a linked worktree**, and deliberately so. In a
  linked worktree `.git` is a *file*, so `.git/hooks` cannot be created and the install fails
  with the filesystem's error rather than a diagnosis. `SystemService` calls
  `crate::hooks::install_hooks` rather than reimplementing it, so the behaviour is identical by
  construction; fixing it means resolving `--git-common-dir`, which is the same resolution the
  worktree wave has to build, and doing it twice would leave two answers to one question.
  Pinned as-is by `service_system.rs::installing_from_a_linked_worktree_fails_loudly_rather_than_silently`.
- **W2b: two orderings inherited unchanged, both user-visible.** `story phase list` sorts
  phases by *label text*, so phase `10` comes before phase `2` (a `BTreeMap<String, _>` over
  `phase:<n>`). Stories *within* a phase, and `story epic list`, sort by story **number**
  (`service::view::sort_story_views`), so `SH-10` comes after `SH-2`. Both are named in the code
  so a later wave can change them on purpose rather than by accident. This is the same family as
  the W0.3 finding about lexicographic-versus-numeric id ordering.
- **W2b: `make test` mass-failing in `web_test` is not always an orphan.** A neighbouring
  project's Swift test suite pinning a core (load average 6–9) failed 52/140 `web_test` tests
  on one run; the same tree ran 140/140 green alone minutes later, and `make test` has been
  green on every commit since. Before hunting a regression, check `uptime` and
  `ps aux | sort -nrk 3` — the readiness deadlines in that suite are wall-clock.

- Baseline `make test` cold: 83s wall (230% CPU) incl. build; failed only in web_test (orphans).
- Post-W0.1 `make test` warm: ~29s wall, 3 consecutive green runs (49 Rust targets + 15 bash).
- `git commit --amend` silently fails in this environment — use reset --soft + fresh commit.
- Post-commit hook scans commit SUBJECTS for story IDs — bookkeeping/story refs go in BODIES.
- Worktree branch base: main @ `838d68a`.

## Step log

- 2026-07-28 W0.0: branch `rearch/w0-gate-repair` created off `838d68a`; five stories blocked
  with rearch reasons; committed `be1601c` (subject deliberately ID-free). Task ledger created
  in session task list (#1–#14, spine dependencies wired).
- 2026-07-28 W0.1: five `fix:` commits, each red→green with its own regression test.
  - `2316ddf` SH-53 — `scratch_dir()`/`scratch_root()` in web_test; fixtures now under
    `/private/tmp` (never Spotlight-indexed), all 134 `tempdir()` sites migrated; harness
    also creates `$TMPDIR/.metadata_never_index` for the suites still using `$TMPDIR`.
  - `4c1aed9` SH-51 — `web::start_server_with_ready` reports the *actually bound* address
    (the adopted design rule, arriving early); in-process test servers bind port 0;
    `serve`/`try_serve_on` gate on the server's own ready callback and never swallow a
    start-up error; `DaemonGuard`; `connect_sse` deadline; `scripts/check-no-orphan-servers.sh`
    brackets `make test`. **`pick_port`/`PORT_COUNTER` are gone — new server tests must use
    `serve()`.**
  - `e8d4cf8` — bounded tailnet probe (see Key facts; production defect, needs its own story).
  - `da3109d` SH-59 — errors to stderr via one `fail()` helper. **Ruling: `--json` envelopes
    stay on STDOUT** (stdout is the machine-readable result channel: exactly one
    self-describing document per run; `move_if_state.rs` and `story.sh`'s CAS claim both read
    the conflict envelope there, and stderr also carries free-text hook warnings). 73 existing
    assertions across 18 files flipped to `.stderr(...)`.
  - `e2531c4` SH-52 — help flags answered ahead of every verb parser; `parse_invocation` split
    into a pure `dispatch()` (used for verb recognition, so there is no second verb list);
    `main` no longer launches the TUI for `story tui --help`. 54 verbs × `--help`/`-h` swept.
  - Gate after: `make test` green ×3, ~29s warm; web_test 143/143 (~3.5s).
  - For W0.2: the `scratch_dir`/`serve`/`DaemonGuard`/orphan-check helpers are deliberately
    self-contained in `tests/web_test.rs` + `scripts/`, ready to move into the test-support
    crate unchanged.
- 2026-07-28 W0.2: five commits, `make test` green after each; two consecutive green full runs
  at the end (27s, 28s warm — the pre-step baseline was ~29s, so the workspace costs nothing).
  - `877c31f` — 2-member cargo workspace + the crate + 23 unit tests of the harness itself.
  - `a170106` — `scratch_dir`/`reserve_port`/`serve`/`try_serve_on`/`DaemonGuard`/`ChildGuard`/
    `wait_for_*`/`http_status_line` moved out of `web_test.rs` unchanged; the three tests that
    test *those* moved with them. `web_test.rs` 143→140; the crate holds 23 unit tests + 1
    doctest, unchanged across the move because `877c31f` already wrote the crate with those
    three in place. (`a170106`'s commit body miscounts this as "crate 23 → 26"; the crate was
    23 throughout.)
  - `c444313` — `move_if_state.rs`, `registry_test.rs`, `story_export.rs` migrated. Zero
    assertion changes: `git show -U0 c444313 -- tests/ | grep '^[-+].*assert'` yields 7 removed
    lines, all `story init` fixture setup or the hand-resolved `cargo_bin` path, and nothing
    added.
  - `f4eefe1` — bash suite data-home isolation (the plan's highest-severity risk).
  - `ca2b4a7` — `clippy.toml` `$TMPDIR` ban + the 45 TODO(rearch) markers.

  **API surface W0.3 builds its RED `two_worktrees_mint_colliding_ids` on** (all of
  `storyhook_test_support::*`, and `--workspace` is now required on cargo invocations):

  ```rust
  TestEnv::shared() -> &'static TestEnv     // one per test binary, the default
  TestEnv::isolated() -> TestEnv            // when a test asserts on env contents
    .story(cwd) -> assert_cmd::Command      // replaces every `fn story(dir)` helper
    .raw_story(cwd) -> std::process::Command// for tests that spawn+race processes
    .apply(&mut std::process::Command)      // isolate an arbitrary command (git, env…)
    .project() -> ProjectBuilder<'_>
    .home()/.data_home()/.config_home()/.state_home()/.data_dir() -> &Path
    .vars() -> [(&'static str, &Path); 5]

  ProjectBuilder: .prefix(&str) .git() .with_local_origin() .worktree(&str)
                  .seed_story(&str) .build() -> Project<'a>
  Project: .path() .env() .origin_path() .worktree_path(name)
           .story() -> assert_cmd::Command
           .run(&[&str]) -> assert_cmd::assert::Assert
           .json(&[&str]) -> serde_json::Value   // appends --json, asserts success
           .new_story(title) -> String           // returns the minted id

  scratch_dir() / scratch_dir_named(label) / scratch_root()
  serve(&Path) -> u16 · try_serve_on(&Path, u16) -> Result<u16, String>
  reserve_port() · wait_for_server(u16) · wait_for_addr(&str)
  http_status_line(u16, Duration) -> Option<String>
  DaemonGuard::new(home, cwd) · ChildGuard::new(child) · story_binary() -> &'static Path
  ```

  For the collision test specifically: `env.project().worktree("a").worktree("b").build()`
  gives a repo with two linked worktrees in the exact `story.sh dispatch` shape
  (`.claude/worktrees/<name>` on branch `worktree-<name>`); drive each with
  `env.raw_story(project.worktree_path("a"))` so both can be spawned before either is waited on.
  `TestEnv` NEVER mutates the current process's environment — tests in a binary are parallel
  threads, and `registry_test.rs` legitimately asserts against the real `$HOME`.

  Two things W0.2 changed that are worth knowing, neither a defect:
  - `scratch_dir()` now roots fixtures at `<tmp>/storyhook-tests/` and sweeps its own
    prefix-matching entries older than 6h. Required because `TestEnv::shared()` parks a
    `TempDir` in a `OnceLock` and Rust never drops statics.
  - Dropping lib.sh's `git add .storyhook` slightly weakens `test-dispatch-cwd.sh`: its linked
    worktree now resolves *no* tracker where it used to resolve a *different* one. Same
    anchoring property, weaker fixture. Its comment was corrected to say so. **W7 (repo
    cutover) should restore the stronger form** once the global store makes it expressible.
- 2026-07-28 W0.3: three commits, `make test` green after each; two consecutive green full runs
  at the end (37.6s, 37.4s warm — the pre-step baseline was ~29.4s, so this step costs ~8s, of
  which 5s is one unavoidable lock deadline; see below).
  - `9aa8df1` — `tests/worktree_truth.rs`: the headline RED pair, `#[ignore]`d.
    **Removing the two `#[ignore]` attributes is W4's exit criterion.** The fixture uses
    `env.project().git().worktree("a").worktree("b").build()` and then *commits* `.storyhook/`
    and fast-forwards both worktrees onto that commit — without that step the worktrees resolve
    no tracker at all (exit 3, "not initialized"), which is a different failure from the
    silently-divergent one under test; a fixture assertion pins it.

    **Captured red evidence** (`cargo test --workspace --test worktree_truth -- --ignored`):

    ```text
    ---- two_worktrees_of_one_repo_mint_colliding_ids stdout ----
    assertion `left != right` failed: two checkouts of one repository must not mint the
    same story id; both `story new` calls returned SH-2.
      left: "SH-2"
     right: "SH-2"

    ---- a_story_created_in_one_checkout_is_visible_from_the_other stdout ----
    a story created in worktree `a` must be visible from worktree `b` — they are one
    project. `story show SH-2` in b exited with code 3 and said: error: story `SH-2` not found
    ```

    They collide at `SH-2`, not `SH-1`, because the fixture seeds one story *before* the
    split — proof the counters start shared and then drift, rather than never having agreed.
  - `b7539f3` — `tests/golden_cli.rs` + `tests/snapshots/`: **177 invocations, 27 snapshots,
    +2.3s.** ~all read-surface commands in human and `--json` form, plus 24 error cases in
    both. Grouped one snapshot per family per form with each invocation labelled inside
    (`$ story list --state todo`) — 177 single-invocation files would be unreviewable and a
    grouped diff still names the invocation that moved. Error snapshots record exit code +
    stdout + stderr, which pins SH-59's stream ruling. **`INSTA_UPDATE=no` is now in the
    Makefile's test target**; proved it gates by perturbing a snapshot (run failed, no
    `.snap.new` written). `*.snap.new` gitignored. **Snapshot tests live in this file only.**
  - This commit — `tests/error_contract.rs` (all 10 `AppError` variants: exit code, stream
    placement, envelope key set, in both forms) and `export_import_export_is_byte_identical`
    in `tests/story_export.rs`. The round trip is **byte-identical with no redaction at all**
    (import replays stored events verbatim rather than re-stamping them), asserted twice so a
    first-pass loss cannot hide behind later stability. This is W3's importer oracle.

  Things the next steps should know:
  - **The error table costs ~5s** and cannot be made cheaper without a production change:
    `with_project_lock` polls a hard-coded 5s deadline (`src/lock.rs:23`) with no env override,
    so provoking a real `LockTimeout` takes that long. The two output forms are provoked
    concurrently so the row is paid once rather than twice (11.1s → 5.9s). If W5 makes the
    deadline configurable, this drops to ~1s.
  - `storyhook::lock::with_project_lock` is public, so a test can hold the real lock from its
    own process; the LockTimeout row uses a channel to prove the lock is *held* before the
    child starts, rather than hoping.
  - `AppError` exhaustiveness is enforced by a `match` in `error_contract.rs::variant_name`:
    an 11th variant stops that file compiling until it is given a row or listed unreachable.
  - The corpus fixture is built entirely through **public CLI verbs**, deliberately — a fixture
    built by importing a legacy export document would stop being constructible at the W4 flip.
  - Three defects the corpus and the table surfaced are in Key facts above (nondeterministic
    `story next`; lexicographic-vs-numeric id ordering; dead `SyncConflict`). None were fixed:
    this step ships `test:` commits only.
- 2026-07-28 W0.4: two commits — `4179f48` (the script) and this one (the artifacts).
  `docs/rearch/baseline/` is the pre-rearch reference point; regenerate at any wave boundary
  with `scripts/capture-baseline.sh` and diff. `--skip-census` gives everything but the 10
  gate runs in ~1min; `--out DIR` captures elsewhere for an A/B.

  **Census: 10/10 green, and 0 tests were anything other than `ok` in any run.** That is a
  per-TEST verdict, not per-run: every `test NAME ... ok` line is parsed out of each run's log
  (1170 per run — 1171 Rust tests minus the 2 ignored, plus 1 doctest) and any test not clean
  in all 10 is named. All ten runs also reported *identical* counts (rust 1170/0/2, bash 16/0),
  so there is no count drift hiding behind a green verdict either. **No ordering flake appeared**
  — the `story next` nondeterminism stayed latent, as the golden corpus's `>=1s` sleep intends.

  **Numbers to diff against later** (Psamathe, M1 Max, rustc 1.97.1 — absolute seconds compare
  only on this machine; ratios compare anywhere):
  - gate median **36.4s** over 10 runs (36.14–37.66, i.e. ±2%)
  - inventory **1171 Rust tests / 52 binaries / 2 ignored**, 1 doctest, 16 bash files
  - per-binary serial sum 19.1s; `web_test` 7.17s (140 tests), `error_contract` 5.22s (3 tests,
    ~5s of it the one real LockTimeout), `golden_cli` 1.64s (27 snapshots). Everything else is
    under 0.9s.
  - legacy tree 26 files / 222093 bytes; 61 stories / 486 events / 44 archived

  Things the next steps should know:
  - **Every bare test name is unique workspace-wide**, and the script asserts it. That is the
    only reason the census can attribute a bare `test NAME ... ok` line — cargo never names the
    binary on those lines. If a future duplicate appears, the inventory says so under Totals
    and the census's per-test table silently merges the two.
  - **The known-red count is asserted**, not reported: the script scrapes `#[ignore]` reasons
    from source and fails if the count disagrees with `--list --ignored`. An `#[ignore]` cannot
    enter the tree without appearing in `known-red.md`.
  - **`error-codes.md` is extracted from `tests/error_contract.rs`**, not transcribed from it,
    and the extractor fails loudly if that file's shape changes. It cannot drift.
  - **The legacy fixture was verified, not just archived**: restored → `story list` works →
    its `story export` is byte-identical to `golden-export.json` → `import-project` + re-export
    is byte-identical again. That third property is W3's target.
  - **What the legacy fixture does NOT cover:** its 5 states and 5 types are the *defaults* and
    `members` is **empty**. Custom states, custom types and members are covered only by
    `story_export.rs::export_import_export_is_byte_identical`'s synthetic project (spike/review/
    Ada Lovelace). **W3 needs both fixtures**; neither alone is sufficient.
  - Two capture bugs were found and fixed before the artifacts were trusted, both of the
    silent-wrong-answer kind: cargo omits `@version` from `package_id` when a package name
    matches its directory, and jq's array construction drops an ELEMENT when `capture` matches
    nothing — which reported `storyhook-test-support`'s 23 tests as zero. A field-count
    assertion and an `-x "$exe"` check now refuse to let either pass quietly.

- 2026-07-28 W0.5: three docs commits, then the PR. No `src/`/`tests/`/`crates/` changes.
  - **The spec of record is now in-repo** at `docs/spec/data-layer-rearchitecture.md`; this
    file's header points there instead of `~/.claude/plans/`. The adaptation dropped the
    plan-approval scaffolding and folded in the plan's own superseding rulings (the W0 row now
    names `storyhook-test-support`, not the abandoned `tests/support/mod.rs` sketch). Design
    changes go there; execution state stays here.
  - **The flip checklist corrected two of the plan's estimates**, both upward:
    - `.storyhook` path refs: plan said ~85 across 18 files; actual **104 across 26 files**
      (99 in `tests/`, 5 in the support crate). The two named hot spots were right
      (`init_command.rs` 20, `session_start.rs` 14).
    - Raw-state fabricators: plan said ~8 tests writing raw JSONL. **8 tests is right, but they
      occupy 10 sites**, and two corrupt by *deleting* (a directory, a `project.toml`) and one
      by writing TOML — so `inject_events()` needs a raw-bytes form and a delete form, not just
      `Vec<StoryEvent>`. The plan's named files were partly wrong: `cli_grammar.rs`,
      `story_delete.rs` and `story_state_archive.rs` only *assert on* the JSONL, they do not
      fabricate; the real fabricators are `doctor.rs`, `error_contract.rs`, `session_start.rs`
      and `tui_integration.rs`.
  - **New finding the plan did not enumerate: 85 white-box call sites** into
    `storyhook::{storage,lock,registry}` + `ProjectPaths` across 6 files — APIs W4 *deletes*, so
    these are hard compile breaks, not silent ones. **70 of the 85 are in `tui_integration.rs`
    (50) and `tui_undo.rs` (20), which W2c already owns.** If W2c ships without porting them,
    W4's budget grows by the largest single chunk in the checklist. Flagged in the checklist's
    category C.
  - Two decisions the checklist refuses to make on W4's behalf, both user-visible: where the
    scaffolded `.storyhook/CLAUDE.md` goes (23 refs depend on the answer), and whether
    user-authored `plugin-config.toml`/`hooks.toml` stay in the repo (15 refs). Also flagged:
    `help_flag_sweep.rs`'s tree-fingerprint (`:135`) becomes a tautology over an empty directory
    unless it is re-pointed at the store — **a green tautology there re-opens SH-52**.
  - `TODO(rearch)` migration list: **45 files** (42 in `tests/`, 3 in `src/`), unchanged by this
    step.
- 2026-07-28 W0b: branch `rearch/w0b-envelope` off W0's tip `c01c116` — **deliberately stacked**,
  because W0's PR #60 was still open. Three commits, `make test` green after each, two
  consecutive green full runs at the end: **39.7s and 39.6s warm**, against W0's 36.4s median —
  +3.3s, of which 0.9s is the two new test binaries running (`invoker_seam` 0.88s,
  `wire_envelope` 0.00s) and the rest is the extra link/startup of two more binaries. Test count
  1170 → **1185** (+9 `wire_envelope`, +4 `invoker_seam`, +2 `story_export`).
  - `d272a7b` — the export fix (see Key facts for the shipped behavior, including the `--quiet`
    side effect and the sibling that was left alone).
  - `2db8310` — the envelope. `Deserialize` on `Response` + `StaleInfo`/`StoryView`/
    `SummaryView`/`ReportData`/`GraphView`/`BlockedChainView`/`GraphOverview`/`PhaseView`;
    `Serialize + Deserialize` on `Invocation`, `CliOptions`, `MemberInput` and the six action
    enums; `WireError` in `src/error.rs`. `tests/wire_envelope.rs`, 9 tests, +0.01s.
  - `ef717f2` — the seam. `src/invoke.rs`; `main.rs` and `web.rs`'s three dispatchers adopt it.
    `tests/invoker_seam.rs`, 4 tests, +0.9s. **Zero assertion or snapshot edits** —
    `git diff --stat ef717f2^ ef717f2 -- tests/` reports one file, the new one.

  **The API W1/W2a/W5 build on** (`storyhook::invoke`):

  ```rust
  pub struct InvokeRequest { pub invocation: Invocation, pub no_hooks: bool }  // #[non_exhaustive]
  impl InvokeRequest {
      pub fn new(invocation: Invocation) -> Self;      // no_hooks: false
      pub fn no_hooks(self, no_hooks: bool) -> Self;   // #[must_use] builder
  }
  pub trait Invoker { fn invoke(&self, request: InvokeRequest) -> Result<Response, AppError>; }
  pub struct LegacyInvoker<'a>;  impl<'a> LegacyInvoker<'a> { pub fn new(root: &'a Path) -> Self }
  ```

  `InvokeRequest` is `#[non_exhaustive]` **on purpose**: W4 adds cwd/project selection and W5
  adds `hook_depth`, and construction must keep compiling. Build it with `new()`, never a struct
  literal — a literal will not compile outside the crate anyway, which is the point.

  Things the next steps should know:
  - **`json`/`quiet` do not cross the seam and must not start.** `app::run` reads only
    `options.no_hooks` and `options.invocation` off `CliOptions` — verified, not assumed — so
    `LegacyInvoker` fills the other two with `false` and nothing observes it. This is the
    structural reason the rearch's byte-compat argument holds: rendering is a *client* concern
    applied to a transported envelope, and `CliOptions` now says so in its doc comment.
  - **The `WireError` design choice, so W5 does not relitigate it:** a mirror enum, internally
    tagged `{"kind": "state_conflict", …}`, **not** serde on `AppError`. `AppError` is a
    `thiserror` type whose `Display` is the CLI's user-facing contract and whose `From` impls
    pull in `std::io::Error`/`rusqlite::Error`; deriving on it would either freeze the enum as a
    public data format or punch `#[serde(skip)]` holes into the error path. Each variant carries
    its payload by name (`detail`, or `expected`/`actual`), **never the rendered message** —
    `GithubAuth`'s `Display` is `github auth: {0}`, so a message-carrying wire form doubles the
    prefix per hop. **Message and exit code are not transported**: they are recomputed from the
    reconstructed `AppError`, so a transported copy cannot disagree with the real value. If W5's
    HTTP envelope wants them visible to `curl`, it should emit them *alongside* a `WireError`,
    derived from the same value — not inside it.
  - **`Response`'s serde form is externally tagged snake_case** (`{"story": {…}}`) and is
    deliberately NOT the `--json` envelope `render_json` emits. Two different formats with two
    different audiences: transport vs. a human's `jq`. Nothing serialized `Response` before this
    commit, so there was no compatibility surface to preserve.
  - **The round-trip tests were verified to bite**, not merely to pass: reverting one
    `#[serde(default)]` on `StoryView::flagged_reasons` fails two of them with ``missing field
    `flagged_reasons` ``, and making `WireError::GithubAuth` carry the rendered message fails
    with "`GithubAuth` came back with a different message".
  - **Two exhaustiveness guards now exist in `tests/wire_envelope.rs`**, both of the
    stops-compiling kind: `variant_name` over `AppError` (10) and `invocation_name` over
    `Invocation` (46), each paired with a corpus-count assertion so a *name* alone is not enough
    — a value has to round-trip. Adding an `Invocation` variant therefore costs a corpus row.
  - `tests/invoker_seam.rs` calls `storyhook::app::run` in-process alongside
    `storyhook_test_support`. Safe — integration tests link ONE copy of `storyhook`. The
    two-copies trap in Key facts applies only to `src/`'s own `#[cfg(test)]` modules.
- 2026-07-28 W1: branch `rearch/w1-store` off merged main `8ffee70`. Six commits, `make test`
  green after each; two consecutive green full runs at the end: **55.8s and 56.5s warm**,
  against W0b's 39.7s. Test count 1185 → **1422** (+237: 162 conformance, 32 rebuild/faults,
  15 migrations, 8 property, 2 schema fixture, 17 `src/store` unit, 1 doctest).

  **The +16s is build, not tests.** Warm `cargo test --workspace` alone is 36.0s, of which the
  five new store binaries contribute **2.2s of runtime** (conformance 0.87s, properties 1.06s,
  rebuild 0.18s, migrations 0.07s, fixture 0.01s) — inside the "<5s added" budget. The rest is
  linking five more test binaries plus compiling `proptest`, the same per-binary cost W0b
  measured at ~1.2s each. If a later wave wants it back, merging `store_schema_fixture.rs` into
  `store_migrations.rs` is the cheapest binary to remove. Note `web_test` measured 18.4–21.4s across
  these runs against a 7.2s baseline: that is parallel-binary contention, not a W1 regression —
  it is unchanged code, and it varied by 3s between two back-to-back runs of the same tree.

  - `ebb7522` — `PartialEq`/`Eq` derives on `StoryEvent`, `StorySnapshot`, `StoryComment`,
    `StateDef`, `TypeDef`, `Member`. **The only change this wave makes outside `src/store/`**
    besides one `pub mod store;` line, and it is a derive-only diff.
  - `a7a8cc6` — schema v1, the migration framework, the SQLite engine.
  - `33c9454` — the rebuild-diff oracle and the fault-injection call sites, plus the
    relation-convergence defect its tests found (see Key facts).
  - `aac3a5c` — the priority-slug constraint (see Key facts) and five raw-connection
    constraint tests.
  - `e792a3e` — `store_conformance_suite!`, 162 cases, instantiated for SQLite.
  - `9f7d0e4` — five proptest properties, `proptest` added as a workspace dev-dependency.

  **The public store API W2a builds on** (`storyhook::store`):

  ```rust
  // Engine
  SqliteStore::open(path) / ::open_with(StoreConfig) -> Result<Self, StoreError>
  StoreConfig { db_path, backup_dir, pool_size, busy_timeout }

  trait Store: Send + Sync + 'static {          // generic S: Store, never dyn
      type ReadTx<'a>: ReadOps;  type WriteTx<'a>: WriteOps;
      fn read<T>(&self, f: impl FnOnce(&Self::ReadTx<'_>) -> Result<T, StoreError>) -> …;
      fn write<T>(&self, f: impl FnOnce(&mut Self::WriteTx<'_>) -> Result<T, StoreError>) -> …;
      fn migrate(&self) -> Result<MigrationReport, StoreError>;
  }

  trait ReadOps {   // EVERY method takes ProjectId — a missing scope is a compile error
      project / project_by_uuid / project_by_slug / project_by_path / projects / project_paths
      states / state_map / types / members / settings
      events_for / head_seq / events_since(after: GlobalSeq, limit: u32) / max_global_seq
      story / stories(&StoryQuery) / relations_from / relations_to / github_base
  }
  trait WriteOps: ReadOps {
      create_project(&NewProject) -> ProjectId · touch_project_path(.., PathKind)
      allocate_story_no(ProjectId) -> StoryNo
      append_events(.., ExpectedSeq, &[StoryEvent]) -> EventSeq
      append_raw_events(.., ExpectedSeq, &[RawEvent]) -> EventSeq   // W3's importer
      put_story(ProjectId, &StorySnapshot, head: EventSeq)
      put_states / put_types / put_member / remove_member / put_settings / put_github_base
  }

  // Newtypes: ProjectId, StoryNo (::parse_id(prefix, "SH-1") / .to_id(prefix)),
  //           EventSeq (::ZERO), GlobalSeq (::ZERO), ExpectedSeq::{Any, Exact}, PathKind
  // Values:   ProjectRecord, NewProject, ProjectPathRecord, ProjectSettings, StoryRow,
  //           StoryQuery (builder) + StorySort, RelationEdge, StoredEvent/StoredPayload,
  //           FeedEvent, RawEvent, MigrationReport, UnknownEventDiagnostic
  // Oracle:   rebuild_read_model / diff_read_model / repair_read_model / ReadModelDiff
  // Errors:   StoreError, with `impl From<StoreError> for AppError` already written
  ```

  **The call pattern W2a must follow** — the store deliberately does not fold, so this is
  visible at every call site rather than hidden behind a storage method:

  ```rust
  store.write(|tx| {
      let head = tx.append_events(project, story, expected, &events)?;
      let stored = tx.events_for(project, story)?;          // includes what was just written
      let (known, unknown) = partition_known(story, &stored);
      let snapshot = domain::fold_story(&story.to_id(&prefix), &known, &tx.state_map(project)?)?;
      tx.put_story(project, &snapshot, head)?;              // same transaction
      Ok(())
  })
  ```

  Things W2a/W2b/W3 should know:
  - **`StoreError` already maps onto `AppError`** (`Conflict→StateConflict`/9,
    `Busy→LockTimeout`/4, `Invariant|Corrupt→Integrity`/5, `Validation`/2) and the conformance
    suite asserts the exit code. Services should propagate `StoreError` with `?` rather than
    re-deriving a mapping.
  - **`put_story` validates the snapshot's `id` against the project's prefix.** A snapshot
    cannot be filed under the wrong project, and a relation naming a foreign prefix is a
    `Validation` error. W3's importer must therefore renumber *before* writing, not after.
  - **`ExpectedSeq::Exact(EventSeq::ZERO)` means "this story must not exist yet"** — the
    create path's CAS.
  - **A rolled-back allocation returns the number** (no gaps). Anything relying on gaps to
    detect lost work will not find them.
  - **The store holds no clock** except `project_paths.last_seen_at` and
    `schema_migrations.applied_at`. Every user-visible timestamp arrives as a parameter, so the
    injectable `Clock` belongs in W5's `Environment` and needs no store change.
  - **`tests/store_support/mod.rs`** holds the fixture helpers (`new_store`, `seed_project`,
    `create_story`, `append_and_fold`, `link_atomic`, `raw`). `link_atomic` is the one-transaction
    relation write W2a's RelationService should mirror.
  - **`tests/fixtures/schema/v1.db`** is a committed schema-v1 database, checked against a
    freshly-migrated one on every run. **Once this wave merges, migration 0001 must never be
    edited again** — a schema change becomes migration 0002. It was edited in place inside this
    wave (`aac3a5c`) only because v1 had not shipped, and the fixture check is what caught it.

  Deviations from the wave brief, all deliberate:
  - **The conformance suite is not feature- or cfg-gated.** It is a `macro_rules!` definition
    plus a four-method trait, both of which generate no code, so nothing reaches a release
    binary anyway — and gating would have split the crate's feature surface for no gain.
  - **A fifth fault point, `backup_verify`**, beyond the four the brief named. Testing "refuse
    to migrate when the backup cannot be verified" otherwise means hand-corrupting a SQLite
    file, which tests the corruption rather than the refusal.
  - **`stories` carries `priority_rank` alongside `priority`**, and the priority index is on the
    rank. `ORDER BY priority` on the slug is alphabetical, which is wrong; a CHECK ties the two.
  - **`put_story` owns relations; there are no `add_relation`/`remove_relation` ops.** The brief
    left the symmetry mechanism to this wave's judgement: both directions are stored, only one
    is ever written, and triggers materialize the mirror from a `relation_inverses` vocabulary
    table that a foreign key also validates against.
  - **`append_raw_events` + `RawEvent` added to `WriteOps`.** W3 needs it for a byte-identical
    legacy round trip, and it is the only honest way to write the unknown-kind tests.
  - **Migration/backup and raw-SQL corruption cases live outside the conformance suite**
    (`tests/store_migrations.rs`, `tests/store_rebuild.rs`): they are properties of an engine,
    not of the contract a second engine would have to satisfy.

  Verification that the tests bite, not just pass: reverting the single-parent index to
  non-unique, and emptying the relation mirror trigger, each fail the conformance case that
  names them; making `put_story` drop one label fails
  `the_read_model_always_equals_a_rebuild`, which shrinks to a two-operation counterexample
  naming the field.

- 2026-07-28 W2a: branch `rearch/w2a-lifecycle` off merged main `271c7cf`. Four commits,
  `make test` green after each; two consecutive green full runs at the end (see below).
  Test count 1422 → **1575** (+153: 86 `service_story`, 24 `service_relations`, 39
  `differential_lifecycle`, 4 `domain` fold units). `src/app.rs` has **zero** changes:
  `git diff 271c7cf..HEAD -- src/app.rs` is empty, and `git diff --stat 271c7cf..HEAD -- tests/`
  reports only the three new files.
  - `845afb9` — `fold_story` retracts the closure markers on a move into an open state
    (see Key facts). The one change outside `src/service/` besides `pub mod service;` and
    the `StoreError::Rejected` variant.
  - `c67642b` — `Ctx`, `Clock`, `invoke::dispatch`, `service::view`, `StoryService`,
    `ServiceFixture` + the drift guard.
  - `fa4d610` — `RelationService`: both ends' events, both folds, both rows in one tx.
  - `757bc7b` — the differential harness, and the legacy id-burn defect it found.

  **The service API W2b/W2c/W2d build on** (`storyhook::service`):

  ```rust
  Ctx::new(&store, project, cwd)            // borrows the store; rebuild per invocation
      .no_hooks(bool) .hook_depth(u32) .clock(Clock)     // #[must_use] builders
      .store() .project() .cwd() .depth() .now() .hooks_enabled()
      .story_view(id) -> Result<Response, AppError>      // fresh read tx, post-hooks
  enum Clock { System, Fixed(String) }       // W5's Environment absorbs this

  StoryService::new(&ctx)
      .create(&NewStoryInput) -> StorySnapshot
      .comment / .assign / .set_priority / .set_awaiting / .clear_awaiting -> StorySnapshot
      .set_labels(id, &[add], &[remove]) -> StorySnapshot
      .set_state(id, state, comment: Option<&str>, if_state: Option<&str>) -> StorySnapshot
      .set_fields(id, &FieldEdits) -> String        // the "updated SH-1: …" summary
      .bulk_update(&[(id, state)]) -> String
      .delete(id, reason) -> String
      .reopen(id, force) -> ReopenOutcome::{Reopened(Box<StorySnapshot>), Aborted(String)}
  RelationService::new(&ctx)
      .relate(a, relation, b, remove)
          -> RelationOutcome::{Changed(Box<StorySnapshot>), Unchanged { remove }}

  // pub(crate), for the services W2b/c/d add:
  service::resolve_story / resolve_open_story / project_prefix / append_and_fold
  service::view::{story_map, story_views, story_view}   // pub
  ```

  Things W2b/W2c/W2d should know:
  - **`append_and_fold` is the mandated store pattern written once.** Call it *inside* your
    own `store.write(|tx| …)`, so the "one transaction" part stays visible at your call site
    while the four-line append/re-read/fold/put boilerplate does not get copied a ninth time.
  - **Hooks fire after the commit, never inside it.** A hook shells out to `story`, which
    opens a second connection; firing inside the write transaction is a deadlock with a
    five-second fuse. `Ctx::story_view` is deliberately a *separate* read tx taken after the
    hooks, because the legacy path built its view after firing and a hook can write.
  - **`service::view` is the minimal read side this wave needed** — `story_map`,
    `story_views`, `story_view`, mirroring `app::build_story_views` minus its
    open-and-archive duplicate check (not representable: a story is one row). **W2c's
    QueryService should absorb and extend it, not duplicate it.**
  - **`ServiceFixture`'s `Drop` runs `assert_no_drift`** (skipped while panicking), so every
    service test in every later wave is also a read-model consistency check for free. Two
    `should_panic` tests in `service_relations.rs` damage a read model through a second
    rusqlite connection to prove the guard is not vacuously green.
  - **The differential harness normalizes exactly one thing: timestamps**, and says so in
    its module docs. Both legs read the system clock at second precision microseconds apart.
    Everything else — ids, states, comments, relationships, derived relationships, progress,
    flagged reasons, error variant, error message, exit code — is compared verbatim.
    `the_ported_arms_are_exactly_the_ones_this_wave_claims` holds the roster of ported arms,
    so **W2b/c/d must update that list as they port**, and an accidentally un-ported arm
    fails there.
  - **`story show` has no dispatch arm yet**, so the harness's view assertions call
    `Ctx::story_view` directly. W2c porting `Show` should point them back through dispatch.

  Gate: **two consecutive green full runs, 62.7s and 62.5s warm** — against W1's 55.8s/56.5s,
  so **+6.5s**. Of that, the three new binaries contribute **2.0s of runtime**
  (`service_story` 0.46s, `service_relations` 0.27s, `differential_lifecycle` 1.31s) and the
  rest is linking three more test binaries, the same ~1.2s-per-binary cost W0b and W1 both
  measured. Well inside the 180s ceiling. **Mid-wave runs measured ~2:07 and are not the
  steady-state number**: touching `src/domain.rs` invalidates every downstream binary, so any
  commit in this wave that changed it paid a full rebuild. `web_test` again measured ~21s
  against its 7.2s baseline — unchanged code, parallel-binary contention, as in W1.

  Verification that the tests bite, not merely pass: the differential harness found the
  legacy id-burn defect on its first full run (Key facts); deleting the `progress` rollup
  from `service::view` fails five of its tests; and the two drift-guard `should_panic`
  tests fail if the guard is removed.

  Deviations from the wave brief, all deliberate and all recorded above or in Key facts:
  - **The dispatch skeleton is not its own commit.** Its helpers (`resolve_story`,
    `append_and_fold`, …) exist only for the services that consume them, so a
    skeleton-only commit fails `clippy -D warnings` on dead code. Adding `#[allow]` in one
    commit to delete it in the next is worse than one honest commit; it is folded into
    `c67642b`.
  - **BulkUpdate is not atomic across items** — see Key facts. The brief asked for
    "one failure → whole batch rolled back"; that would change user-visible output, and
    byte-compat is the higher rule.
  - **`fold_story` changed**, which is outside `src/service/`. Reopen is not expressible
    append-only without it; the alternative was dropping a named deliverable. Its own
    commit, its own tests, zero assertion edits elsewhere.
  - **`StoreError` gained a variant.** W1's store is otherwise untouched.
  - **A `Clock` on `Ctx`.** The spec puts the injectable clock in W5's `Environment`; the
    service tests need a pinned "now" three waves earlier, and 20 lines now beats an
    untestable service.

- 2026-07-28 W2b: branch `rearch/w2b-config` off merged main `67b516a`. Five commits,
  `make test` green after each; two consecutive green full runs at the end (see below).
  Test count 1575 → **1716** (+141: 19 `service_project`, 49 `service_config`,
  18 `service_system`, 16 `service_grouping`, 39 `differential_config`). 2 ignored,
  unchanged — still the W4 headline REDs. `src/app.rs` has **zero** changes
  (`git diff 67b516a..HEAD -- src/app.rs` is empty) and `tests/snapshots/` is byte-unchanged.
  - `4056817` — `ProjectService`, `service::templates`, `invoke::dispatch_unscoped`,
    the pointer file, the `uuid` dependency.
  - `afa2bca` — `ConfigService`; `state_transition_events` becomes `pub(crate)`;
    `service::refold_story` joins `append_and_fold`.
  - `b4e7598` — `SystemService`; the five text-only arms; the roster test gains its
    46-variant completeness assertion.
  - `1354798` — `tests/differential_config.rs` (31 rows at that commit) and the shared
    harness extracted to `tests/differential_support/mod.rs`.
  - `ddfe113` — `GroupingService` (phases and epics), `view::sort_story_views`.

  **Ported-arm roster: 13 → 27.** Added this wave: `init`, `state`, `type`, `member-add`,
  `scaffold`, `hooks`, `plugin`, `phase`, `epic`, `help`, `help-topic`, `help-compact`,
  `help-all`, `version`. **Remaining 19**, all in `unported_probes()`:
  `list`, `show`, `search`, `next`, `summary`, `report`, `graph`, `context`, `handoff`,
  `doctor` (W2c); `import`, `import-project`, `export`, `decompose`, `commit-sync`,
  `github-sync`, `update`, `web`, `session-start` (W2d/W3/W5). The roster test now asserts
  `ported + probes == 46`, so an arm cannot be ported — or added to `Invocation` — without
  landing on one of the two lists.

  **The service API W2c/W2d build on** (`storyhook::service`):

  ```rust
  ProjectService::new(&store, root).clock(Clock)          // NOT Ctx-based: init has no project
      .init(&InitOptions { prefix, agents_md, pointer }) -> InitOutcome
  service::project::{DEFAULT_PREFIX, default_states, default_types, closed_state,
                     ProjectPointer, pointer_path, read_pointer, write_pointer}

  ConfigService::new(&ctx)
      .list_states() -> Vec<StateListing>          // StateListing { state, usage }
      .add_state(slug, SuperState, role, description) -> StateDef
      .update_state(slug, &StateChanges, move_stories_to) -> StateEdit { state, moved }
      .remove_state(slug, move_stories_to) -> usize
      .reorder_states(&[String]) -> Vec<StateDef>
      .list_types() / .add_type(slug, description) / .remove_type(slug)
      .list_members() / .add_member(&MemberInput) -> Member
  service::config::state_usage(tx, project) -> BTreeMap<String, StateUsage>

  SystemService::new(&ctx)
      .scaffold(kind) -> String
      .install_git_hooks() / .uninstall_git_hooks() -> String
      .list_event_hooks() -> String   (infallible)
      .test_event_hook(event_type) -> String
      .install_plugin(target) / .uninstall_plugin(target) -> String

  GroupingService::new(&ctx)
      .phases() -> Vec<PhaseView>
      .phase_stories(phase) -> Vec<StoryView>
      .assign_phase(id, phase) -> StorySnapshot        (fires label_change)
      .clear_phase(id) -> PhaseCleared::{Removed(Box<StorySnapshot>), NoAssignment}
      .create_phase(phase, title) -> StorySnapshot     (no hook)
      .epics() -> Vec<StoryView>
      .create_epic(title) -> StorySnapshot             (no hook)
      .add_to_epic(epic_id, story_id)                  (no hook)

  service::templates::{agents_md(prefix, done_state), claude_md, cursor_rules}
  service::view::sort_story_views(&mut [StoryView])    // numeric, not lexicographic
  service::refold_story(tx, project, story, prefix, states)   // pub(crate)

  invoke::dispatch_unscoped(&store, root, now, Invocation)     // init + the text-only arms
  ```

  Things W2c/W2d should know, beyond the Key facts above:
  - **`tests/differential_support/mod.rs` is the shared harness now.** `Differential::new()`,
    `step`/`step_id`/`show`/`assert_no_drift`, plus `legacy_only`/`store_only`/`legacy_path`/
    `store()` for the rows where the two legs are *expected* to differ. A new differential
    file is `mod differential_support;` and nothing else. The module carries
    `#![allow(dead_code)]` because each test binary uses a different subset.
  - **`service::view::story_views` is still the read side, and W2c still owns absorbing it.**
    This wave added `sort_story_views` beside it and consumed `story_views` from
    `GroupingService`; `QueryService` should take over both rather than growing a third copy.
  - **`ServiceFixture::with_states` is how you get a catalog with two CLOSED states**, which is
    what the superstate-flip and archived-history cases need.
  - **A hook-suppressed context is `Ctx::new(ctx.store(), ctx.project(), ctx.cwd())
    .no_hooks(true).clock(Clock::Fixed(ctx.now()))`.** There is no clock *accessor* on `Ctx`
    — the builder method occupies the name — and pinning to `ctx.now()` is equivalent and
    more deterministic.
  - **`ProjectService` is the one service that does not take a `Ctx`.** Anything W5's
    `Environment` does about project selection has to account for that asymmetry rather than
    assume every service is context-shaped.

  Gate: **two consecutive green full runs, 90.3s and 88.2s warm** against W2a's ~62.5s.
  **Most of the +26s is machine contention, not this wave**: a neighbouring project's Swift
  test suite held a core at 100% throughout (load average 6–9; `cargo test --workspace` alone
  measured 75.8s wall against 27.4s of user time). The five new binaries contribute **1.5s of
  runtime** (`service_project` 0.08s, `service_config` 0.20s, `service_system` 0.09s,
  `service_grouping` 0.11s, `differential_config` 0.99s) plus the ~1.2s-per-binary link cost
  W0b, W1 and W2a all measured. Re-measure on an idle machine at the next wave boundary
  before treating this as a trend.

  Verification that the tests bite, not merely pass:
  - Deleting the `refold_occupants` call fails
    `flipping_a_superstate_re_derives_the_rows_of_the_stories_left_in_it` with
    `left: Closed, right: Open`.
  - Changing one word of one `ConfigService` validation message
    (`already exists` → `exists already`) fails
    `differential_config.rs::adding_states_agrees_including_every_rejection`.
  - The fault-injection rows (`BeforeCommit`, `MidReadModelUpdate`) fail the atomicity tests
    when armed and pass when not, which is what makes "nothing moved" a claim rather than a
    hope.

  Deviations from the wave brief, all deliberate:
  - **`init`'s Response text is byte-identical to legacy**, `.storyhook/` reference and all,
    rather than differentially normalized as the brief suggested. Byte-compatibility is the
    port's governing rule and W4 owns the rewrite; keeping the text identical makes the
    differential row a strict equality, which is strictly stronger than a normalization. The
    text is wrong under the new storage model and `init_message`'s doc comment says so.
  - **`Phase` and `Epic` were ported**, though the spec files their *list* forms under W2c's
    QueryService. `service::view::story_views` already existed, so the read halves cost one
    sort helper; splitting the two `Invocation` arms across waves would have left a
    half-ported enum arm and a roster that could not describe itself.
  - **A `GroupingService` module rather than methods on `StoryService`.** Phases and epics are
    conventions over labels and relations; putting them where the conventions are written once
    is the point, and `SystemService` — the brief's suggested home — has nothing to do with
    stories.
  - **`state_transition_events` became `pub(crate)`** (a visibility change to W2a's file, no
    behaviour change): the migration path has to produce the identical batch, and a second
    copy of it is exactly what W2a wrote that function to prevent.
  - **The `uuid` crate was added.** A project's portable identity travels in a committed file
    to other machines, so it cannot be derived from a path, a counter, or a clock.
  - **The differential harness moved to `tests/differential_support/mod.rs`** — a verbatim
    move, no assertion changes; `differential_lifecycle.rs` keeps all 39 of its rows.

- 2026-07-28 W2c: branch `rearch/w2c-query` off merged main `da446e9`. Five commits,
  `make test` green after each; two consecutive green full runs at the end (see below).
  Test count 1716 → **1775** (+59: 19 `service_query`, 9 `service_integrity`, 28
  `differential_query`, 1 conformance case, 2 wire-envelope corpus rows). 2 ignored,
  unchanged — still the W4 headline REDs. **`src/app.rs` gains exactly two additive arms
  and changes no existing one** (`git diff da446e9..HEAD -- src/app.rs` is +42/-2);
  `tests/snapshots/` is byte-unchanged.
  - `e9e170e` — `QueryService`, the nine read arms, `service::view` absorbed into
    `service::query` and deleted, `tests/{service_query,differential_query}.rs`.
  - `d3ee37c` — the store `fix:` this wave's own tests exposed (see Key facts).
  - `eb6aa63` — `IntegrityService`, the rebuild-diff doctor, `tests/service_integrity.rs`.
  - `b937e8c` — `Invocation::{ProjectSnapshot, History}` and their two `Response` variants.
  - `8bf7eeb` — `src/tui/` onto the seam, and the 30 white-box tests reconstructed.

  **Ported-arm roster: 27 → 38.** Added this wave: `list`, `show`, `search`, `next`,
  `summary`, `report`, `graph`, `context`, `handoff`, `doctor`, `project-snapshot`.
  **Remaining 10**, all in `unported_probes()`: `export`, `session-start`, `history`,
  `import`, `import-project`, `decompose`, `commit-sync`, `github-sync`, `update`, `web`.
  The roster test now asserts `ported + probes == 48`; `wire_envelope.rs` pins 48
  independently.

  **The service API W2d/W3/W4/W5 build on** (`storyhook::service`):

  ```rust
  QueryService::new(tx, project, now)          // takes &impl ReadOps — CANNOT write
      .story_map() / .story_views(include_derived)
      .project_snapshot() -> ProjectSnapshotView
      .show(id) -> StoryView
      .list(&ListFilters) -> Vec<StoryView>
      .search(query) / .next(count, phase) -> Vec<StoryView>
      .summary() / .report_summary() -> SummaryView
      .report_data() -> ReportData
      .graph(&GraphMode) -> GraphView
      .context(json: bool) / .handoff(since) -> String
  ListFilters { state, assignee, flagged, priority, label, created_after,
                updated_after, blocked, ready, stale, phase, story_type }
  service::query::{story_map, story_views, story_view, sort_story_views}  // was service::view

  IntegrityService::new(&ctx)
      .report() -> Vec<String>     // empty == healthy; caller makes it AppError::Integrity
      .fix() -> String

  // in invoke.rs, the one place a read arm gets a transaction:
  fn query<S: Store, T>(ctx, f: impl FnOnce(&QueryService<'_, S::ReadTx<'_>>) -> …) -> …
  ```

  Things W2d/W3/W4/W5 should know:
  - **`QueryService` takes a transaction, not a store, and that is the design.** A query
    arm has no store in scope, so "this command must not mutate" is a type error rather
    than a review comment. The generic `query()` helper in `invoke.rs` is what makes the
    lifetimes work; copying it is cheaper than trying to put the helper on `Ctx`, where
    the higher-ranked bound `S::ReadTx<'r>: 't` cannot be expressed.
  - **`service::view` is gone.** Its four functions live in `service::query` unchanged;
    `Ctx::story_view`, `RelationService` and `GroupingService` were re-pointed.
  - **Ordering is reproduced, defects included.** `list`/`search` sort numerically;
    `graph`/`handoff`/`context`/`summary` sort lexicographically because they iterate a
    `BTreeMap` keyed by the id *string*; `handoff` puts every open story before every
    archived one. `tests/service_query.rs` pins each with a twelve-story fixture, so the
    wave that normalizes one does it on purpose.
  - **`Invocation::ProjectSnapshot` is the TUI's only read**, and W5's SSE resync should
    use it too. Open stories only — the archive would make the payload grow without bound
    in exchange for rows nothing renders.
  - **`Invocation::History` is UNPORTED and W4 inherits it.** `Read` is expressible
    against the store today (`events_for` + `partition_known`); `Restore` is not — the
    store is append-only with no truncate, and the flip checklist already flags the TUI's
    undo as needing a store-level design rather than a port. Until then the TUI's undo
    works on the legacy path only.
  - **`storage::load_open_snapshots_tolerant` moved out of `src/tui/data.rs`.** The
    torn-final-line tolerance is a property of the JSONL format, and `ProjectSnapshot`'s
    legacy arm needs it.
  - **The TUI holds `root` for one reason: `tui::event.rs`'s notify watcher.** Everything
    else goes through `&dyn Invoker`, and every request carries `no_hooks` — the TUI has
    never fired the project's event hooks and must not start. W5 deletes the watcher.
  - **Flip-checklist category C is 85 → 14 sites**, none of them the TUI's:
    `registry_test.rs` 3, `web_test.rs` 5, `error_contract.rs` 1,
    `crates/storyhook-test-support/src/server.rs` 1, plus 4 the differential harness and
    `differential_config.rs` legitimately need to build a legacy leg. **W4 inherits none
    of the TUI's 70.**

  Gate: **two consecutive green full runs, 88.8s and 88.4s warm**, against W2b's
  90.3s/88.2s — i.e. no measurable cost. Load average was 5–7 throughout (the same
  neighbouring Swift suite W2b noted), so these are contended numbers on both sides of the
  comparison and neither is a steady state. The three new binaries contribute **1.3s of
  runtime** (`service_query` 0.14s, `service_integrity` 0.07s, `differential_query` 1.09s).

  Verification that the tests bite, not merely pass:
  - The `report --html` differential row failed on the first run because `report_data`
    left `ready_count` at zero — a number invisible in the human rendering.
  - `a_write_rejected_at_commit_does_not_poison_the_store` was RED before `d3ee37c` with
    exactly the message the fix names, and green after.
  - The doctor differential row failed on its first run with the legacy leg's data loss,
    which is how that defect was found at all.

  Deviations from the wave brief, all deliberate:
  - **Two additive `src/app.rs` arms, not one.** `app::run`'s match is exhaustive over
    `Invocation`, so a variant cannot exist without an arm, and the TUI's exit criterion
    is unreachable while undo needs to read and rewrite a story's log. Rebuilding undo out
    of compensating invocations loses undo-a-comment outright and changes
    undo-a-creation's meaning. Grouping `Read`/`Restore` under one `History { action }`
    variant is what keeps it at two rather than three.
  - **The differential rows landed with their features** rather than as a sixth commit.
    Each commit is then green and self-contained; a commit that adds a service and a
    commit that proves it agrees are the same claim.
  - **`IntegrityService::fix` diverges from legacy** — see the data-loss defect in Key
    facts. Reproducing it faithfully would have shipped data loss forward.
  - **Two small TUI behaviour changes**: labels set from the detail editor arrive sorted
    (`SetLabels` merges through a `BTreeSet`; same set, different stored order), and the
    "update status"/"remove status" notifications now show the seam's message because the
    moved-story count lives in the response.
  - **`tests/tui_undo.rs` calls the seam invocation directly** rather than
    `tui::app::dispatch`, which needs a real terminal. That is the same shape as before —
    the file always duplicated the dispatch logic — except the duplication is now one
    invocation instead of a lock, a path and a rewrite.

- 2026-07-28 W2d: branch `rearch/w2d-git` off merged main `59ee60c`. Six commits.
  Test count 1775 → **1882** (+107: 22 `service_transfer`, 12 `differential_transfer`,
  8 `service_git`, 13 `differential_git`, 14 `service_github`, 12 `service_catalog`,
  12 `service_session`, 7 `differential_session`, 2 conformance cases, 2 `paths` units,
  3 `session` units, plus the roster edits). 2 ignored, unchanged. **`src/app.rs` has
  zero changes** — `git diff 59ee60c..HEAD -- src/app.rs` is empty — and
  `tests/snapshots/` is byte-unchanged.
  - `c2b214c` — `TransferService` (export, the batch importer, `import_project`),
    `WriteOps::reserve_story_no`.
  - `1be07fe` — `GitService`; `Differential::with_git()`/`commit()`.
  - `1babe06` — `github::storage::SyncStorage` + `LegacySyncStorage` +
    `service::github::StoreSyncStorage`; `src/paths.rs`.
  - `40ba417` — `CatalogService`, `SessionService`, history, update;
    `WriteOps::forget_project_path`. Roster complete.
  - the store-leg commit — `StoreInvoker`, `STORYHOOK_INVOKER`, `make test-store`,
    and the three fixes the leg demanded (WAL locking, concurrent migration,
    project-less root resolution).
  - the docs commit — this entry, the flip checklist's section G, HANDOFF.

  **Ported-arm roster: 38 → 48. `unported_probes()` is empty**: every `Invocation`
  variant dispatches, `dispatch`'s match is exhaustive without a catch-all, and a
  new variant now stops `invoke.rs` compiling. One *action* is still owed a design
  rather than a port — `History::Restore` — and it answers loudly, pointing at the
  flip checklist. `an_unported_invocation_fails_loudly_rather_than_silently` was
  re-pointed at it.

  **The API W3/W4/W5 build on:**

  ```rust
  TransferService::new(&ctx)
      .export() -> ProjectExport
      .import(&[ImportStory]) -> ImportBatch { views, relationship_lines }
  service::transfer::import_project(&store, root, &Clock, &ProjectExport) -> usize

  GitService::new(&ctx).commit_sync(since: Option<&str>) -> String

  CatalogService::new(&store)                 // NOT Ctx-based: `list` spans projects
      .register(path, name) / .deregister(target) -> CatalogEntry
      .list() -> Vec<CatalogEntry>

  SessionService::new(&ctx).context() -> String     // the RawJson body
  service::session::{history, SILENT}

  // feature = "github-sync"
  GithubSyncService::new(&ctx).sync(story_id, dry_run) -> Response
  StoreSyncStorage::new(&ctx).backups_dir(path)      // #[must_use] builder
  github::storage::{SyncStorage, LegacySyncStorage}  // the 8-method seam
  github::run_sync_with(&dyn SyncStorage, story_id, dry_run)

  invoke::StoreInvoker::new(&store, cwd).hook_depth(u32).pointer(bool)
  invoke::dispatch_unscoped_with(&store, root, now, invocation, pointer: bool)
  paths::{data_dir, state_dir, store_path}
  store: WriteOps::{reserve_story_no, forget_project_path}
  ```

  **The store test leg**, `make test-store`: 38 targets green, **6.4s warm**,
  `STORYHOOK_INVOKER=local` over the real CLI binary. `scripts/run-store-leg.sh`
  builds the target list and exports an isolated `STORYHOOK_DATA_DIR` and
  `XDG_STATE_HOME` for the whole run — see the `TestEnv` trap in Key facts for
  why that export is load-bearing rather than tidy. **`golden_cli` is IN the leg
  and green**: all 27 byte-compatibility snapshots are identical on both legs,
  which is the strongest single piece of evidence this port has produced. The
  exclusion list, with a reason and a burn-down wave per entry, is
  `flip-checklist.md` section G.

  **Standing rule for W3, W4 and every later data-layer wave:** run
  `make test-store` after every commit, alongside `make test`, and record both
  times here. The leg is not part of `make test` — it would double a gate that
  every wave pays on every commit — so it is a discipline rather than a
  mechanism until the flip makes it the only leg.

  Verification that the tests bite, not merely pass:
  - The store leg found three production defects on its first three runs (WAL
    locking, concurrent migration, project-less root resolution), each of which
    was fixed at the origin and none of which any existing test could see.
  - The export differential row failed on its first run with `prefix: null` vs
    `"SH"` — a byte the golden corpus would have caught at the flip instead.
  - `session_start_agrees_when_every_story_is_closed` failed on its first run
    because `query::story_map` includes archived stories and
    `load_all_open_snapshots` does not.

  Deviations from the wave brief, all deliberate:
  - **The roster is `ported == 48, probes == 0`, not 47 + History.** `History::Read`
    is expressible today and porting it costs four lines; only `Restore` is owed a
    design, and it refuses loudly with a pointer at the checklist.
  - **Two store operations were added** (`reserve_story_no`, `forget_project_path`),
    both minimal and both with conformance or service tests. A restored project's
    counter and a deregistered checkout are not expressible without them.
  - **`src/service/system.rs` grew five free functions**, so the hooks and plugin
    families can be answered without a project — which is what the legacy path did
    and what the store leg proved was missing.
  - **`git commit --amend` was never used**; every fix is its own commit.

  Gate: `make test-store` **green ×2 at 6.4s**. `make test` could not be measured
  honestly at the end of this wave: three Swift test processes from a *different*
  project (one of them hung since 10:18, 348+ CPU-minutes) saturated the machine,
  and `cargo test --workspace --test web_test` **alone** measured 59–109s against
  its 7.2s baseline and failed 1–84 of 140 tests. `web_test` is unchanged code in
  this wave, every failing test passes in isolation in ~4s, and the failures are
  all wall-clock readiness deadlines. Earlier in the wave the same tree ran
  `make test` green three times (1:39, 4:10, 4:31). **Re-run the gate on an idle
  machine before treating any number here as a trend**, and check
  `ps aux | sort -nrk 3` first.

- 2026-07-28 W3: branch `rearch/w3-importer` off merged main `7eccae2`. Seven commits.
  Test count 1882 → **1946** (+64: 17 `legacy_reader`, 35 `service_migrate`, 4
  `migrate_round_trip`, and 8 `src/` units — 3 `domain::event_kind_tests`, 5 in
  `legacy::{events,paths}`. The `wire_envelope` corpus and the ported-arm roster gained rows
  rather than tests). 2 ignored, unchanged — still the W4 headline REDs.
  `tests/snapshots/` is byte-unchanged. `src/app.rs` gains **one** additive arm.
  - `fb9b2f9` — `[profile.dev] split-debuginfo = "packed"` (see Key facts; a ride-along).
  - `75dd9c1` — the harness fixture race two concurrent gates hit (Key facts).
  - `036c768` — `src/legacy/`, the read-only reader, plus both fixtures.
  - `46e9e13` — the store-leg script's `set -u` bug (Key facts).
  - `6f817c3` — `Invocation::Migrate`, `src/service/migrate.rs`, the CLI surface.
  - `a5871a6` — `tests/migrate_round_trip.rs`, the W4 revert gate.
  - this commit — STATE, the flip checklist's section D2, HANDOFF, CLAUDE.md.

  **Ported-arm roster: 48 → 49** (`migrate`). `wire_envelope.rs` pins 49 independently, and
  `unported_probes()` is still empty.

  **The API W4/W7 build on** (`storyhook::legacy`, `storyhook::service::migrate`):

  ```rust
  legacy::find_root(&Path) -> Option<PathBuf>          // ancestor walk, migrate only
  legacy::read_project(&Path) -> Result<LegacyProject, LegacyError>
  LegacyProject { root, schema, created_at, prefix: Option<String>,
                  sync_auto_transition, doctor_stale_threshold,
                  states, types, members, next_id, stories: Vec<LegacyStory> }
      .effective_prefix() -> &str
      .unknown_events() -> impl Iterator<Item = (&LegacyStory, &LegacyEvent)>
  LegacyStory { id, events: Vec<LegacyEvent>, archived, source }
  LegacyEvent { kind, at, payload: String, decoded: Option<StoryEvent> }
  LegacyPaths::new(&Path)  // project_file/states_file/…/archive_db/archive_wal/exists
  LegacyError::{NotAProject, Unreadable, Malformed}   // → AppError NotFound / Storage

  MigrationPlan::build(LegacyProject) -> Result<MigrationPlan, AppError>  // all checks here
      .report(dry_run: bool) -> MigrationReport
      .apply(&store, dest: &Path) -> Result<MigrationReport, AppError>    // one transaction
  migrate::refuse_in_linked_worktree(&Path) -> Result<(), AppError>
  MigrationReport { source, prefix, dry_run, stories, events, archived, deleted,
                    states, types, members, next_story_no,
                    repairs: Vec<Repair>, unknown_events, settings }.render() -> String
  Repair { story, relation, other, at, kind: RepairKind }
  RepairKind::{CompletedInverse, RetractedUnilateralParent { child, winner }}

  domain::{EVENT_KINDS, is_known_event_kind, event_kind}
  ```

  **`MigrationPlan::build` is where every decision happens**, and `apply` is a transcription.
  That split is what makes `--dry-run` a real read-only mode rather than a flag threaded
  through a writer, and it is why a refused tree leaves the store completely empty
  (`a_refused_tree_leaves_the_store_completely_empty`).

  **Dry-run evidence against this repository's own tree** (from a *copy*; this worktree's
  `.storyhook` is divergent and the real migration is W7's, from the main checkout):

  ```text
  # 1. from the worktree itself — the guard fires
  error: `…/.claude/worktrees/rearch` is a linked git worktree; migrate from the main
  checkout at `/Volumes/Code/mikeyward/storyhook` instead.

  # 2. the plan, from a read-only copy of the same tree
  would import 61 stories (44 archived, 1 soft-deleted) and 496 events
    prefix SH, 5 states, 5 types, 0 members, next story SH-62
    repairs (SH-60): 5 one-sided relations completed, 5 unilateral parent claims retracted
  ```

  486 + 10 = 496. The ten repairs settle all fifteen violations, and the migrated project's
  `compute_integrity_issues` is empty.

  Things W4 and W7 should know:
  - **The rollback procedure is written and is the flip checklist's section D2.** Paste it into
    the W4 PR. It is gated on `cargo test --test migrate_round_trip` being 4/4 green; if that
    file is red the flip is a one-way door.
  - **`.storyhook/` must stay in the repository until W7** for a concrete reason, not caution:
    it is the only copy of `created_at`, the `sync`/`doctor` settings and the burned story
    numbers, none of which an export document can carry.
  - **W7's own migration will print the same ten repairs**, and they are a real change to what
    the tracker says: SH-40 loses five children it claimed alone, SH-31 gains a parent. Worth
    eyeballing `story graph` before and after.
  - **`story migrate` runs on the legacy leg by design** — `src/app.rs`'s new arm opens the
    global store and forwards to `dispatch_unscoped`. Without it nothing could be migrated
    until after the flip, which is backwards.
  - `story migrate --help` has a real topic; `HELP_TEXT` and the plugin's `cli-reference.md`
    both document the verb. No golden snapshot moved (there is no help snapshot).

  Gate: `make test` green in **1:33** with a rebuild and **43.7s fully warm**, `make
  test-store` green in **6.4s over 40 targets** (38 + `legacy_reader` +
  `migrate_round_trip`). The warm number is the first honest steady state this program has
  measured since W1 — W2b/W2c/W2d all ran on a contended machine and reported 88–90s. The three new binaries contribute **1.4s of
  runtime** (`service_migrate` 1.15s — six of its tests drive the real CLI — `legacy_reader`
  0.06s, `migrate_round_trip` 0.13s) plus the ~1.2s-per-binary link cost every wave since W0b
  has measured. Load average was 2–4 throughout, so unlike W2b/W2c/W2d these are uncontended
  numbers.

  Verification that the tests bite, not merely pass:
  - Adding one `fs::write` to `src/legacy/paths.rs` fails `the_reader_contains_no_write_calls`
    naming the file and the call.
  - Dropping `members` from `TransferService::export` fails the round trip on the custom-config
    fixture.
  - Making `raw_events` skip the retraction repairs fails the migration itself with the store's
    own `Integrity("adding a relation: a story may have at most one parent")` — which is the
    store refusing exactly the shape the ruling exists to settle.
  - The naive repair (complete every one-sided claim) was written first and *did* fail this
    way on the real tree; that failure is how the 10-plus-5 structure was found at all.

  Deviations from the wave brief, all deliberate:
  - **No `--rollback` verb.** The brief called the reverse path non-negotiable; it is, and it
    is delivered as the round-trip test plus the written procedure. A verb would have had to
    call `storage::import_project`, pinning storage.rs's *write* half alive past the flip —
    the one thing `src/legacy/` was made independent to avoid. The two halves of the rollback
    are `story export` and the reverted binary's own `story import-project`, both of which
    already exist.
  - **Multi-parent conflicts are repaired, not refused**, under the agreement-beats-assertion
    rule. See Key facts for the argument and for why refusing was rejected. Genuinely
    undecidable conflicts *are* refused.
  - **`test(migrate):` rather than `feat(migrate):` for the round-trip commit.** It adds no
    production code.
  - **Two extra commits** beyond the brief's six: the harness race and the store-leg script
    bug, both defects found in-wave, both fixed at the origin in their own commits.
  - **`serde_json`'s `raw_value` feature was added.** Recovering an archived event's original
    JSON text out of the array it is stored in needs it; a `Value` round trip sorts keys, and
    "verbatim" has to mean verbatim.
  - **`domain::EVENT_KINDS` + `event_kind` + `is_known_event_kind`** are new, outside
    `src/legacy/` and `src/service/`. The corrupt-versus-unknown distinction cannot be made
    without them and it is a property of `StoryEvent`, so it lives next to `StoryEvent`.

- 2026-07-29 W4: branch `rearch/w4-flip` off merged main `e70a632`. Eleven commits,
  `make test` green after each. Test count 1946 → **2003**, and **2 ignored → 0**: the
  headline REDs this program was written to turn green are green.

  **The commit order changed, and the change is the wave's principal deviation.** The
  brief's slots were swap (4) then test-rewrite (6). Measured against reality that
  ordering cannot be green-per-commit: `make test` runs the legacy leg before the swap
  and the store leg after it, so every file on `make test-store`'s exclusion list is one
  the swap turns red — 57 tests across 15 files. The burn-down therefore moved *ahead*
  of the swap, and the swap's precondition became "the exclusion list is empty". See Key
  facts; the generalization is that in a strangler the suite crosses over before the
  default does.

  - `e97ca5f` — `store::test_support` (inject/forget/corrupt) + store-default fixtures.
  - `ff634f8` — a latent differential flake that fired on the first full run.
  - `1c512d0` — the pointer file's `[plugin]`/`[hooks]` tables, `paths::legacy_global_dir`,
    `service::adopt_legacy_registry`. Additive; nothing called it yet.
  - `ab6b2ff` — **ancestor-walking root resolution**, its own commit and its own tests,
    because it is a semantics change: `story <verb>` in a subdirectory starts succeeding.
  - `e8c1d1c` — the pre-flip old-vs-new `--json` diff harness. **49 of 49 commands
    byte-identical** on a fourteen-story fixture (39 exiting 0, 8 exiting 3, 2 exiting 2).
    Deleted in `eef531d`; this commit is where it can be re-run.
  - `1e70f58` — the burn-down: store-leg failures **57 → 16** with no file exclusions.
  - `4b932ed` — **THE SWAP.** Small production diff; the last 16 leg-specific tests rode
    with it because they cannot be written to pass on both models.
  - `1445291` — the exit criterion un-ignored. 20/20 stable.
  - `c50a650` — the unmigrated-repository guard.
  - `ca4f4f7` — compensating-events undo; `EVENT_KINDS` 15 → 17.
  - `eef531d` — `make test-store` retired, the quarantine made enforceable.

  **The API W5 builds on:**

  ```rust
  invoke::open_store() -> Result<SqliteStore, AppError>   // open + migrate + adopt
  invoke::StoreInvoker::new(&store, cwd)                  // the only CLI/TUI invoker
  service::history::restore(&ctx, id, &[StoryEvent]) -> Vec<StoryEvent>
  service::adopt_legacy_registry(&store, path) -> RegistryAdoption
  service::project::{legacy_project_at, unmigrated_error, pointer_hooks, pointer_plugin}
  paths::legacy_global_dir() -> ~/.storyhook
  store::WriteOps::rename_project
  store::test_support::{inject_events, inject_raw_events, forget_events, forget_story,
                        corrupt_snapshot}          // feature = "fault-injection"
  storyhook_test_support::{TestEnv::{store_path, open_store},
                           Project::{open_store, project_id, pointer, story_no, is_legacy},
                           ProjectBuilder::legacy, project_id_at}
  ```

  Gate: **two consecutive green full runs, 49.7s and 50.7s warm** — against W3's 43.7s,
  so +6s for 57 more tests and one more test binary. `migrate_round_trip` **4/4 green**,
  which is what keeps the revert window open. Load average 2–4 throughout.

  Verification that the tests bite, not merely pass:
  - Reverting root resolution to a single level fails three of the six new resolution
    tests, naming the subdirectory, the nested project and the path-row cases.
  - `the_legacy_path_is_reachable_only_from_the_web_dashboard` failed on its first run and
    found real leftovers: `src/tui/data.rs`'s fixtures were still building a
    `LegacyInvoker`, so they were green about a stack the TUI had stopped running on.
  - `EVENT_KINDS`'s drift test failed on the first attempt at the two new variants,
    with the two orderings side by side.
  - The store leg found `story web status` and `story update --check` broken by a missing
    `is_project_less` arm — two real bugs the flip would have shipped.

  Deviations from the wave brief, all deliberate:
  - **The commit order**, above. Nine slots became eleven commits.
  - **`StoreInvoker` keeps its name**; the spec calls it `LocalInvoker`. Renaming inside
    the swap would have inflated the bisect atom for no behaviour, and `StoreInvoker` vs
    `HttpInvoker` reads correctly for the daemon wave.
  - **`STORYHOOK_INVOKER=local` still parses** (as the no-op it now is) and only `legacy`
    is refused. The spec calls `--local` a permanent documented mode; refusing the value
    that names today's only stack would be gratuitous.
  - **Two new `StoryEvent` variants**, which the brief did not anticipate. An append-only
    undo of a comment or of an assignment is not expressible without them; the scope is
    exactly the operations the TUI can undo, and a story's *type* is deliberately left
    without one.
  - **`WriteOps::rename_project` added** to stop the flip regressing
    `story web register --name`.
  - **The bash plugin suite was touched**, which the checklist filed under W7. Three of
    its tests asserted the defects this wave removes; leaving them would have left the
    gate red. Only those assertions and `story.sh`'s two state-reading helpers changed —
    the `.storyhook` literals W7 owns are untouched.
  - **`docs/rearch/baseline/` is NOT regenerated.** It is the pre-rearch reference point,
    including its "Ignored: 2 of 1171" line, and regenerating it would destroy the thing
    it exists to be. The current counts are here instead.

- 2026-07-29 W5: branch `rearch/w5-daemon` off merged main `01d8332`. Seven commits,
  `make test` green after each. Test count 2003 → **2084**, 0 ignored.

  **The daemon is one process.** `src/web.rs` became `src/api/{http,rest,rpc,wire}.rs`
  and `src/daemon/{backup,bus,commands,lifecycle,serve,tailnet}.rs`; what is left of
  `web.rs` is the deprecated `story web` aliases and the browser/clipboard seams.
  The tailnet listener, `TailnetIdentity`, the trusted-host allowlist and the CSRF
  guard moved **verbatim**, in their own commit, before anything was ported.

  - `5996898` — `Environment`, resolved once in `main` and passed down; `paths.rs`
    deleted. `StoreSyncStorage::backups_dir` deleted with it: the environment *is*
    the redirect. `error_contract`'s LockTimeout row asks the child for a
    one-second busy timeout — 5.5s → 3.4s for that binary.
  - `a5320d9` — the HTTP plumbing and the tailnet identity extracted verbatim.
    `web_test` untouched and 140/140, which is what makes it a pure motion.
  - `c4365c3` — the dashboard over the service layer. The double dispatch dies
    with the lock; the change bus replaces the filesystem watcher;
    `~/.storyhook/` is retired with a `MIGRATED.txt`; the runtime files move to
    the state home. `web_test` rewritten, 26s → 3.8s.
  - `74bd28e` — the lifecycle: portfile, the pidfile lock that *is* liveness,
    `/api/v1/hello`, auto-spawn under a lock held through the child's write,
    launchd, and the `web` aliases.
  - `32e641e` — `/api/v1/invoke`, `HttpInvoker` as the default, `--local` as a
    permanent documented mode, and `hook_depth` in the envelope.
  - `7288e70` — the TUI off `notify`; the dependency leaves `Cargo.lock`.
  - `44c5bf7` — `make test-daemon`, daily verified backups, and the defects the
    leg found.

  Gates, two consecutive runs each: `make test` **64.6s / 57.5s**, 2084 tests,
  0 ignored. `make test-daemon` **58.1s / 51.9s**, the same 2084 over RPC, all
  green. The daemon leg costs about the same as the in-process one once its
  parallelism is bounded.

  Verification that the tests bite, not merely pass:
  - `no_filesystem_watcher_remains` writes the three shapes the old watcher
    reacted to and asserts exactly one event arrives afterwards — the store
    write that follows them.
  - `every_answer_is_byte_identical_through_the_daemon_and_in_process` compares
    fifteen commands on stdout, stderr and exit code, failures included.
  - `a_hook_that_runs_story_terminates` uses an `on_create` hook that runs
    `story new`: two stories, not infinity. Without depth in the envelope it
    would not fail, it would never finish.
  - `make test-daemon` found three transport defects that no in-process test
    could reach, and the orphan check found a fourth in the bash suite.

  **Deferred, and the wave's principal deviation: the quarantine deletion.**
  `app::run`, `lock.rs`, `registry.rs` and `storage.rs`'s write half are still
  there. They are genuinely dead — the dashboard was their last caller — and the
  quarantine test still enforces that nothing new reaches them, so deferring is
  safe. What blocks the deletion is one piece of real work rather than volume:
  `ProjectBuilder::legacy` builds `service_migrate`'s fixtures by running
  `app::run`, and it needs replacing with a direct writer against
  `src/legacy/`'s layout (~90 lines, `init` and `new_story` only). HANDOFF.md
  carries the measured blast radius, row by row.

## Resume protocol (fresh session)

1. `cd` the worktree; `git log --oneline -5`; read this file + HANDOFF.md if present.
2. `make test` MUST be green before touching anything (if web_test mass-fails: check for
   orphaned `web_test-*` listeners in 19xxx first, then `uptime` and
   `ps aux | sort -nrk 3` — see Key facts).
3. `make test` runs in-process; `make test-daemon` runs the same suite over RPC. Both
   must be green. Do not fold the second into the first — see the Key facts on why
   `--test-threads=4` there is a bound on live daemons rather than tuning.
4. Continue at the first non-DONE step above via a fresh subagent; orchestrator keeps its own
   context minimal (delegate reads/edits; terse reports only).
