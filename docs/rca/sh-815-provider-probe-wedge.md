# SH-815: a Codex probe that never returned stopped the verifier

## Failure

On 2026-09-26 the central verifier landed SH-799 and then stopped for the
storyhook project. It did not write the CLEANUP comment for SH-799. SH-792 and
two other stories waited in `verifying` without progress. `story verifier
status` showed only "Last evidence 6042s ago" and "owned generation … is no
longer in the verifying queue", the same text a normal conflict reconcile shows
(SH-768).

| Time (UTC) | Event |
|---|---|
| 09:11:41 | Last known good: the reap for SH-783 finishes in 18 s |
| 15:01:27 | SH-799 lands: `done`, `StoryPrMerged`, CENTRAL VERIFICATION GREEN |
| 15:01:28 | The daemon (pid 2621) starts `codex plugin list --json` (pid 6775) |
| 16:1x | The worker thread is still in `poll(2)` in `Command::output`; SH-815 filed |
| ~16:30 | pid 6775 is 1 h 26 min old. A second probe from the same daemon is 3 min old. A manual run of the same command writes nothing in 20 s and ends only on SIGKILL |

Evidence: `sample` of the daemon, with the stack reap → helper_path →
resolve_dispatch_script → codex_installed_plugin_root → run_provider →
`Command::output` → `poll`; `sample` of pid 6775, with one thread at
`_dyld_start`; `lsof` of pid 6775, showing only the stdout and stderr pipes to
the daemon; the verifier status text. Four `codex plugin list --json` launches
on the machine were stuck at `_dyld_start` at the same time, one of them under
another session's `cargo test`. A code-signing check of the quarantined Codex
cask binary is the probable cause of the stuck launch. It is not proven, and it
is outside storyhook.

## Causes

1. **No deadline.** `plugin::run_provider` ran every provider CLI through a
   bare `Command::output()`. A third-party binary that waits on a
   code-signing check, a login prompt or the network held its caller for as
   long as it waited.
2. **A needless provider dependency.** The verifier's `helper_path` and block
   delivery tried `resolve_dispatch_script(Codex)` first, and that runs
   `codex plugin list` before it reads any file. The project dispatches with
   Claude, and reap, notify and submit do not depend on the provider, so the
   probe had nothing to contribute.
3. **The guard was held across it.** The Merged arm reaps before
   `record_cleanup_*` and keeps the project's attempt guard while it does. One
   stuck child therefore stopped the whole project's verifying queue.
4. **Nothing showed the child.** `run_provider` did not use the journaled
   spawner, so `story daemon logs` had no "process started" line for it. The
   status text for a stuck post-landing hold was the same as the text for a
   normal reconcile.

## Fix

- `plugin::provider_cli` runs every provider CLI through `crate::process`,
  with `PROVIDER_CLI_TIMEOUT` (60 s), then SIGTERM, then SIGKILL of the
  process group. It journals the start, the timeout and the finish, and
  refuses an answer longer than 8 MiB rather than parse a cut one.
- `api::dispatch::resolve_control_script` finds the helper for the daemon's
  control verbs from files first. Codex's registry is the last candidate and is
  bounded.
- `codex_installed_plugin_root` returns `Result`, so a failed probe is named
  instead of being read as "no plugin".
- The detector for a guard held after landing is SH-768's Cleanup
  reservation, which warns after a bound. That bound is true only because of
  the two changes above.

## What now guards the class

- `src/plugin/provider_cli.rs` tests: a provider that never answers, with a
  grandchild holding its output, ends within deadline + grace + a named load
  margin, and leaves no process behind. A mutation that loosened the deadline
  failed the test.
- `tests/block_delivery.rs::block_delivery_never_waits_on_a_provider_cli_to_find_its_helper`:
  a daemon with a `codex` that never answers delivers a block interrupt and
  never starts `codex`. Before the fix it failed with "codex was asked: plugin
  list --json".
- `tests/spawn_inventory.rs` asks every new spawn site what ends its wait.
- Tests that used to run this machine's real `codex` now use a double or an
  injected probe.

## Not changed

Local `git` queries on daemon paths remain without a deadline. They follow the
documented `env::git_env` rule: a caller that can reach a remote adds one.
SH-816 subsequently added short, shared result caching for engine reconciles
and dispatch options while retaining the bounded authoritative registry probe.
See [Codex helper resolution](../spec/codex-helper-resolution.md).
