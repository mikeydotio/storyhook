# SH-706: Stop Now retained unfinished resources

## Failure and cause

In v2.4.2, Full Auto immediate stop delegated to leased **unclaim**, whose
contract deliberately preserves worktrees and branches. It also discarded
quarantined lane bindings without restoring their active stories. A verifying
story could reach cleanup before the service recognized the verification handoff.
Normal reconciliation could observe the intentional window closure and quarantine
the story while cancellation was still running.

This was a lifecycle mismatch, not a dashboard rendering error. Service tests
against unchanged production code reproduced both the verifying helper call and
the quarantined story remaining in-progress; three existing stop controls passed.
The original tests asserted resource preservation, so they protected the old
contract rather than the intended reset behavior.

## Correction

Explicit immediate stop first drains the run, then reserves each occupied active
story in a short SQLite transaction. The reservation owns an immutable token,
run/lane, exact cleanup lease and restoration destination. Shared story mutation
guards serialize verification handoff, claim and deletion with that reservation.
Verification committed first retains its complete story and resource ownership.

The leased helper authorizes its reservation before mutation, checks resource
identity, closes the exact agent window, then removes its worktree and local
branch with Git's supported force-removal operations. It refuses installed,
protected, current-caller and mismatched resources. Success must prove all four
resource absences and echo the exact token and lease. State restoration, awaiting
clearance, lane release and reservation removal commit together afterward.

Failures retain ownership and diagnostics without adding a story block. Other
lanes continue. Same-run workers serialize on a process-lifetime filesystem lock;
stale reconciliation cannot quarantine cancellation. Restart preserves the
reservation, and steady reconciliation retries the explicit stop. Ordinary crash
recovery and ordinary unclaim remain non-destructive.

## Regression evidence

- `tests/engine_run_model.rs`: verification exclusion, quarantined restoration,
  dispatch publication, missing leases, independent-lane failure and retry.
- `tests/engine_reset.rs`: receipt rejection, immutable reservation identity,
  caller protection, stale observations, duplicate stop, diagnostic comments,
  verifier-return fallback and restart recovery.
- `plugins/story/tests/test-engine-reset.sh`: production daemon/helper with real
  isolated Git and tmux; dirty, untracked, unpushed and locked resource removal;
  verifying/unrelated preservation; changed-marker refusal and same-token retry.
- `e2e/specs/engine.spec.ts`: destructive consequence confirmation and the real
  Full Auto lifecycle path.

The helper integration also exposed a missing registration of internal
`reset-target` flags in the CLI's earlier validation gate. A parser regression
failed before that registration was added. A separate reservation regression
proved that a same-token update could change its run identity; updates now
permit diagnostic changes only.
