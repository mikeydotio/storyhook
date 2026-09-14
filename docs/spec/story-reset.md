# Native and card reset — SH-664, SH-717, SH-718

StoryHook retains two deliberate reset contracts. Both apply only to open
ordinary stories, return to `todo` after proven cleanup, and preserve story
metadata, relationships, discussion, assignee, remote branches, and pull requests.
Epics and closed stories are ineligible.

| Entry point | Local work | Awaiting reason | Confirmation |
|---|---|---|---|
| `story reset <id> [--force]` | Preserves local branches and commits. Dirty or locked worktrees require explicit `--force`. | Restores the previous reason. | Each invocation supplies its own force authorization. |
| Dashboard card Reset | Removes the local worktree and branch, including dirty, locked, and unpushed work. | Clears awaiting. | Requires the exact canonical story ID. |

The native command continues through the ordinary CLI/RPC invocation. Its
recoverable journal retains the exact cleanup lease and failure diagnostics
across daemon restart. `story show` exposes incomplete native reset details.
A retry never infers Force from an earlier invocation. Missing owned resources
are idempotent success; ownership mismatch, primary checkout, caller worktree,
caller tmux window, and active workspace operations remain protected.

The shared card menu opens a typed-ID confirmation dialog. Cancel has initial
focus. No mutation occurs before confirmation. The dashboard captures the
project and story for the operation, prevents repeat submission, polls its
handle, and displays failures without reporting optimistic success. The menu
scrolls within the viewport when its actions exceed the available height.

## Card operation lifecycle

`POST /api/repos/{project}/story/{id}/reset` accepts JSON with `confirmation`
equal to the canonical story ID. Normal API authentication and mutation
guards apply. It returns 202 with `reset.handle`, `reset.story`, `reset.state`,
and `reset.detail`. `GET .../reset/{handle}` is authenticated and scoped to the
same project and story. States are `running`, `ok`, and `error`. The endpoint
requires typed confirmation; the native command's `force` payload is not a
card reset request.

The daemon persists the reset before external cleanup and handles it outside
the fixed store dispatcher pool. A per-operation file lock serializes retries
across processes. Existing dispatch must settle before discovery; only the
selected verifier is cancelled and joined. After quiescence, the reset takes
the repository/story WorkspaceLock shared with dispatch, verification, and
native reset. Pinned identities are checked under that lock. Destructive
Git/tmux subprocesses inherit it, so an orphaned cleanup child retains ownership
until it exits. The lock remains held through the final store transaction and
is released before state-change hooks run.

ResourceService pins exact resource identities. Device/inode observations pin
the common Git directory, worktree directory, and private Git directory, so a
retry refuses a replacement even when its path and branch are unchanged. Cleanup
shares native Git/tmux observations, lease validation, bounded process capture,
and the installed artifact guard with existing lifecycle operations. Destructive
confirmation waives only local work preservation: protected branches, ambiguous
resources, replaced windows, invalid leases, installed artifacts, and the caller's
own worktree remain protected. A project with no checkout and no durable resource
evidence has no local cleanup to perform. Local-only repositories need no remote;
when an origin exists its authoritative default branch remains protected.

Only proven resource absence permits one transaction to return the story to
`todo`, release its selected engine lane, clear a verifier incident belonging
to that story, and record completion. Unrelated lanes and project verifier
permission remain unchanged. Failed operations retain ownership and diagnosis;
an explicit retry reuses their token and pinned identity. After daemon restart,
polling an unfinished operation reports interruption and offers retry.

## Shared ownership and upgrades

Native reset, active card reset, Stop Now engine reset, and durable landing
intent are mutually exclusive owners of one story. The store validates this
invariant at the transaction boundary, including imports, repairs, and deletion.
An unresolved landing intent prevents reset even after daemon restart; an
uncertain merge outcome must be reconciled before resource authority changes.

Reservations prevent conflicting lifecycle changes, claims, dispatch and
verification admission, engine lane reassignment, and changes or deletion of
the owning project identity. Ready lists, dashboard ordering, and continuation
eligibility exclude reserved stories.

The 3.0.0 schema bridge preserves published native reset journals in
`story_reset_reservations`; card receipts use `story_resets`, and Stop Now keeps
`engine_resets`. The bridge preserves unresolved authority and validates the
historical schema lineage before mutation. Completed card receipts remain
pollable until the story is reset again or deleted.

## Verification

Native CLI regressions cover safe and forced cleanup, retained branches/commits,
caller protection, unrelated panes, failed cleanup, durable recovery, and hooks.
Card service and API regressions cover typed confirmation, scoped receipts,
local-only and no-checkout resets, pinned identity, cleanup failure/retry,
selected-lane isolation, and reservation exclusion. Shared-lock regressions cover
quiescence ordering and retry after the former owner exits; inherited-child
exclusion remains covered by the workspace-lock suite. Browser tests exercise
the shared menu, confirmation, cancellation, failure, concurrent story closure,
and the real endpoint. Full-suite certification belongs to the release verifier.
