# Idle-machine bounds turned a utility-QoS gate into false reds in the tmux and daemon contract tests

- **Date**: 2026-09-18 to 2026-09-26 UTC
- **Severity/Impact**: `dispatch_tmux_context` and `verify_window` failed on trees that did not touch them. Each false red returned unrelated work to its author and cost about one hour of central verification (SH-774 lost a round). Nothing stored was wrong.
- **Status**: fixed on story SH-806 (`crates/storyhook-test-support/src/load_grace.rs`, the two harnesses, seven sibling waits). Three findings were filed as their own stories: SH-808, SH-809 and SH-810.

## Summary

Every bound that failed was sized for an idle machine at default QoS. SH-785 runs each verifier gate under `taskpolicy -c utility`, so every process a test starts (tmux servers and clients, daemons, python) loses CPU and I/O to default-QoS work. The tests became stricter than production: the daemon runs its tmux clients at default QoS (SH-784), and it retries what the tests required to succeed at the first attempt. Three mechanisms:

1. **Fixture readiness was taken from existence.** Both harnesses declared a fixture tmux server ready when its socket file appeared. tmux binds before it listens and initialises before it serves, so the first client request paid for server startup. In `verify_window` that request ran inside the production reconciler's 3 s per-call bound.
2. **Harness patience was a fixed idle bound.** The engine child waited 10 s for an answered liveness probe while each probe may take `TMUX_TIMEOUT` (3 s, clock including spawn), and the hand-spawned daemon had 10 s to publish its portfile.
3. **One tick was required to succeed.** The view harness required every reconciler tick to succeed, where the daemon treats a failed tick as one WARN and retries it 5 s later.

## Timeline

- **2026-09-18**: PR 839: the engine test reads one `Unanswered` probe as wrong routing. SH-740 keeps the production timeout and adds a 10 s harness retry loop (78306a6d).
- **2026-09-26 01:58**: SH-785 lands: gates run at utility QoS.
- **2026-09-26 12:38**: PR 868 (SH-774) at load 31-38 fails all three tests. The failure of the view cases prints the composed program but not the reconciler's stderr.
- **2026-09-26 13:30**: after the harness began carrying stderr, one of five utility-QoS runs at load 46-60 captures the cause: `tmux list-sessions ... timed out after 3 seconds`, on the first reconcile after `setUp`. This matches both view failures in the gate.
- **2026-09-26 13:50**: at load 136-167, a run shows a fourth mechanism in the engine test: the pane's identity was read before its shell handed off to the final program (`zsh`, then `bash`).

## Root cause & trigger

The bounds were **chosen for the machine as it usually is**. The gate at utility QoS is the machine at its worst. ODC: **Timing/Serialization / Incorrect / test harness**. The production timeouts are not the defect: an `Unanswered` probe is no evidence to the engine, `kill_window` has no production caller, and the reconciler's failed tick is retried. Each harness demanded a result sooner than production promises one.

The trigger was ordinary: several worktree sessions and the central verifier on one 10-core machine, with the gate clamped to utility QoS.

## Contributing factors

- `CalledProcessError` names the argv (the whole composed reconciler) and drops stderr, so the gate log could not say why the reconcile exited 1.
- The daemon's stderr went to `/dev/null`, so a late publication could not say which startup phase it had reached.
- Rust had no port of the SH-347 policy. Each Rust test chose its own fixed bound (5, 10 or 20 s for the same daemon-publication event).
- A pane reports what its shell runs *now*. The fixture read the pane's identity once, early, and pinned the engine's identity check to it.

## What now guards the class

- `storyhook_test_support::load_grace` ports SH-347 to Rust in-process (`getloadavg`, no spawn). The idle value is unchanged at or below one runnable thread per core. The grace re-samples at expiry and only grows, stays within 15 minutes, is reported on stderr, and uses test-local samplers instead of a global knob. `tests/timing_assertions.rs` allows only that module to read the load average and pins its ceiling to the Python port's.
- Readiness means the fixture server answers `display-message -p '#{pid}'` with its own pid. Regressions in both harnesses: a socket that is bound but never listens is not ready.
- `engine_waits_for_an_answer_as_long_as_contention_grants` delays 5 probes past `TMUX_TIMEOUT` and states a contention floor for its own child only. It was red on the old loop with the gate's exact message. The occupant settles before its identity is read, and a delayed exec makes that hand-off certain on every run.
- The view harness runs a tick again only for the exact timed-out-client line (with `TIMEOUT` read from the shipped script), and only under measured contention. At idle, a 3 s tmux call still fails at once.
- Every wait on a hand-spawned daemon goes through `load_grace`, and the stale `PORTFILE_DEADLINE` doc is corrected. The constant itself stays ungraced, because the crash harness uses it as a proof ceiling.

## Lessons

- A test run at a lower QoS than the code it exercises is stricter than production. Give the harness the patience that production's own caller has, and no less.
- A file that exists, a window ID that was returned, a pane that was created: none of these is a process that is ready. Wait for the answer the test needs, from the process that has to give it.
- A failure report that names the argv and drops stderr hides the one line that would have settled the diagnosis.
