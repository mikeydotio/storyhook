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
| W0b envelope + Invoker seam | `rearch/w0b-envelope` | **PR OPENED** — awaiting merge |
| W1 store engine | — | **entry-ready** |
| W2a/b/c/d services | — | pending |
| W3 importer | — | pending |
| W4 THE FLIP | — | pending (one uninterrupted session; revert only while W3 round-trip green) |
| W5 daemon | — | pending |
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

## Key facts discovered (do not re-derive)

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

## Resume protocol (fresh session)

1. `cd` the worktree; `git log --oneline -5`; read this file + HANDOFF.md if present.
2. `make test` MUST be green before touching anything (if web_test mass-fails: check for
   orphaned `web_test-*` listeners in 19xxx first — see Key facts).
3. Continue at the first non-DONE step above via a fresh subagent; orchestrator keeps its own
   context minimal (delegate reads/edits; terse reports only).
