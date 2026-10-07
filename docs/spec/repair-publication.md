# Repair publication (SH-884, v3.0.3)

## Contract

A commit on a dispatched story branch with an open closing PR must reach that
PR without waiting for verification. The owner selected publication on commit,
including unfinished work. Agents still commit only. The system owns pushes.

Managed Git hooks and the plugin fallback already call `story commit-sync`.
That service captures the worktree lease, full commit ID and linked PR. It
does not need a story reference in the commit message. It writes a durable,
store-scoped request before returning. Invalid leases fail with context.

## Processing and authority

The daemon checks its publication spool every 250 ms and on startup. Publication
does not acquire a verifier slot or the agent's workspace lock. A controller
lock serializes request acknowledgement. A separate inherited transport lock
serializes publication with normal verifier submission for the same story.

Each notification has an exclusive file name. Duplicate notifications cannot
erase retry evidence. The worker groups notifications for the same lease and
PR, proves every requested commit belongs to the current HEAD, and publishes
that HEAD. Atomic replacement and filesystem sync retain retry evidence.
Notifications that arrive during publication remain for the next pass.
If cleanup removes the worktree before acknowledgement, the worker retains the
request until fresh merged-PR evidence proves that it contains every commit.

The existing submission helper and receipt validator perform the transport.
Private publication parameters name the exact commit and existing PR. The helper
permits dirty worktrees and in-progress stories, but publishes committed content
only. It checks the PR before the push and before acknowledging success. It
never creates a replacement PR. Git pushes an immutable commit ID without force.
Ordinary submission keeps its verifying-state and clean-worktree requirements.

Failures retain the request and add a contextual story comment. Automatic retry
waits 30 seconds. Recovery adds a comment. Publication does not create a gate
attempt, certification receipt, state transition, or PR link refresh.

## Overrides and cleanup

An override attempts to flush pending publication before its state transition.
It also captures the latest HEAD from its recorded worktree. No network operation
runs inside a store transaction. Publication failure adds an explicit warning;
the override remains allowed. A GitHub merge outside Storyhook during an outage
cannot be made atomic with a local commit. Local work must remain recoverable.

Existing cleanup checks require local tips to be ancestors of the fetched base.
The new merged-PR check also requires the branch tip to be an ancestor of that
PR's exact merged head. Both native cleanup and leased shell reap preserve the
branch when this evidence is missing, contradictory, or excludes a repair.
The current SH-814 and SH-820 records report an additional filesystem-identity
refusal. That incidental refusal is not the repair-preservation contract.

## Validation and delivery

Tests cover notifications without story references, dirty worktrees, duplicate
notifications, failed receipt retention, stale leases, real local pushes,
divergence, closed PRs, managed hooks with stopped verification, and an override
whose remote merge includes the repair. An endpoint outage and daemon restart
exercise durable retry and coalescing of multiple commits. Missing worktrees
cannot acknowledge unpublished commits. Cleanup checks cover a repair added
after the original PR head and reject deletion despite a later base.

Only added or changed tests run in the implementer lane. The verifier owns
publication of this implementation, the full gate, merge and lane cleanup.
