# SH-790: Stop Now reacquired live admission for a halted run

## Failure and reproduction

In v3.0.3, Stop Now on a halted run fails while another run of the same project
is live. SH-774 identified and deferred this case. `reset_now` accepts Halted
but unconditionally writes Draining. The store's unique partial index covers
Running, Paused and Draining, so the intent transaction fails before cleanup.

The initial three service regressions reproduced the defect on unchanged
production code: two reported `UNIQUE constraint failed: engine_runs.project_slug`;
the failure/restart test without a sibling observed Draining instead of Halted.
An isolated SQLite experiment using migration 0024 independently reproduced
the constraint failure and confirmed that a halted intent can coexist with
one live run while a second live run remains rejected.

## Cause and correction

Immediate-stop ownership was equated with live-run admission. Cleanup needs
durable intent and exact resource ownership, but does not need permission to
claim new work. Halted runs now retain their state during cleanup and move
directly to Finished when all lanes are idle. The operator reason replaces
the halt reason; breaker counters and quarantine evidence remain.

A state-only fix would miss daemon retries: both engine sweeps and the change
watcher previously selected only live runs. A separate reconcilable-run query
includes halted immediate-stop intent even before a lane reset is reserved.
Admission and capacity still use the original live-only query. Restart retains
intent without helper callbacks; the steady pass retries cleanup.

## Regression coverage

- Service tests cover each live sibling state, no sibling, partial failure,
  reopened-store recovery, helper token authorization, busy controllers,
  unchanged retries, leaseless release, reserved unclaim and external reset.
- The store matrix checks every state against absent, breaker and immediate-stop
  reasons, with multiple cleanup intents, deterministic ordering and both
  read and write transactions.
- Daemon and watcher tests cover intent before reservation, startup versus
  steady cleanup, project-change attribution and no duplicate wake.
- The REST regression stops an explicitly selected halted run beside a live
  run and checks the stored and returned result.

The defect class is a cleanup transition that accidentally reacquires admission.
Keep cleanup discovery separate from admission and test both sides of that
boundary. The unique index is the invariant, not the defect.
