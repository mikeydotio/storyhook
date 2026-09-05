# SH-556 Verification Handoff

## Delivered

- The centralized verifier shares the daemon's existing `InFlight` registry.
- An accepted candidate publishes `verify:<project>:<story>:<generation>` with
  project, PID, checkout, start time, and the existing gate deadline.
- Lifecycle and SH-549 activity guards remain live through outcome recording
  and story-state transition, then retract on result, error, or panic.
- Malformed and cleanup-only candidates retain their existing unowned paths.
- Graceful replacement drains active verification; crash/force recovery still
  harvests stale work and reapplies current queue priority.
- Adopted the same missing-ledger defect in synchronous dashboard `stop --now`:
  valid immediate stops now publish `engine-stop:<project>:<run>` until lane
  termination, unclaim, and final run-state recording finish.
- Ordinary engine controls and invalid stop requests remain outside the ledger.
- Returned verification exposed an invalid private-poller `HEAD`: production
  passed a base ref while tests covered only object IDs. The gate now resolves
  that ref to an exact commit before writing its detached private `HEAD`.

## Verification scope

- New blocked-actuator shutdown-ownership regression.
- Verification queue ownership, all outcomes, write-error, and unwind cases.
- Existing graceful drain and stale-entry lifecycle cases.
- Immediate engine-stop ownership metadata, lifetime, and cleanup regression.
- Real-Git symbolic-base private-poller regression and complete merge-gate
  target.
- AGENTS/template synchronization, targeted Clippy, formatting, diff check.

## Submission contract

- Branch: `worktree-SH-556`.
- One PR references SH-556 and remains open for centralized verification.
- The verifier owns the full suite, merge, completion, and lane cleanup.
