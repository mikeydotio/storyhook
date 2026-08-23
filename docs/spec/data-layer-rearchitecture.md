# StoryHook Data-Layer Rearchitecture — Specification

> **What this is:** the design of record for moving storyhook's story data out of
> per-repository `.storyhook/` directories and into a single global store owned by a local
> daemon. Every wave of the program is specified here; a wave that departs from this document
> records the departure rather than editing it silently.
>
> **Where execution state lives:** [`docs/rearch/STATE.md`](../rearch/STATE.md) — the ledger.
> Wave status, step log, discovered defects, and everything a resuming session must not
> re-derive belong there. This file changes only when the *design* changes.
>
> **Companion artifacts:** [`docs/rearch/baseline/`](../rearch/baseline/) (the pre-rearch
> reference capture) and [`docs/rearch/flip-checklist.md`](../rearch/flip-checklist.md) (the
> enumerated W4 work).

## Context

StoryHook currently stores all story data version-controlled inside each repository
(`.storyhook/`: per-story JSONL event logs, a SQLite archive, a `next-id` counter file, TOML
config). Three days of concentrated use falsified this design. Evidence gathered from the live
repo:

- **Every worktree is an independent, silently-divergent database** (SH-46): `--if-state` CAS
  claims can succeed in one checkout while the worktree is created from another; the
  per-directory flock protects nothing that matters (each checkout locks its own copy).
- **IDs collide across branches — it has corrupted data twice**: two branches independently
  minted `SH-49` for different stories; a merge with *zero* source conflicts required
  hand-reconciling `next-id`, hand-merging the binary `archive.db`, and renumbering a story
  (merge `2ba5eec`).
- **The archive is a binary blob git cannot merge** — 23 commits mutate it; no `.gitattributes`
  merge driver exists even for the JSONL.
- **32% of the last 25 PRs were pure `.storyhook/` bookkeeping** — org rules make every status
  change cost a branch + PR + merge commit.
- **The post-commit hook re-dirties the tree on every commit**, and commit-sync feeds its own
  output back through git: SH-61 documents that fixing SH-58 (body scanning) turns the churn
  loop non-terminating; today it terminates only via an undocumented commit-subject convention.
- **No schema versioning**: adding a `StoryEvent` variant is a silent breaking change that
  already took the web dashboard down (SH-54); split-brain between event log and archived
  snapshot has occurred (SH-20).
- **Invariants are unenforced**: 15 live `story doctor` violations (SH-60) — half-written
  bidirectional relations are representable and survive indefinitely.

Root causes, ranked: (1) tracker identity is a directory, not a project; (2) mutable
coordination state stored in an immutable, branched medium; (3) derived metadata written into
the authored event log; (4) invariants enforced by convention, not by the storage layer.

The 2026-03-22 research doc's principles — "single binary, zero infrastructure," "git-as-auth" —
were reasonable bets that the evidence above has falsified. This specification deliberately
trades them for a local daemon owning a global store.

**Intended outcome:** story data decoupled from repositories entirely; branches/worktrees/
machines all see one truth; zero repo writes during normal operation; a data layer that is
server-hostable by construction; strong testing foundations built TDD-first.

## Locked decisions

Settled 2026-07-27. Each is a constraint on every wave; revisiting one means revisiting this
document, not working around it.

| # | Decision |
|---|---|
| 1 | **Local daemon + thin CLI client.** Daemon auto-spawned by CLI; optional `story daemon install` registers a launchd agent for always-on tailnet serving. |
| 2 | **HTTP + JSON.** Versioned RPC (`POST /api/v1/invoke`, existing `Invocation`/`Response` envelope) for CLI/TUI; existing REST resource API retained for the web dashboard. One shared service layer beneath both. |
| 3 | **SQLite behind a `Store` trait** designed to admit Postgres later (no Postgres impl now). |
| 4 | **Single global DB**, project-scoped rows. |
| 5 | **Event sourcing kept**: events table is source of truth; read-model rows updated in the same transaction; rebuild-and-diff facility (also becomes the sound `doctor`). |
| 6 | **XDG data home** (`~/.local/share/storyhook` for data; state/logs per XDG conventions). Existing `~/.storyhook/{registry.toml,web.pid,web.log}` absorbed. |
| 7 | **Repo footprint = one tiny committed pointer file** (project id + prefix). Nothing else ever written to a repo during normal operation. |
| 8 | **CLI surface byte-compatible** where feasible — the 585 assert_cmd characterization tests are the behavior-preservation harness. |
| 9 | **Git features kept** (commit-sync, git hooks, github-sync) — reads from git, writes to the store; zero repo writes. |
| 10 | **Auto-migrations at daemon startup** with timestamped pre-migration backup; CLI/daemon version mismatch → auto-restart daemon. |
| 11 | **Legacy import**: `story migrate` using the existing `ProjectExport` envelope; only storyhook's own repo needs importing. |
| 12 | **Execution**: implement directly from this spec; TDD; wave-per-PR; every commit passes `make test`; bisectable; two-hats; Conventional Commits. Work happens in a linked worktree → each wave ends at "PR opened"; no version bumps/deploys from the worktree. |

## What already exists to build on

- **The seam**: `app::run(root, Invocation) -> Result<Response, AppError>` (src/app.rs:22)
  already serves both `main.rs` and the web server (`run_and_reply`, src/web.rs:183).
  `Invocation` is the de facto command envelope.
- **A proto-daemon**: `src/web.rs` (2,535 LOC) is already a multi-repo, registry-driven,
  tailnet-native daemon — tiny_http, REST covering ~90% of the needed surface, SSE push
  (currently fs-watcher-driven), tailscale identity + trusted-host/CSRF guards, pidfile
  lifecycle.
- **Event-sourced domain**: `StoryEvent` (15 variants), `fold_story`, `StorySnapshot`
  (src/domain.rs) — unchanged by this plan.
- **Migration payload**: `ProjectExport`/`ExportedStory` (src/storage.rs:1341–1487) already
  round-trips whole projects.
- **Deps already shipped**: rusqlite (bundled SQLite), tiny_http, notify, fs4 — no significant
  new dependencies expected.
- **Characterization harness**: 585 binary-driven integration tests asserting stdout/JSON/exit
  codes — implementation-agnostic.

Key liabilities to fix along the way: no storage abstraction (~50 free fns on `root: &Path`,
295 call sites, 201 in app.rs); no shared Rust test helpers (32 duplicated `fn story` builders);
`src/lock.rs` untested and panic-unsafe; clock unmockable; port allocation in web tests
collision-prone.

