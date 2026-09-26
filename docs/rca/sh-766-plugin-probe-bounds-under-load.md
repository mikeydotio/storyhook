# A 5 s ps bound failed interrupts and the interruption contract test under machine load

- **Date**: 2026-09-14 to 2026-09-26 UTC
- **Severity/Impact**: `tests/block_interrupt.rs` `native_interrupt_quiesces_gate_and_preserves_session` returned six stories red from central verification (SH-720, SH-727, SH-729, SH-730, SH-760, SH-764) on trees with no defect. In production, block-delivery interrupts MT-32 #92, MT-20 #32 and WT-1 #73/#75 ended Unreached (SH-772). A latent path could leave captured processes stopped.
- **Status**: fixed on story SH-766 (`plugins/story/lib`, the notify and readiness fixtures, a fence). SH-798 tracks the one helper left out: `continuation_runtime.py`.

## Summary

The pane helpers shared one probe function, `stop-dispatch-pane.py` `run()`, with a bare `timeout=5`. Its `processes()` census (`ps -axo pid=,ppid=,stat=,lstart=`) takes about 20 ms at idle and more than 5 s at load averages of 300 or more on 10 cores. The helpers took a census for every liveness check of a PID they had already captured, including every 50 ms poll of their waits. So one slow census failed the operation while its caller still had most of a 45 s budget.

## Root cause & trigger

1. **Liveness through a census.** `same_process(table, pid, identity)` needed a fresh census and then read the native start token. The native token alone identifies the incarnation. The census added only a whole-second start spelling and a spawn. ODC: **Algorithm / Extraneous / probe**.
2. **Bare per-probe bounds.** No bound was tied to a caller budget: `timeout=5` in three helpers, 10 s in two more, and `monotonic() + 5` waits that were judged before a fresh observation.
3. **A `finally` that needed a census.** Each helper took a census before it sent SIGCONT to the processes it froze. When a census had just failed, that census usually failed too, and the processes stayed stopped. A stopped gate holder keeps the machine lock. No sighting is recorded; it was found by reading the code.

The trigger is ordinary: several worktree gates and the central verifier on one machine.

## Contributing factors

- The fixtures were stricter than production: 25 s and 15 s per `notify` against production's 55 s, a waiter with `--max-wait 15`, and a 30 s fake pane that died during 180 readiness polls.
- `plugins/story/tests/test-dropped-cleanup-pane.py`, the direct suite for one of the helpers, never ran: no gate runner named it.
- Nothing fenced bare bounds in the shipping plugin helpers; the SH-698 scan read only `scripts/tests`.

## What now guards the class

- `alive()` and a shared `signal_known()` use native identity only. Regressions: `target()` binds while the census raises, a frozen child is continued while the census raises, a stale incarnation is never signalled, and a zombie reads as gone (`tests/support/interrupt_capture.py`). A failed census after freezing leaves nothing stopped (`test-dropped-cleanup-pane.py`, `test_pane_processes.py`). Each case was red before the fix.
- `probe_budget.py`: one 30 s budget for each helper operation. A slow-census case completes a stop whose census takes 5.5 s. With the old bound restored it fails with the production message ("Command ... timed out after 5 seconds").
- Rust unit tests pin the budget to at most two thirds of `NOTIFY_TIMEOUT` and `CLEANUP_HELPER_TIMEOUT`.
- `tests/timing_assertions.rs` `no_plugin_helper_bounds_a_probe_with_a_bare_literal`. Mutation-checked: the old `stop-dispatch-pane.py` turns it red.
- `tests/gate_tiers.rs` `every_plugin_python_suite_is_named_by_a_gate_runner`.
- The fixtures derive patience from the production bounds and grace it with `load_grace.patience()` (SH-347).

## Lessons

- Before bounding a probe, ask whether the question needs the probe at all. Here most calls asked about a process that was already identified.
- Cleanup that must run after a failure cannot depend on the operation that just failed.
- A fixture that is stricter than production tests the machine, not the code.
