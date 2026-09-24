# SH-760: The test suite uninstalled the real Claude Code plugin on every run

- **Date**: 2026-09-21 21:50:43Z (the loss investigated); live since 2026-07-28
- **Severity/Impact**: Silent loss of a supported integration, repeated. Every
  run of `tests/invoker_seam.rs` — under `make test` on every gate, and under
  any bare `cargo test` that selected it — removed the developer's Claude Code
  registration of the storyhook plugin and swept its plugin cache. Under a bare
  `cargo test` it also removed storyhook's own install receipt, after which
  `story doctor install` reported `every component agrees` over a machine with
  no `/story` command and none of the plugin's Claude hooks. No store data was
  touched.
- **Status**: Fixed on `worktree-SH-760`. The loss was filed three times
  before this (SH-640, SH-671, SH-760) and attributed to Claude Code each time.

## Reported failure

SH-760, filed 2026-09-23 05:21Z: the storyhook plugin was not registered in
Claude Code, the Codex registration was intact, and the Claude install receipt
at `~/.local/share/storyhook/provider-installs/claude` — the evidence SH-671
added so that the doctor could tell a lost registration from a machine that
never had one — was gone too. The `provider-installs/` directory's mtime was
2026-09-21 14:50:44 local. No `story plugin uninstall` appeared in any Claude
or Codex transcript, in the shell history, or in the store's event log. The
story asked for the actor, a failing test, and a doctor that this cannot defeat.

## Evidence

Sources: the snapshot at `~/Enderchest/storyhook/claude-registration-loss/`,
the Codex session transcripts under `~/.codex/sessions/2026/09/21/`, the
daemon activity log, the store's `events` table, the verifier's gate receipts
under `.git/storyhook/gate-receipts/`, and git history. Times are UTC.

| Time | Fact | Source |
|---|---|---|
| 2026-07-28 | `tests/invoker_seam.rs` gains a roster of project-less verbs that it executes in-process; one is `plugin uninstall claude` | `git log -S`, commit 5ff6d774 |
| 2026-09-21 14:34:16 | Mikey runs `story plugin install claude` from a shell | zsh history; the Codex receipt beside it is stamped 14:33:56 |
| 2026-09-21 20:52 | A Codex session starts on story SH-752 in `.codex/worktrees/SH-752` | session `01a0c5be-2fc5…` |
| 2026-09-21 21:50:02 | That session runs `cargo test --no-fail-fast --test daemon_invoke --test invoker_seam --test wire_envelope --test mcp_server --test mcp_tool_drift` — a bare `cargo test`, no `scripts/run-tests.sh` | same transcript |
| 2026-09-21 21:50:43 | `~/.claude/plugins/data/` is modified: Claude Code's own `plugin uninstall` removes the plugin's data directory | snapshot `state.txt` (mtime 14:50:43 local) |
| 2026-09-21 21:50:44 | `~/.local/share/storyhook/provider-installs/` is modified: the receipt is removed | snapshot `state.txt` (mtime 14:50:44 local) |
| 2026-09-21 21:55 | SH-752 moves to verifying; the verifier's `make test` starts | store `events`, daemon log |
| 2026-09-23 05:15 | The loss is noticed and filed | SH-760 |

The two mtimes, one second apart, are the exact order of
`plugin::uninstall_claude`: the provider's own uninstall first, then the
marketplace removal, then the residue sweep, then the receipt. Nothing else in
this tree removes the receipt. No transcript, shell, hook, script or store event
ran the verb at that moment — because it ran inside a test binary, where
nothing logs it.

Why a bare `cargo test` lost the receipt while `make test` had not: the
environment table in `scripts/test-env.sh` scopes `HOME` as `story`, so a
wrapper around `cargo` never redirects it (cargo would lose its registry and
build cache); only `STORYHOOK_DATA_DIR` is exported for the whole run. An
in-process call is not a `story` child, so it keeps the real `HOME` under both
harnesses. Under `make test` the receipt lived in the isolated data directory
and survived; the registration and the cache did not, which is the
`DEREGISTERED` shape SH-671 saw. Under the bare `cargo test` nothing was
isolated at all.

Why the provider call reached the real Claude: `run_provider` rebuilds the
child's environment from an allowlist (`COMMON_MAY_SEE` in
`src/env/spawn_env.rs`), and that list carries `PATH` and `HOME`. The
verifier's `PATH` includes `~/.local/bin`, where `claude` lives.