## Target architecture

Layering (workspace: `storyhook` + `storyhook-test-support`):

```
CLI (main.rs)          TUI            Web dashboard (browser)
     │                  │                    │
     └── Invoker seam ──┘                    │  REST + SSE (tailnet listener,
          │ HttpInvoker → /api/v1/invoke     │   existing guards verbatim)
          │  (loopback only + token)         │
          ▼                                  ▼
        api/ (ONE daemon: tiny_http) ────────┘   REST calls services DIRECTLY
          │                                       (kills web.rs:203 double-dispatch)
        service/  ← invariants live here (CAS, relation symmetry, state rules)
          │
        store/    ← Store trait (sync, GATs); SQLite impl; events + read model
          │         in one txn; ID allocation in-tx; catalog; migrations;
          ▼         rebuild-and-diff
        SQLite @ $XDG_DATA_HOME/storyhook/store.db   (WAL; pool + write mutex)
```

### Type-system proposal (UML)

```mermaid
classDiagram
    class Invoker {
        <<trait>>
        +invoke(InvokeRequest) Result~Response, AppError~
    }
    class LegacyInvoker {
        wraps app::run verbatim
        (W0b..W4 only, then deleted)
    }
    class LocalInvoker~S: Store~ {
        -store: S
        -services: Services~S~
        in-process; --local mode
    }
    class HttpInvoker {
        -daemon_addr, -token, -client_version
        auto-spawn; version handshake
    }
    Invoker <|.. LegacyInvoker
    Invoker <|.. LocalInvoker
    Invoker <|.. HttpInvoker

    class Store {
        <<trait>>
        +read(f: FnOnce(&ReadTx)) Result~T, StoreError~
        +write(f: FnOnce(&mut WriteTx)) Result~T, StoreError~
        +migrate() Result~MigrationReport, StoreError~
    }
    class ReadOps {
        <<trait>>
        +project_by_uuid/path() / projects()
        +states/types/members/settings(ProjectId)
        +events_for/head_seq/events_since(ProjectId, ..)
        +story/stories/relations_from/relations_to(ProjectId, ..)
    }
    class WriteOps {
        <<trait, : ReadOps>>
        +allocate_story_no(ProjectId) StoryNo
        +append_events(ProjectId, StoryNo, ExpectedSeq, [StoryEvent]) EventSeq
        +put_story(ProjectId, StorySnapshot, EventSeq)
        +put_states/types/member/settings(..)
        +create_project(NewProject) ProjectId
        +touch_project_path(..)
    }
    class SqliteStore {
        -pool: Mutex~Vec~Connection~~ (cap 8)
        -write: Mutex~()~
        WAL, BEGIN IMMEDIATE, busy_timeout 5000
    }
    Store <|.. SqliteStore
    Store ..> ReadOps : ReadTx GAT
    Store ..> WriteOps : WriteTx GAT
    LocalInvoker --> Store

    class Services {
        ProjectService / StoryService / RelationService
        ConfigService / QueryService(&ReadOps only)
        IntegrityService / GitService / SystemService
    }
    class dispatch {
        invoke::dispatch(&Ctx, Invocation) Result~Response, AppError~
        arms ≤15 lines: validate → one service call → view
    }
    class Ctx {
        +project: ProjectId
        +no_hooks, +hook_depth, +cwd
        +env: Environment
    }
    class Environment {
        data_home / state_home / home
        clock: Clock / daemon_addr
        built once in main()
    }
    Services --> Store
    dispatch --> Services
    dispatch --> Ctx
    Ctx --> Environment

    class domain {
        StoryEvent (15 variants, UNCHANGED)
        fold_story() / StorySnapshot
        + StoryNo / ProjectId / EventSeq newtypes
    }
    Services --> domain : fold in service, never in store
```

Key type rules: **sync throughout** (rusqlite/ureq/git/tiny_http are all blocking; the
`postgres` crate is sync too — async buys nothing and infects everything). **Generic
`S: Store`, never `dyn`** (one impl per process; Postgres later = `enum AnyStore` delegation).
**No mock Store ever** — a mock that fakes transactions re-ships the split-brain class; tests
run the conformance suite against real SQLite files. `ProjectId`/`StoryNo` newtypes structurally
required by every query — a missing project scope is a compile error (critical: every repo
defaults to prefix `SH`, so cross-project ID collision is the norm in a shared DB).
`StoreError → AppError` conversion preserves variants (`exit_code()` and HTTP `status_for` both
switch on them; `Conflict→StateConflict→409`, `Busy→LockTimeout→409+Retry-After` — today's
client contract byte-for-byte).

### The strangler seam

