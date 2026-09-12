# The block-delivery worker wrote on an idle daemon and killed every armed crash case

- **Date**: 2026-09-11 PDT / 2026-09-12 UTC
- **Severity/Impact**: `dev` red from ac7f2aef2 until the fix landed. `tests/fault_injection.rs` (1 of 3) and `tests/crash_matrix.rs` (8 of 13) failed on every gate run against that base, so no story could be certified. No user, runtime, or data impact.
- **Status**: Fixed in `3ff9d9c00` (worker) and `85af3bd32` (harness diagnosis); story SH-693.

## Summary

SH-690 (`aea70c156`, merged as PR #791 at `ac7f2aef2`) added a daemon worker, `src/daemon/block_delivery.rs`, whose `recover()` and `process_one()` each opened a write transaction on every pass — including a fresh daemon's first pass with nothing to recover and nothing pending. Every store fault point fires inside every commit, an empty transaction included. A daemon armed at `before_commit` for a client's single command was therefore killed by its own housekeeping a few milliseconds after publishing its portfile, before it accepted a connection. The crash harness, which waits on the daemon's port without looking at the daemon, reported this as a port that "never began accepting connections" and pointed the reader at the machine. The fix reads first and writes only for deliveries that exist; the harness now reports a daemon that dies before serving as exactly that.

## Timeline

- **2026-09-11 18:57:39 PDT** — `aea70c156` (SH-690) creates the worker and spawns `block_delivery::poll` from `serve()`.
- **2026-09-12 02:11:30 UTC** — SH-690 submitted; the verifier adopts PR #791. The gate log records `an_armed_daemon_dies_by_sigkill_rather_than_by_its_own_abort ... FAILED` before the gate is terminated (exit 143).
- **2026-09-12 02:18:16 UTC** — PR #791 merged as `ac7f2aef2`; SH-690 closed at 02:18:27 UTC with no verdict recorded. (SH-692 is the story of that gate.)
- **2026-09-12 04:09 UTC** — SH-692 filed; its bisect places the failure at `ac7f2aef2` with `dd67d579b` last good and names `recover()`'s unconditional write.
- **2026-09-12 17:52 UTC** — SH-693 filed for the defect itself, at high, because a red base makes every gate red.
- **2026-09-12 18:08 UTC** — RED measured in the SH-693 worktree: 2 of 4 `fault_injection`, 8 of 13 `crash_matrix`, and both new idle pins fail; the write-still-happens pin passes. The failure text is the harness's 5 s port bound, not the SIGABRT the story predicted.
- **2026-09-12 18:11 UTC** — `3ff9d9c00`: read-before-write in both functions, three in-process pins, and the idle-armed-daemon class detector. 14/14, 13/13, 4/4.
- **2026-09-12 18:16 UTC** — `85af3bd32`: `crash_the_daemon` polls the armed child while waiting for its port and reports an early death with the reading and the evidence; regression provoked through `mid_migration` over the committed v1 store.

## Root cause & trigger

`recover()` opened `store.write` and only inside it scanned for `Attempting` deliveries; `process_one()` opened `store.write` and only inside it scanned for `Pending` ones, returning `Ok(None)` from within the transaction when there were none. `SqliteWriteTx::commit` calls `fire(FaultPoint::BeforeCommit)` unconditionally, and `SqliteStore::write` commits even an empty closure, so "opened a write transaction" and "an armed `before_commit` fires" are the same event. The trigger is the crash harness's arming of `STORYHOOK_FAULT` in a hand-spawned daemon: the worker's first pass reached the point before the client did.

The defect also had a cost with no fault armed: `BEGIN IMMEDIATE` against every client once per second, on a daemon with no work, in a module whose first line promises it holds no database lock.

ODC classification: **Checking / Missing / start-up + fault injection**. The "is there anything to do?" check existed but was placed inside the transaction it should have guarded.

## Contributing factors

- The gate that would have refused the merge was terminated mid-run and the merge proceeded anyway (SH-692). The failure was in its log.
- The harness's two readiness waits (`port_of`, `wait_for_server`) never looked at the armed child. A daemon dead by its own fault read as a slow port, and its message named `target/debug/deps` and FSEvents as the usual suspects. One case got far enough for the client to auto-start a successor and reported "a different daemon identity claimed the lock" instead. SH-528 had built the diagnosis for a daemon that *does not* die; nothing existed for one that dies too early.
- The story's own WHY paragraph predicted "SIGABRT or a fault that never fired", reproducing the misdirection: the actual signature was neither.

## What now guards the class

- `tests/block_delivery.rs`: `recovery_with_nothing_interrupted_opens_no_write_transaction`, `an_idle_delivery_pass_opens_no_write_transaction` (in-process `before_commit` armed to fail: `Ok` proves no transaction), and `recovery_still_writes_when_a_delivery_was_interrupted` (the write path is still taken when there is work).
- `tests/fault_injection.rs::an_armed_daemon_left_idle_is_not_killed_by_its_own_housekeeping`: any poller, present or future, that writes on an idle pass turns this red with a message that names the finding. The window is `3 × block_delivery::IDLE_POLL`, the shortest cadence the daemon runs.
- `crash_the_daemon` reports a daemon that dies before serving, with the reading (SIGKILL means the point was reached, just not by the command), the two paths that reach a point without a client, and the daemon's stderr. Pinned by `an_armed_daemon_that_dies_before_serving_is_reported_as_such`.
- The invariant is written where it is enforced: the module doc of `src/daemon/block_delivery.rs`, and the As-built section of `docs/spec/block-interruption.md`.

## Judged and not adopted

- `VerificationQueue::upsert_generation_comment` opens `write_stories` and only then finds `events.is_empty()`. Its Queued and Running bodies embed the tick timestamp, so those writes are real; the empty write occurs only for a queue stalled behind a verifier incident, on a 1→2→5→10-minute ladder. No idle-daemon exposure. Left as recorded on SH-693.
- `engine::transition` via `apply_breaker` writes per live run per tick with the changed-check inside the transaction. A live run is the work, and the in-transaction compare is the state machine's atomicity guarantee. Left as recorded on SH-693.
