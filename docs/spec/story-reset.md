# Reset a story (SH-664)

## Contract

`story reset <id> [--force]` and the dashboard Reset dialog use one native service.
Reset applies to open ordinary stories. It removes the exact owned worktree and
tmux window, releases the claim, and finishes in Todo. Branches, commits, story
content and relationships remain. Missing resources are idempotent success.
Closed stories and typed epics are refused.

Dirty and locked worktrees require explicit Force. Force never overrides identity
checks, the primary-checkout prohibition, caller protection or active verification.
The dialog starts with Force unchecked and states that uncommitted work will be lost.

## Ownership and recovery

The council decision is recorded on SH-664. A durable per-story reservation holds
the exact cleanup lease, Force authorization and diagnostics across partial failure
or daemon restart. Cleanup uses short store transactions and bounded subprocesses.
The reservation prevents claims, state changes, dispatch preparation and verification.
One live executor owns cleanup. Existing dispatch preparation or verification prevents
reset admission. Cleanup must not run while an earlier cleanup subprocess survives.
An explicit retry revalidates resources; it does not infer Force from an earlier failure.

An unresolved landing intent also prevents reset admission after a daemon restart.
The store rejects transactions that hold both reservations for the same story.
Landing attempts and recovery children inherit workspace ownership until they exit.
Schema migration 39 adds Reset after the published landing-intent migration 38.

The final transaction moves the story to Todo and clears its reservation. It occurs
only after Git registration, worktree path and owned tmux windows are absent.
Failures retain identity and diagnostics so retries cannot guess at deleted markers.

## Validation

Real CLI and API tests cover destination, repeated reset, dirty/forced cleanup,
locked worktrees, ownership mismatches, missing resources, active work and failures.
Browser tests exercise confirmation, Force, cancellation and error recovery.
Only new and directly impacted tests run in the implementation lane.

## Deferred findings adopted during validation

SH-664 remains open for two pre-existing inventory failures, recorded in its
comments. These were deferred under the user’s context rule:

- `route_authority`: add the missing `VerificationControl` route probe.
- `dashboard_enter_submit_guard`: classify the existing `verificationMenuNode`
  keydown listener and cover its composition/focus contract.

Each repair belongs in its own commit with its regression. Neither is hidden by
changing or disabling its existing failing check.

SH-660 has published both repairs in PR #779, in separate commits. They await
central verification. Reconcile those changes when they reach the default branch;
the findings remain open here until that integration is verified.
