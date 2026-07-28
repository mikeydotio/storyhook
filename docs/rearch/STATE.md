# Rearchitecture Execution Ledger

> **Purpose:** continuity record for the data-layer rearchitecture. Any session resuming this
> program reads this file first, then the approved plan. Update + commit after EVERY step.
>
> **Approved plan:** `/Users/mikey/.claude/plans/help-me-plan-a-toasty-pelican.md` (source of
> truth until W0 commits it as `docs/spec/data-layer-rearchitecture.md`).
> **Worktree:** `/Volumes/Code/mikeyward/storyhook/.claude/worktrees/rearch` (linked worktree —
> waves end at "PR opened"; NO version bumps, NO deploys, NO force-push, NO touching main).
> **Execution model:** orchestrator (main session) spawns one subagent per step, sequentially.
> Each subagent: does the step TDD-style, commits (story IDs in commit BODIES only, never
> subjects), updates this file's Step Log + status table, includes it in its final commit,
> runs `make test`, reports tersely.

## Wave status

| Wave | Branch/PR | Status |
|---|---|---|
| W0 gate repair + harness | `rearch/w0-gate-repair` | **IN PROGRESS** — step W0.3 next |
| W0b envelope + Invoker seam | — | pending (after W0) |
| W1 store engine | — | pending |
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
- **W0.3** — RED `two_worktrees_mint_colliding_ids`; insta golden CLI corpus (~200 invocations,
  ≥12-story fixture, timestamp redactions, INSTA_UPDATE=no in gate); error-code table;
  export-idempotency tests.
- **W0.4** — baseline capture: committed script → docs/rearch/baseline/ (test-name inventory,
  3-run median timings, golden export + legacy tarball of this repo's .storyhook, archive.db
  schema snapshot as "version 0", known-red list); 10-run flake census.
- **W0.5** — commit plan as docs/spec/data-layer-rearchitecture.md; CLAUDE.md mini-roadmap;
  W4 flip checklist enumeration (path-assertion tests + ~8 raw-JSONL corruption fabricators);
  HANDOFF.md; open PR `test: repair the quality gate and unify the integration harness`.

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

## Resume protocol (fresh session)

1. `cd` the worktree; `git log --oneline -5`; read this file + HANDOFF.md if present.
2. `make test` MUST be green before touching anything (if web_test mass-fails: check for
   orphaned `web_test-*` listeners in 19xxx first — see Key facts).
3. Continue at the first non-DONE step above via a fresh subagent; orchestrator keeps its own
   context minimal (delegate reads/edits; terse reports only).
