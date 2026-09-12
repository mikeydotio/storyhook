# An exited gate's test orphan read as an ambiguous writer and halted the verifier for the boot

- **Date**: 2026-09-12 UTC
- **Severity/Impact**: the central verifier's whole queue halted twice on one red rust-suite leg (incidents 2:33129 and 2:33168, PR 792, SH-693). A legitimate RED verdict was reported as an infrastructure halt, and after the operator acknowledged it every further gate on the machine was refused until the owner record was hand-repaired. No data lost; the queue was stalled for roughly forty minutes.
- **Status**: fixed on story SH-695 (`scripts/verifier-owner.py`); the gate-kill design of SH-683/SH-686/SH-687 is unchanged.

## Summary

`verifier-owner.py` supervises the gate command in its own session and, when the command exits, takes a census of that session to prove quiescence. Three hook tests deliberately leave a `sleep 300 &` grandchild behind (`tests/event_hooks.rs`, `tests/hook_bounds.rs` twice), and hooks are spawned without setsid, so the orphan sits in the gate session. A green rust-suite leg is followed by minutes of other legs and the sleep is dead by teardown; a red leg tears the gate down while it is alive. The supervisor refused at once, with no grace and no signal, and its refusal skipped the bookkeeping that clears `gate_started` in the owner record. The outer supervisor's census includes the recorded gate session, so it refused too, and its `run-json` path discards the child's buffered verdict on any refusal: verify-pr.sh's RED became `infrastructure-failure/permanent`, which the daemon always turns into a whole-queue halt. On the next attempt the record still said `gate_started=true` on the same boot, which the design reads as an interrupted gate of unknown state and refuses until a reboot or a hand edit.

## Timeline

- **18:25:17Z** — rust-suite leg exits 101 (golden_cli snapshots, SH-690's block-delivery comments). The gate session census finds `[65329, 68406, 68421]`; 65329 is the `sleep 300` from `tests/event_hooks.rs:397`. Halt 2:33129, "still has live writers".
- **18:29:32Z** — the sleep dies on its own. Nothing looks.
- **18:53:25Z** — the operator acknowledges 2:33129. The next run is refused: "interrupted arbitrary gate has ambiguous ownership on this boot". Halt 2:33168.
- **18:57:57Z** — SH-696 filed (later merged into SH-695): diagnosis, both mechanisms, the operator paths.
- **19:10:07Z** — operator recovery, path B: quiescence re-checked by census, the checkout, administration, lease and lifecycle records preserved together, `gate_started/gate_session/gate_supervisor` cleared through `verifier_state.save`, acknowledgement. The verifier admitted a new run at 19:10:54Z.
- **2026-09-12** — SH-695 lands the fix.

## Root cause & trigger

`execute()` had two exit outcomes: an empty census, or an immediate `Refusal`. The cancellation path already knew how to settle a session (TERM the confirmed members, wait a grace derived from the cleanup budget, KILL at the deadline, refuse only if members outlive a further eighth), but the exit path never used it. The refusal propagated out of gate mode past the record bookkeeping, so the record could not distinguish "leader exited, survivors being reaped" from "gate interrupted, state unknown", and the same-boot admission rule — correct for the second — was applied to the first.

The trigger is ordinary: a red test in the same leg as a test that leaves a long-lived grandchild. Nothing about it is a fault of the story under test.

ODC classification: **Algorithm / Missing / teardown**. The quiescence proof handled cancellation and the healthy case but not the third case, an exited leader with orphans.

## Contributing factors

- The owner record carried no exit evidence for the gate leader, so nothing the tool could re-check distinguished the two states; only a boot change or a human could.
- The outer `run-json` path replaces the child's verdict with its own refusal, so the inner RED was not merely mislabelled but lost.
- `merge-watch.sh` recorded the inner refusal's exit status 1 as the gate's own status, so even the inner classification would have named the wrong exit code.
- SH-692's gate-kill test, written the same day, deliberately used a busy loop "rather than `sleep`: the gate must have no grandchild for the owner to count as a live writer once the leaf is gone" — the edge was known and worked around rather than fixed.

## What now guards the class

- `scripts/tests/test_verifier_lifecycle.py`: an exited gate with a `sleep 300` orphan is `tests-failed`, the orphan is reaped, the record is clean and the next admission succeeds; a TERM-resistant orphan is killed at the grace; a recorded leader exit with a quiet session admits and with a live session still refuses; malformed leader-exit pairings are refused as incomplete identity; the outer settles a dead supervisor's gate session and the next run is still refused as interrupted; the exited leader stays a zombie until its session is quiet; an invalid cleanup budget refuses before the record says a gate started.
- `tests/merge_gate.rs::a_red_gate_whose_test_left_an_orphan_is_red_not_infrastructure`: the production `verify-pr.sh --run-gate` seam, twice on the same poller.
- The reap is loud without being fatal: one `verifier-owner:` line in the attempt log names the session, the exit code and the survivors.
- Mutation-checked: the immediate refusal restored, admission ignoring the recorded exit, admission ignoring the census, the leader reaped at observation, the budget validated after the record write, and the outer never signalling a dead supervisor's gate session each turn exactly their test red (recorded in the test file headers).

## Lessons

- A supervisor that can only say "quiet" or "refuse" will refuse over things it could settle. Every refusal should first ask whether the tool can establish the evidence itself.
- A refusal raised past the bookkeeping that would have recorded what happened converts a transient condition into a permanent one. Record first, then refuse.
- A test that leaves a process behind on purpose is a fixture the infrastructure must tolerate, not a bug to shorten.
