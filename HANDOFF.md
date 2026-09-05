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

## Verification scope

- New blocked-actuator shutdown-ownership regression.
- Verification queue ownership, all outcomes, write-error, and unwind cases.
- Existing graceful drain and stale-entry lifecycle cases.
- AGENTS/template synchronization, targeted Clippy, formatting, diff check.

## Submission contract

- Branch: `worktree-SH-556`.
- One PR references SH-556 and remains open for centralized verification.
- The verifier owns the full suite, merge, completion, and lane cleanup.
