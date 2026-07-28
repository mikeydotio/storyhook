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
| W0 gate repair + harness | `rearch/w0-gate-repair` | **IN PROGRESS** — step W0.1 next |
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
- **W0.1 NEXT** — four gate fixes, each own `fix:` commit + regression test (red→green):
  SH-53 (Spotlight/$TMPDIR), SH-51 (port allocation + readiness + orphan class), SH-59
  (errors→stderr), SH-52 (`--help` creates story + sibling-verb sweep).
- **W0.2** — `storyhook-test-support` workspace crate: TestEnv (unconditional HOME/XDG/
  STORYHOOK_DATA_DIR isolation), ProjectBuilder, scratch_dir + clippy tempdir ban, Daemon
  guard scaffolding, orphan postlude in make test; migrate 3 proof files
  (move_if_state, registry_test, story_export); plugin lib.sh XDG isolation + run-tests.sh
  assertion; drop lib.sh `git add .storyhook`.
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
- Baseline `make test` cold: 83s wall (230% CPU) incl. build; failed only in web_test (orphans).
- `git commit --amend` silently fails in this environment — use reset --soft + fresh commit.
- Post-commit hook scans commit SUBJECTS for story IDs — bookkeeping/story refs go in BODIES.
- Worktree branch base: main @ `838d68a`.

## Step log

- 2026-07-28 W0.0: branch `rearch/w0-gate-repair` created off `838d68a`; five stories blocked
  with rearch reasons; committed `be1601c` (subject deliberately ID-free). Task ledger created
  in session task list (#1–#14, spine dependencies wired).

## Resume protocol (fresh session)

1. `cd` the worktree; `git log --oneline -5`; read this file + HANDOFF.md if present.
2. `make test` MUST be green before touching anything (if web_test mass-fails: check for
   orphaned `web_test-*` listeners in 19xxx first — see Key facts).
3. Continue at the first non-DONE step above via a fresh subagent; orchestrator keeps its own
   context minimal (delegate reads/edits; terse reports only).