## Root cause & trigger

The verified chain:

1. `the_project_less_verbs_all_answer_outside_a_project` drives
   `Invocation::Plugin { Uninstall { "claude" } }` through `StoreInvoker`
   in-process. Its only assertion about the answer is that it does not say
   "not initialized in this directory", so success was a pass.
2. `dispatch_unscoped_with_stdin` routes the verb to `plugin::uninstall`,
   which is project-less by design and consults nothing about *who* is asking.
3. `uninstall_claude` runs the real `claude plugin uninstall story@storyhook`
   and `claude plugin marketplace remove storyhook` under the test process's
   `HOME`, sweeps `~/.claude/plugins/cache/storyhook` and
   `~/.claude/plugins/marketplaces/storyhook`, and removes the receipt at
   `plugin::data_dir()`.
4. `story doctor install` reads no registration, no residue and no receipt as
   a provider that was never installed here (`Row::ok("not registered")`),
   which is the correct reading of that evidence and the wrong reading of the
   machine.

The trigger was a test exercising a destructive project-less verb in-process.
The defect is the missing invariant: nothing in the tree said that a build
which is not the installed `story` must not manage a provider's registration
— the same shape SH-404, SH-411 and SH-634 closed for the store's schema, the
login agent and the daemon seat.

ODC classification: **Checking / Missing / guard absent on a mutating verb
reachable in-process**.

## Why it was misread three times

- **SH-640 (2026-09-09)**: the cache survived, the registration did not. At
  the time `uninstall_claude` did not sweep residue — SH-640 itself added the
  sweep — so the leftovers matched a provider-side loss, and the mtimes cited
  were read as a Claude Code marketplace refresh.
- **SH-671 (2026-09-10)**: the cache went too. Attributed to Claude Code
  2.1.268 sweeping it; it was the sweep SH-640 had just added, run by the same
  test. The receipt was designed to survive "the provider taking everything",
  and it did — until a bare `cargo test` ran with no data directory.
- **SH-760**: the receipt was gone, so the doctor had nothing to flag.

Each fix made the doctor better at reading the evidence the test left, and the
test kept running. The lesson is the one `docs/spec/test-environments.md`
already carries for the store: isolation applied to children does not reach a
process that calls the library directly, and a verb that can reach the
operator's home must refuse an uninstalled build itself.

## Fix

- **`src/plugin/guard.rs`** — the fourth guard on `path_identity::build_dir`.
  `story plugin install|uninstall|reinstall` is refused, before any provider
  call or file write, to a test build (the `fault-injection` feature every
  `cargo test` artifact carries) and to a binary still in its build directory,
  unless `STORYHOOK_ALLOW_UNINSTALLED_PLUGIN_INSTALL=1` says the home is the
  operator's to change. The variable is cleared by the environment table and
  is *not* re-armed by `--uninstalled-build`, because `make scratch` keeps the
  real `HOME`. Harnesses that own an isolated home set it on their children.
- **`src/plugin/receipt.rs`** — the receipt is never removed. A deliberate
  uninstall rewrites it as a tombstone recording when, by which executable,
  what kind of build that was, and whether the override was set. The doctor
  stays quiet over a tombstone from an installed binary or an overridden one,
  and flags `UNINSTALLED BY AN UNINSTALLED BUILD` over one from a test or
  checkout build with no override — the guard being bypassed. Receipts written
  before this read as installed, unchanged.
- **`tests/invoker_seam.rs`** keeps the roster case and pins that it is
  answered by the guard's refusal. **`tests/plugin_guard.rs`** proves the
  refusal end to end for the test binary and for a copy of it outside the
  build directory, with a fake `claude` whose invocation log must stay empty,
  and the override as the control.

## Lessons

- A harness that isolates children proves nothing about in-process tests. A
  destructive verb that can be reached in-process needs its own refusal.
- A detector that reads evidence the actor leaves is only as good as the
  actor's leftovers. The receipt now survives because the only process that
  can rewrite it is the one doing the removing, and it writes who it is.
- When the same loss is filed repeatedly and the attributed cause is external,
  look for the thing this project runs on every gate.

## What this does not cover

- A daemon spawned before the override was exported keeps refusing; the
  message says to restart it. A daemon spawned with it keeps permitting until
  restarted.
- `make scratch --isolate-home` shells are refused the verbs unless the person
  exports the override; the parameter table forbids a tracked script doing it
  for them.