A file-backed `Store` impl cannot honestly provide transactions or CAS, so strangling at the
store level would validate services against a fake. Instead the strangler is the **Invoker
seam**: `LegacyInvoker` (wraps today's `app::run` — perfect fidelity by identity) adopted by
CLI+web in W0b; services/dispatch/`LocalInvoker` built additively in W1–W2 (app.rs **frozen**,
not split — the top schedule risk evaporates); once dispatch is complete, the integration suite
runs **both invokers** (`STORYHOOK_INVOKER` test-harness switch) until W4, whose flip is a
literal constructor swap. Legacy leg + `LegacyInvoker` deleted in the flip PR.

### SQLite schema (summary — full DDL carried in the W1 PR)

`schema_migrations` · `projects` (uuid ← pointer file, slug ← absorbs registry id, prefix,
`next_story_no`, `next_global_seq` — **both allocated via `UPDATE … RETURNING` inside the write
tx**: that one statement is the entire ID-collision fix) · `project_paths` (a project has MANY
checkouts — this kills SH-46) · `events` (append-only source of truth; `seq` per-story = CAS
target; `global_seq` per-project = change feed) · `stories` read model (folded columns for
everything `List` filters/sorts on + full snapshot JSON; **`archived` column replaces the
open/archive.db split entirely**) · `story_labels`, `story_relations` (+reverse index:
`is_ready`/`graph`/`next` stop being full scans) · per-project config as **columns, not blobs**
(`project_states` with explicit `position`, `project_types`, `project_members`,
`project_settings`, `github_bases`) — blob round-trips are how SH-49 destroyed `description`.
Portability: no AUTOINCREMENT, RFC3339 TEXT timestamps, `ON CONFLICT DO UPDATE`, `RETURNING` —
all Postgres-compatible.

### Daemon (one process — `story web` becomes aliases)

- **Listeners:** loopback (RPC `/api/v1/invoke` — **loopback-only + per-daemon random token** in
  mode-0600 `daemon.json`; it's a full-privilege surface and loopback is not a trust boundary) +
  tailnet (dashboard REST/SSE; `TailnetIdentity`, trusted-host allowlist, CSRF guard survive
  **verbatim** — security-load-bearing code).
- **Port:** 3456 if free else ephemeral (keeps the bookmarked dashboard URL, never fails to
  start); portfile `$XDG_STATE_HOME/storyhook/daemon.json`
  `{pid, port, version, protocol, exe, exe_mtime, started_at, token}` is authoritative.
- **Auto-spawn:** flock → re-check → child **binds first, then atomically writes daemon.json**,
  then serves; parent polls health. Daemon holds a lifetime flock on its pidfile — liveness =
  "can I take the lock?", deleting `is_process_alive` (PID-reuse-fooled today). Spawn lock held
  through pidfile write (fixes the release-early race at web.rs:1839 before auto-spawn makes it
  hot).
- **Version skew:** mismatch on version/exe/exe_mtime (from daemon.json — no round trip) →
  graceful shutdown (drain with deadline, SSE `reload` event) → respawn → send the request
  *first time*. **Never auto-retry a mutation.** Strict same-version enforcement keeps
  `Invocation`/`Response` free of compat obligations (documented loudly); `protocol: 1` field
  retained for precise mismatch errors. `/api/v1/hello` (identity+version) guards against
  trusting a foreign service on a recycled port.
- **Concurrency:** pool (cap 8) + WAL for reads; process-wide write mutex + `BEGIN IMMEDIATE` +
  `busy_timeout=5000` (matches today's 5s lock deadline — LockTimeout contract preserved).
  **No per-project locks** (deadlock surface for zero benefit at this write rate). CAS
  (`ExpectedSeq`) stays load-bearing for `--local` writers and TUI read-then-write.
  `catch_unwind` at the request boundary; `store.transact` is panic-safe by construction
  (rollback on drop). Nothing replaces `lock.rs` — it's deleted.
- **Change feed:** SSE from post-commit in-process bus (bounded drop-oldest queue,
  **observable** drops → `resync` event) + `PRAGMA data_version` polling to catch external
  `--local` writers. Strictly more reliable than the notify watcher (which provably misses
  config changes today). TUI: one bulk snapshot read + SSE invalidation (replaces 5 reads per
  timer tick).
- **Execution modes:** default `HttpInvoker` (auto-spawn); `--local`/`STORYHOOK_INVOKER=local` =
  `LocalInvoker`, **documented first-class mode** recommended for git hooks and CI (a daemon
  spawn inside `prepare-commit-msg` is hostile; SQLite WAL multi-process access is its design
  point). Daemon unreachable without `--local` → **fail loud** naming the remedy. Never a silent
  fallback (silent fallback = dashboard silently stale = the SH-54 failure shape).
  **Reversed — see "As built".** `--local` is deleted (SH-114); there is one route from a CLI
  command to the store.

### Wire envelope

`POST /api/v1/invoke`:
`{protocol, client_version, request_id, project: uuid|null, cwd, no_hooks, hook_depth, invocation}`
→ `{protocol, server_version, result: ok|error, response | error{kind, message, fields, exit_code}}`.
Ship `Response`, not rendered text — rendering stays client-side in `output.rs` (byte-compat
structural). Errors round-trip the `AppError` variant + fields (`StateConflict{expected,
actual}`), not a string. **`hook_depth` travels in the envelope** — read from env by the CLI,
enforced by the daemon (refuse hooks at depth ≥1, export depth+1 to children); otherwise a user
hook that shells out to `story` becomes an unbounded loop through the daemon (SH-61 relocated
and amplified). `json`/`quiet` stay client-side (render-only); only `no_hooks` crosses.

### What dies (~2,300 LOC) / net delta

Deleted: `lock.rs` (unwind-leaks; subsumed by transactions) · `registry.rs` (→ `projects` +
`project_paths`; registry.toml read once by migrate) · legacy write half of `storage.rs`
(~1,300 LOC) · the open/archive split · `repair_archived_snapshots` (→ rebuild-and-diff,
strictly more general) · notify watcher + **`notify` dependency** · `is_process_alive` ·
`.gitignore` machinery · vestigial `open/indexes/`. Survives relocated: `domain.rs`
(+newtypes), `output.rs`, `cli.rs`, `help_topics.rs`, `decompose.rs`; `web.rs` →
`api/{http,rest,rpc}.rs` + `daemon/lifecycle.rs`; legacy readers → `migrate/legacy.rs`
(~200–350 LOC, permanent, read-only). Net: **roughly +1,000–1,500 src LOC** (importer + daemon
lifecycle + rebuild-diff are real additions); `app.rs` 3,414 → ~1,200. The win is structural,
not size.

### Engineering risk register (top items, owned by waves)

| Risk | Mitigation | Wave |
|---|---|---|
| **Bash plugin suite writes into the REAL data home on every `make test` post-flip** (lib.sh has no XDG isolation; highest severity) | Per-run `XDG_*` exports in lib.sh + run-tests.sh asserts they're set and under /tmp; drop its `git add .storyhook` | **W0 (mandatory)** |
| Output-ordering drift: `handoff` and `export` iterate filename-lexicographic (SH-1, SH-10, SH-2…) today | ≥12-story golden fixture in W0 corpus; pre-flip old-vs-new `--json` diff of every command; any real ordering change = own labeled behavior commit, never smuggled into the flip | W0/W4 |
| Ancestor-walk changes `ensure_project` semantics (subdir calls start succeeding; tests assert failure) | Own commit + tests adjacent to the flip; add `--repo`/`STORYHOOK_PROJECT` so the plugin's cd-subshell workaround can die (follow-up story) | W4 |
| `story migrate` from a linked worktree mints a duplicate project (same prefix, colliding IDs — the corruption we're fixing) | migrate refuses in linked worktrees (`--git-common-dir`); pointer minted once in main worktree; doctor check "two projects, same prefix, overlapping paths" | W3 |
| ~8 tests fabricate corruption via raw JSONL writes | `store::test_support::inject_events()` (validation-bypassing) so they keep testing corruption; flip checklist enumerated in W0 | W0/W4 |
| Backup of a hot-WAL DB via `fs::copy` = corrupt backup that fails exactly when needed | `VACUUM INTO` + `PRAGMA integrity_check` of the copy; refuse to migrate if either fails | W1 |
| One global DB = one global blast radius | Daily `VACUUM INTO` snapshot (retain 7) + pre-migration backups + `story export` JSON escape hatch; doctor reports integrity + backup age | W5/W8 |
| `~/.storyhook` legacy global state | First run imports registry.toml, relocates logs, writes `MIGRATED.txt`, **never deletes**; doctor reports leftovers | W4/W5 |
| commit-sync idempotency is an O(events) string scan defeated by a user comment starting with `[git] ` | First-class `StoryCommitLinked` event + unique constraint `(project, story_no, sha)`; rendering compatibility verified against golden corpus; own labeled commit | W6 |

## Test strategy

**The structural byte-compat argument:** `app::run` already returns
`Result<Response, AppError>` (src/output.rs:88) and all rendering lives client-side in
`render_response`/`render_error`. If the wire carries that envelope faithfully and rendering
code never moves, CLI output is identical **by construction**. Gap to close: `Response`/view
types derive `Serialize` only; `AppError` has no serde (and `StateConflict` carries structured
payload). → dedicated small PR right after W0: `feat(output): wire-serializable Response and
AppError` with round-trip tests (`render(x) == render(from_wire(to_wire(x)))` across all
`(json, quiet)` combos). Prerequisite for the differential harness (W2+) and W5.

**Test architecture (target):**

- **Store conformance suite** — `store_conformance_suite!` macro (~160 tests; one `#[test]` per
  case for granularity) run against the SQLite impl now, a future Postgres impl for one line.
  File-backed SQLite in scratch dirs, never `:memory:` (WAL/reopen/crash paths must be real).
  Includes **16 cross-project isolation tests** — in a single global DB where every repo
  defaults to prefix `SH`, a missing `project_id` scope is silent cross-project corruption; this
  is a brand-new risk class with zero precedent in the current suite.
- **Service invariants** (~180–220 tests) + **`assert_no_drift` in the service fixture's
  `Drop`** — every service test doubles as a read-model↔events drift detector (~200 free
  checks/run) and exercises the rebuild-diff oracle enough to trust it as `story doctor`.
- **HTTP API contract** (~120–150 tests, in-process server on `127.0.0.1:0`, driven by ureq):
  every Invocation variant, error-mapping table over all 10 `AppError` variants (exit code +
  JSON envelope survive the hop), REST routes the dashboard consumes, SSE, `/api/v1/hello`.
- **Full-binary e2e**: the 585 (assertion-identical) + ~80 new (lifecycle, races, crash
  atomicity, worktree resolution).
- **Bash plugin suite**: per-test `XDG_DATA_HOME`/`XDG_STATE_HOME` isolation added in W5 at
  latest (post-cutover the 17 scripts otherwise share one DB — nondeterministic collisions).

**Test-support: workspace member crate `storyhook-test-support`** — compiles once instead of
into 44 binaries, exports the conformance macro, and the fixture builder is itself tested.
API: `TestEnv::shared()` (one daemon per test *binary* via OnceLock — the rule that keeps 585
tests from spawning 585 daemons; per-test isolation via per-test project ids) /
`TestEnv::isolated()` (opt-in, ~60 tests: migration/corruption/lifecycle); `ProjectBuilder`
(`.git()`, `.with_local_origin()` porting lib.sh's `mk_story_repo`, `.worktree()` for the SH-46
shape); `Daemon` (Drop: SIGTERM→wait→SIGKILL→waitpid). Orphan defense in four layers: Drop
guard, own process group, `STORYHOOK_PARENT_PID` suicide contract, `make test` postlude failing
on surviving `story --serve` processes. `scratch_dir()` resolves outside `$TMPDIR` (Spotlight)
with a `clippy.toml` `disallowed-methods` ban on raw `tempfile::tempdir`. Migration of the 44
test files: 3 proof files in W0, the rest opportunistically in the wave touching their subject;
**a migration commit may change setup lines but never an `assert*` line** (grep-reviewable).

**TDD sequence highlights (mapped onto the waves below):**

- **W0 (red first):** `two_worktrees_of_one_repo_mint_colliding_ids` — RED today, the program's
  headline test; goes green at W4. (Single-dir parallel `story new` is already lock-safe — the
  per-checkout lock is the defect, not the RMW.) Plus: golden CLI corpus (~200 invocations ×
  json/human × success/error) as insta snapshots with declarative timestamp redactions, scoped
  to `tests/golden_cli.rs` only, `INSTA_UPDATE=no` in make test; error-code table;
  export-idempotency tests; **snapshot of this repo's live `.storyhook/` tree + golden export
  now** (richest import fixture available and it keeps changing).
- **W1:** conformance macro red → SQLite impl green. **Commit old-schema fixture DBs here** with
  a provenance script (impossible to generate once legacy code is deleted). Legacy world =
  schema version 0.
- **W2:** service invariants + drift-guard Drop; **differential harness** — replay golden
  invocation sequences through legacy `app::run` and the new services, assert identical
  `Response` (strongest available compat proof; enabled by the envelope PR).
- **W4/W5:** the two flips each get their own proof: W4 leans on the characterization suite +
  W3 round-trip rollback; W5 adds the in-process-vs-RPC byte-comparison test and runs the
  integration suite **both ways** (`make test-daemon`; `--local` / `STORYHOOK_INVOKER=local`
  stays a supported mode permanently, so the second leg is not throwaway).
- **W5 SSE:** write push tests red *before* deleting the notify watcher;
  `no_filesystem_watcher_remains` (touch a repo file → assert no event) proves deletion, not
  disuse.
- **W6:** wave-0's red worktree test family extends to hooks: fire from subdirectory, from
  linked worktree, two worktrees resolving to one project id, cross-checkout visibility.
- **W8:** kill -9 matrix via **compile-time-gated fault injection points** (`before_commit`,
  `after_commit_before_ack`, `mid_read_model_update`, `mid_migration`) — designed in at W1,
  never present in release builds. Key cases:
  `sigkill_after_commit_before_ack_does_not_report_false_success` (no lost acks),
  `concurrent_daemon_starts_migrate_exactly_once`, `recycled_pid_does_not_hijack`, disk-full via
  `PRAGMA max_page_count`.

**Property-based testing: proptest** (persistent `proptest-regressions/` = automated
regression-test tenet), exactly five properties: fold never panics on any event sequence
(highest value — a fold panic in a daemon is an outage, not an exit); fold deterministic; event
serde round-trip + unknown-kind contract (SH-54); read-model == rebuild under random *service
operations* (64 cases, model-driven generation); relation symmetry under any sequence.
Explicitly not: CLI parsing, rendering, routing. Budget < 20s added wall-clock; `PROPTEST_CASES`
env for nightly soaks.

**Suite budget & hygiene:** `make test` hard ceiling 180s (target 120s); no test > 5s, no binary
> 30s; `make test-timing` with fail-on->25%-regression vs the W0 baseline. Flake protocol: zero
`thread::sleep` outside bounded wait helpers (grep gate); **a flake is a P1 defect** — file a
story, fix or `#[ignore = "SH-NN"]`; **no automatic retries ever** (retries convert exactly the
race class this rearch exists to kill into noise); `make test-repeat N=20` once per wave.
`make test-hygiene` postlude: orphan processes, daemon fd counts, scratch-dir residue.

**Baseline protocol (before W1, committed to `docs/rearch/baseline/` via a committed capture
script):** test *name* inventory (a vanished test becomes a diff, not a mystery), 3-run median
timings, golden CLI corpus, golden export + legacy tarball of this repo's tree, error-code
table, current archive.db schema snapshot (documented as version 0), known-red list. Per-wave
gate: regenerate + diff; any non-empty diff explained in the PR body.

**Testability-driven design requirements (adopted into the architecture):**

1. `serve(listener: TcpListener)` not `serve(port)`; daemon binds `:0` and publishes the real
   port to the portfile (also deletes the "port taken" failure mode in production).
2. One injected `Environment { data_home, state_home, home, clock, daemon_addr }` constructed in
   `main()`, replacing ~10 scattered `env::var` sites — includes a `Clock` (makes `days_stale`
   testable for the first time).
3. `GET /api/v1/hello` (identity + version) checked before a client trusts a discovered daemon;
   version-mismatch → restart, never serve stale (the 42-hour-stale-daemon lesson).
4. Auto-spawn holds its lock through spawn **and** pidfile write (current `handle_start`
   releases early at web.rs:1839 — a real race that auto-spawn would make hot).
5. SSE publishes after commit, outside the transaction, bounded drop-oldest queue with
   **observable** drops (counter + `resync` event).
6. Rebuild-and-diff is a public service operation (doctor feature ≡ test oracle).
7. `ProjectId` newtype structurally required by the query layer — missing project scope becomes
   a compile error.
8. *(Declined)* RAII retrofit of `lock.rs`: it dies in W4; W5's request boundary gets
   `catch_unwind`, and `store.transact()` is panic-safe by construction (txn rollback on drop).

## Waves

**Standing rulings:** (a) the strangler lives at the **Invoker seam**, not a file-backed
`FsStore` — a file Store can't honestly implement transactions/CAS, so it would validate
services against a fake. `LegacyInvoker` wraps `app::run` verbatim from W0b; the suite runs both
invokers from end-of-W2 until W4; the flip is a constructor swap; app.rs is **frozen** W2–W4,
not split (kills the merge-contention risk). (b) W0 includes the four bug fixes below — the
baseline must not freeze known-wrong behavior, and SH-51/53 make the gate itself unenforceable
until fixed. (c) The bash plugin suite gets XDG isolation in W0 too — otherwise post-flip it
writes junk projects into the real data home on every `make test`.

Twelve PRs. Every commit green under `make test`; two-hats throughout.

| Wave | PR (Conventional Commits title) | Scope highlights | Size | Effort |
|---|---|---|---|---|
| **W0** | `test: repair the quality gate and unify the integration harness` | Fix SH-53 (Spotlight/$TMPDIR), SH-51 (readiness handshake + OS-assigned ports), SH-59 (errors→stderr), SH-52 (`--help` creates story; sweep sibling verbs) — each own `fix:` commit + regression test. Then the `storyhook-test-support` crate's `Project` fixture killing 32 duplicated helpers, with **unconditional** isolation of `HOME` + `XDG_DATA_HOME/CONFIG/STATE` + `STORYHOOK_DATA_DIR` (isolates vars nothing reads yet — deliberate). Baseline metrics + 10-run flake census in `docs/rearch/baseline/`. QA additions: RED `two_worktrees_mint_colliding_ids` test, insta golden CLI corpus (incl. a ≥12-story fixture to trip lexicographic-vs-numeric ordering), error-code table, snapshot of this repo's live `.storyhook/` + golden export. SWE additions: XDG isolation in `plugins/story/tests/lib.sh` + run-tests.sh assertion (mandatory); enumerate the W4 flip checklist (path-assertion tests, the raw-JSONL corruption fabricators). | L | 10% |
| **W0b** | `feat(output): wire-serializable envelope and the Invoker seam` | `Deserialize` on `Response`/view types + `Invocation`; serializable `AppError` wire form (incl. `StateConflict` payload); render-preserving round-trip tests. `Invoker` trait + `LegacyInvoker` (wraps `app::run` verbatim) adopted by main.rs and web.rs — zero behavior change. Prerequisite for differential harness + W5. Parallel with W1. | S | ~2% |
| **W1** | `feat(store): event-sourced Store trait with SQLite engine, migrations, and rebuild-diff` | Pure new code, `src/store/`, nothing wired. Trait leaks no rusqlite types. WAL, `BEGIN IMMEDIATE`. Schema enforces relation symmetry + single-parent as constraints (kills SH-60 class). Unknown-event tolerance + `schema_version` gate (kills SH-54 class). Versioned migrations w/ verified backup. `rebuild_read_model()`/`diff_read_model()` oracle. Transactional ID allocation. | L | 12% |
| **W2a** | `feat(service): story lifecycle and relation services over Store` | Additive — app.rs frozen. `invoke::dispatch` skeleton + `Ctx`; StoryService (single private fn for the state-transition batch, today duplicated 4×) + RelationService (both sides in ONE tx — kills app.rs:1492). Arms ≤15 lines: validate → one service call → view. Differential harness (golden replays legacy vs new) for lifecycle invocations. | XL | 10% |
| **W2b** | `feat(service): project, config, and system services over Store` | ProjectService (project+defaults+counters atomic), ConfigService (state/type edit + occupant migration in one tx), SystemService. Differential leg for the family. | L | 8% |
| **W2c** | `feat(service): query and integrity services; TUI onto the Invoker seam` | QueryService (takes `&impl ReadOps` — statically cannot write), IntegrityService (doctor/rebuild). Additive `Invocation::ProjectSnapshot` bulk read (one small additive app.rs arm — the sole freeze exception); TUI's ~59 storage sites → Invoker calls (LegacyInvoker pre-flip, so behavior unchanged); 30 white-box TUI tests reconstructed via the seam. | L | 8% |
| **W2d** | `feat(service): git and GitHub integration services over Store` | GitService (CommitSync, GithubSync — 24 sites in src/github/ re-pointed), Decompose, Update. Exit: dispatch covers all Invocation variants; **`make test` gains the second leg — full integration suite under `STORYHOOK_INVOKER` = new stack (test-harness-only switch, deleted in W4).** | M | 6% |
| **W3** | `feat(migrate): import legacy .storyhook projects into the store` | `src/legacy/` read-only reader (independent of storage.rs). `story migrate`. Repairs SH-60 violations on import (refuses silently-lossy). **Reverse path (store→ProjectExport→legacy) = W4 rollback mechanism — round-trip test is non-negotiable.** Parallelizable with W2. | M | 6% |
| **W4** | `feat!: move story data to a single global SQLite store` | **The flip = constructor swap.** Default invoker `LegacyInvoker` → `LocalInvoker`; `story init` writes pointer file + DB row only; ancestor-walking root resolution as its **own commit + tests** (semantics change: subdir calls start succeeding); `~/.storyhook/registry.toml` imported (MIGRATED.txt, never delete); unmigrated repos fail loud ("run `story migrate`"); delete `LegacyInvoker` + legacy test leg, `lock.rs`, `registry.rs`, storage.rs write half. CLI runs in-process; `--local` (`STORYHOOK_INVOKER=local`) is a permanent documented mode. Internal commit order: harness → additive resolution → ancestor walk → **the swap (small commit — the bisect atom)** → guard → test rewrite (`init_command.rs`, the corruption fabricators via `inject_events()`) → deletions. Pre-flip: old-vs-new `--json` diff of every command on the ≥12-story fixture (ordering drift = own labeled commits). No feature flag, no dual-mode init. | M (max risk) | 12% |
| **W5** | `feat(daemon): promote the dashboard server into the storyhook daemon with /api/v1/invoke` | **One process** — `story web *` becomes aliases (deprecation notice); tailnet guards survive verbatim. RPC **loopback-only + token**; port 3456-else-ephemeral, `daemon.json` authoritative; auto-spawn (bind-then-write under flock held through pidfile write); version/exe_mtime handshake with auto-restart, never auto-retrying mutations; `/api/v1/hello`; `story daemon install` (launchd). REST routes call services directly (deletes the web.rs:203 double-dispatch). SSE from post-commit bus + `data_version` polling (`notify` dep removed). `hook_depth` in the envelope + hook-termination test. TUI: bulk snapshot + SSE invalidation. Daily `VACUUM INTO` backups (retain 7). `make test-daemon` target = full suite over RPC. **Hard rule: zero assertion edits in the 585 tests**; in-process-vs-RPC byte-comparison test. | L | 14% |
| **W6** | `fix(git): scan full commit bodies for story references now that sync is churn-free` | SH-56 + SH-58 (`%s`→`%B`) — **gated on W4** (SH-61: fixing earlier arms the infinite churn loop). github-sync state relocated to global home (zero `.storyhook` literals left in src/github/). commit-sync idempotency becomes a DB constraint (`StoryCommitLinked` event, unique `(project, story_no, sha)`; rendering compat verified against golden corpus; own labeled commit). Termination test = SH-61 acceptance. | M | 5% |
| **W7** | `chore: migrate storyhook's own tracker and retire the .storyhook directory` | Run migrate on this repo; delete `.storyhook/` (keep pointer); update plugin bash (story.sh, post-git.sh, stop-handoff.sh, lib.sh); opportunistic SH-47, SH-48 as own `fix:` commits; docs + help_topics.rs. | S–M | 4% |
| **W8** | `test: crash, concurrency, and corruption hardening for the store and daemon` | kill -9 mid-txn, multi-client property tests, corrupted-DB recovery, doctor runs rebuild-diff, backup verification, perf vs W0 baseline. | M | 5% |

**Dependencies:** strictly serial spine `W0 → W1 → W2a → W2c → W4 → W5 → W6 → W7 → W8` (~76% of
effort). Parallelizable: W3 alongside any W2; W2b/W2d alongside W2c after W2a lands.

### Cutover risk & rollback

- **Highest risk: post-flip global-state contamination** — 44 concurrent test binaries against
  one global DB. Killed by W0's *unconditional* fixture isolation; W0's `--test-threads=1` vs
  parallel comparison is the control.
- **Rollback rules:** only the tip wave is revertible (`git revert -m 1`); main never left red
  across a session boundary; W0–W3 revert-first, W4 revert only while W3's round-trip test is
  green (recovery procedure goes in the W4 PR body verbatim), W5–W6 fix-forward, W7–W8
  revert-first. `.storyhook/` stays in the repo (unused) until W7 — that plus the W3 round-trip
  keeps W4 a two-way door.
- W4 has **no safe internal handoff point** — budget it as one uninterrupted session.

### Open-story disposition (17 open at plan time)

- **Absorbed** (fix = acceptance criterion): SH-51, SH-52, SH-53, SH-59 (W0) · SH-54 (W1) ·
  SH-60 (W1+W3) · SH-56, SH-58 (W6).
- **Obsoleted** (close on killing wave's merge, acceptance test named in close comment): SH-46
  (W4), SH-61 (W4/W6).
- **Survive independently**: SH-42, SH-43, SH-44 (web UI — defer past W5; SH-43's "archived
  state" must NOT leak into W1 schema), SH-47, SH-48 (fixed opportunistically in W7), SH-49,
  SH-50 (want the daemon — blocked-by rearch, re-spec after W5). **Pre-W0 action: mark all five
  deferred/blocked so concurrent sessions don't pick them up mid-flight.**

Defects discovered *during* execution are recorded in STATE.md's "Key facts" list and filed as
stories once the flip removes the ID-collision hazard of minting IDs from a worktree.

### Non-goals (scope tripwires)

No Postgres impl (trait must merely not preclude it) · no auth/RBAC/multi-user (tailnet +
existing trusted-host guard) · no remote hosting · no GitHub sync features beyond state
relocation · no TUI/web feature work · no perf work beyond baseline sanity · no new CLI commands
except `story migrate` + `story daemon install` · **no changes to existing
`Invocation`/`Response` variants (they ARE the byte-compat contract; additive variants allowed
only where a client needs them: `ProjectSnapshot`, `Migrate`)** · no event-schema redesign (one
additive variant permitted: `StoryCommitLinked`, W6). Tripwire: any `src/store/` table or
Cargo.toml dependency not traceable to a locked decision.

## Verification

- Every commit: `make test` (fmt, clippy `-D warnings`, cargo test, bash plugin suite). From
  end-of-W2: a second leg runs the full integration suite against the new stack; from W5:
  `make test-daemon` runs it over RPC.
- Behavior preservation, three tiers (strongest first): structural (envelope round-trip +
  client-side rendering), differential (golden replays legacy-vs-new per W2 family), behavioral
  (585 characterization tests green at every commit, zero assertion-semantics edits; insta
  golden corpus recorded before W1, `INSTA_UPDATE=no` in the gate).
- End-state acceptance: two worktrees of one repo share one truth (W0's RED SH-46 test goes
  green); N parallel `story new` yield unique IDs; `kill -9` mid-write leaves the DB consistent
  with no false-success acks; `git status` clean after exercising the full CLI surface; the
  dashboard live-updates from a CLI write via SSE with the notify watcher deleted; a hook
  calling `story` terminates (reentrancy); legacy import of this repo verifies
  counts/relations/archive equivalence and `story doctor` (rebuild-and-diff) exits 0.
- Progress tracking: per-wave HANDOFF.md, the project CLAUDE.md mini-roadmap, and
  `docs/rearch/STATE.md` track execution against this document.
- Worktree discipline: every wave ends at "PR opened" from the linked worktree; merges verified
  before the next wave begins; no version bump or deploy until the program completes and is cut
  from `main` (`/semver bump major` then — total data-layer replacement).

## Session/handoff protocol

HANDOFF.md at every wave boundary (<100 lines): wave + PR state (worktree ⇒ ends at "PR
opened", no bump/deploy), exit-criteria evidence, next entry criteria, deviations, scope-creep
ledger, story closures. Resumption checklist: confirm tip → read HANDOFF → **`make test` green
before touching anything** → `story list` → `gh pr list` → reconfirm worktree constraints.

## As built

The nine waves are recorded in [`docs/rearch/STATE.md`](../rearch/STATE.md), which stays the
document of record for them. This section carries what later work *reversed* in the design
above, so a reader of this file is not left believing a decision that no longer holds.

### `--local` was not a permanent mode (SH-114, 2026-08-02)

"**Execution modes**" above calls `--local` / `STORYHOOK_INVOKER=local` a "documented
first-class mode", recommended for git hooks and CI. It is deleted. So are the second leg of
the gate (`make test-daemon`, `make gate`) and every mention of a `--local` writer in the
change-feed and expected-sequence paragraphs.

**Why the recommendation did not survive its own reasoning.** It rested on two claims, and
measurement contradicted the first and outgrew the second:

- *"Git hooks and CI want it."* **No git hook ever used it.** `src/main.rs` documented the
  flag as existing precisely for them, and the managed hooks in `src/hooks.rs` never passed
  it. Its only live consumers were this project's own test harnesses.
- *"SQLite WAL multi-process access is its design point."* True, and still true — it is why
  the TUI may keep its own store handle. But a second *supported* route from a CLI command to
  the store is a second thing to keep honest, and the daemon-only failure mode ("the daemon
  will not start") was being papered over by the escape hatch rather than fixed. It is fixed
  now: a client reports the daemon's own diagnosis, as data, in 71ms rather than after a
  five-second timeout.

**What was given up, deliberately.** Coverage of a bare, directly-invoked process holding the
write transaction and dying — that process shape is now **unbuildable**, not merely untested.
Plus the two-transport agreement property, which the byte-comparison test proved structurally
and which `tests/golden_cli.rs`'s frozen snapshots now hold instead. Plus
`tests/concurrency_soak.rs`'s premise that two supported modes write one store at once: the
only remaining second writer is `story tui`, and moving it onto `/api/v1/invoke` is SH-150.

**What survives.** `StoreInvoker` and the `Invoker` trait, because `StoreInvoker` is the
*executor* rather than a transport: the daemon runs a client's request through it
(`src/api/rpc.rs`), and, until SH-150, so did the TUI directly. `HttpInvoker` is now every
client's only door — see below.

Design of record for the decision: SH-114's council verdict, clauses
D1–D8, unanimous.

### The TUI became a client too (SH-150, 2026-08-07)

The paragraph above, un-reversed until now, called the TUI's own store handle a consequence
of "SQLite WAL multi-process access is its design point" — true, and beside the point once
measured. Three things this design's own target architecture already called for
(`Target architecture` above puts the TUI behind the Invoker seam; the W5 row says "TUI: bulk
snapshot + SSE invalidation") had drifted from what shipped, each recorded rather than
silently kept:

- **The TUI was a second migrator.** `invoke::open_store` runs `Store::migrate` and
  legacy-registry adoption behind a pre-migration backup, unsupervised by the
  version/exe/mtime handshake (`daemon::lifecycle::usable`) every other route to the store
  passes through. An upgraded binary's `story tui` could migrate a store an older daemon was
  still holding open.
- **The write path it depended on was untested.** SH-114 gave up
  `tests/concurrency_soak.rs`'s premise that two supported modes write one store at once —
  SQLite's multi-process write path (`busy_timeout`, the `BEGIN IMMEDIATE` retry, `SQLITE_BUSY`
  reaching a user as exit code 4) kept running for the TUI and stopped being exercised by
  anything.
- **The move itself was small.** `main_loop`, `dispatch` and `DataStore::load` already took
  `&dyn Invoker`; production code under `src/tui/` touched the store on five lines total.

**What changed.** `story tui` now resolves the daemon (`daemon::lifecycle::ensure`) before
opening the alternate screen, reads and writes through `HttpInvoker` — with
`.announce_waits(false)`, since the daemon-wait notice writes to stderr and inside the
alternate screen stderr *is* the screen — and subscribes to `GET /api/events`
(`daemon::subscribe::Subscriber`, a raw-`TcpStream` SSE client rather than `ureq`: the one
body-deadline `ureq` offers bounds the whole response, wrong for a connection meant to stay
open all day) before its first snapshot load, preserving SH-140's ordering guarantee — no
write can land in a gap between "subscribed" and "loaded" — with a subscription proof instead
of a baseline read. `tests/invoker_seam.rs::the_tui_opens_no_store_of_its_own` pins it, in the
`the_legacy_write_path_is_gone` idiom.

**What did not change.** `story tui` still resolves its project from `root` alone — it never
sets `InvokeRequest.project`, so `--project`/`$STORYHOOK_PROJECT` do not apply to it. Real,
orthogonal to the transport, filed separately rather than folded in here.

**Known cost, measured rather than assumed.** A TUI mutation now costs 2–3 HTTP round trips
(an undo snapshot, the mutation, a reload) where it used to cost the same number of in-process
SQLite calls. SH-173 (concurrent dispatch) landed first and removed the amplifier that would
have made this a queuing hazard. Measured on a 100-story fixture, `story` subprocess included
(process spawn + connect + one invocation + render — not the bare round trip a long-lived TUI
process would see, since a `story` binary re-execs on every call and `HttpInvoker::invoke`'s
`lifecycle::ensure` cannot be called in-process from anything other than the real `story`
binary — its usability check compares the daemon's recorded executable against the *calling
process's own*): a `story list` over 100 stories averaged 66.8ms (min 20.2ms, max 146.5ms,
n=10, warm daemon); one `story move` measured 13.6ms. Against SH-140's in-process baseline for
the equivalent read (`DataStore::load`, 952–1003 µs), the round trip is roughly two orders of
magnitude slower in absolute terms and still comfortably sub-100ms — the cost this design
accepted knowingly, not a surprise found after the fact.

### `project_paths` was not the answer to "which project is this?" (SH-119, 2026-08-03)

The schema summary above lists `project_paths` — "a project has MANY checkouts — this kills
SH-46" — and "What dies" records `registry.rs` folding into `projects` + `project_paths`. Both
are reversed. The table, its unique index, `PathKind`, `ProjectPathRecord`, `project_by_path`,
`touch_project_path`, `forget_project_path` and `adopt_legacy_registry` are all deleted by
migration 8, and the upward walk that read them reads only the committed pointer file now.

**The half that was right is kept.** SH-46 was two defects wearing one number: a worktree
resolving to a *different tracker*, and a worktree's ids colliding with its main checkout's.
The second is killed by there being one store, which is untouched here. The first is killed by
the pointer file being **committed** — a worktree checks out the same file, so it names the
same project — which is a property of the repository rather than of an index this machine
keeps.

**Why the index had to go rather than merely stop being consulted.** It is a fact about one
machine's filesystem, and the epic's stated invariant is that nothing about the filesystem is
ever *required* to answer which project a directory belongs to. Left in place it kept two
answers alive: a checkout could carry a pointer naming one project and a row naming another,
and the resolver silently preferred one — SH-151's defect, which measured a real sub-project
in a monorepo answering for its sibling.

**What answers instead**, in order: `--project <slug>`, `$STORYHOOK_PROJECT`, the nearest
committed `.storyhook.toml` at or above the working directory, and the repository's registered
git origin. The climb stops at the first ancestor holding a `.git` **directory**, so a
directory inside one repository never inherits an identity from outside it; a linked worktree
holds a `.git` *file*, which is why it does not stop the climb and a worktree still resolves
through its main checkout.

**What it cost, and what pays for it.** A project with no committed pointer and no registered
origin is unreachable from its own directory. `story doctor` reports every project whose
checkout owns an origin nobody registered, and `--fix` records it; anything the checkout does
not own is reported and never guessed at. That is SH-151's R4, which was recorded as a
blocking acceptance criterion on SH-119 rather than assumed.

`projects.checkout_path` (migration 7) is **not** a replacement. Nothing resolves by it, two
projects may share one, and it answers a different question: where a project's repo-side work
runs.

Design of record for the decision: SH-151's council verdict,
clauses D2 and R1–R4.

### The change feed's request boundary did not cover every route (SH-202, 2026-08-08)

The W5 row above and `daemon::bus`'s own module doc described the change feed as having a
request-boundary publisher that fires "immediately after a mutating request commits" — stated
as covering `route_job_inner`'s dispatch as a whole. It covered exactly one of its two arms.
`rest::route`, the dashboard's own REST mutation surface, computed and published a precise
`Changed` signal at the boundary. `rpc::route`'s `POST /api/v1/invoke` — the *only* way an
ordinary `story` command has reached the store since SH-114 — answered and returned without
ever touching the bus. Every CLI write reached an open dashboard tab only via
`poll_change_token`'s 250ms `PRAGMA data_version` safety-net poll, the mechanism its own doc
comment named a fallback. Harmless in practice (confirmed by SH-145's own CLI-write SSE test)
but a false design claim, and a real latency and robustness gap under the poller's own
documented subscriber-count edge cases.

**The fix.** `poll_change_token`'s own attribution — read the change token, and if it moved,
diff each project's `global_seq` against a baseline to publish `Change::Project` /
`Change::Catalog`, falling back to `Change::Resync` when nothing is attributable — moved into
a new `ChangeWatcher` (`daemon::watch`), shared via one baseline mutex between the poller and
`route_job_inner`'s RPC arm, which now calls it once after an invoke answers and before the
reply is sent. A commit both notice is attributed once, not twice.

**REST does not also call it, reversing the plan approved before implementation.** The
original design had both `route_job_inner` arms call the shared watcher, for doc-accuracy
uniformity and to stop the poller redundantly re-publishing a REST change ~250ms later.
Empirically, joining REST caused two existing integration tests to fail deterministically:
an out-of-band change (an in-process store write bypassing the daemon — a `story tui` session,
a second machine — the exact scenario the safety net exists for) discovered incidentally by
the shared diff can publish first and get retained by `ChangeBus`'s *leading-edge* 200ms
coalescing, silently dropping a later, genuinely relevant precise publish for the same
project. Every mutating REST route already has exhaustive `Changed` coverage by construction,
so joining it to the diff added no coverage and only this hazard. A 3-member council
(software-architect and qa-engineer delivered independently and agreed; api-designer
abstained) confirmed narrowing the fix to the RPC arm alone, matching this story's own
originally-scoped suggested fix. Its audit trail was untracked and local to the worktree
that ran it, and is gone (SH-363); it belonged to no story, so the paragraph above is the
whole of the record — the fix is scoped to the RPC arm, deliberately.

**What survives as a known, filed gap.** The coalescing defect the council surfaced —
`ChangeBus::publish` dedups purely by `Change` value equality within its window, blind to
cause — is not fixed by narrowing the trigger to the RPC arm; it is only made unlikely rather
than removed, since the RPC arm's own `notice()` call can still incidentally discover and
publish an unrelated project's change. Filed as SH-216, out of this story's scope.
