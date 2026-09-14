# Story card reset — SH-717

Reset is a destructive restart of one open ordinary story. It closes the
story's worker window, removes its local worktree and branch including dirty,
locked, and unpushed work, releases operational ownership, clears awaiting,
and returns the story to `todo`. It preserves story metadata, relationships,
discussion, assignee, remote branches, and pull requests. Epics and closed
stories are ineligible.

The shared card menu opens a typed-ID confirmation dialog. Cancel has initial
focus. No mutation occurs before confirmation. The dashboard captures the
project and story for the operation, prevents repeat submission, polls its
handle, and displays failures without reporting optimistic success. The menu
scrolls within the viewport when its actions exceed the available height.

## Lifecycle

`POST /api/repos/{project}/story/{id}/reset` accepts JSON with `confirmation`
equal to the canonical story ID. Normal API authentication and mutation
guards apply. It returns 202 with `reset.handle`, `reset.story`, `reset.state`,
and `reset.detail`. `GET .../reset/{handle}` is authenticated and scoped to the
same project and story. States are `running`, `ok`, and `error`.

The daemon persists the reset before external cleanup and handles it outside
the fixed store dispatcher pool. A per-operation file lock serializes retries
across processes. The reservation prevents claiming, conflicting transitions,
verification admission, engine lane reassignment, and identity changes or deletion of the owning
project. Ready lists, dashboard ordering, and continuation eligibility all
exclude reserved stories. Existing dispatch must
settle before discovery; only the selected verifier is cancelled and joined.

ResourceService pins exact resource identities. Device/inode observations also
pin the common Git directory, worktree directory, and private Git directory,
so a retry refuses a replacement even when its path and branch are unchanged. Cleanup shares native Git/tmux
observations, lease validation, bounded process capture, and the installed
artifact guard with existing lifecycle operations. Force waives only local
work preservation: protected branches, ambiguous resources, replaced windows,
invalid leases, installed artifacts, and the caller's own worktree remain
protected. A project with no checkout and no durable resource evidence has
no local cleanup to perform. Local-only repositories need no remote; when an
origin exists its authoritative default branch remains protected.

Only proven resource absence permits one transaction to return the story to
`todo`, release its selected engine lane, clear a verifier incident belonging
to that story, and record completion. Unrelated lanes and project verifier
permission remain unchanged. Failed operations retain ownership and diagnosis;
an explicit retry reuses their token and pinned identity. After daemon restart,
polling an unfinished operation reports interruption and offers retry.

The additive `story_resets` table leaves Stop Now's `engine_resets` records and
state-restoration behavior intact. Completed card-reset receipts remain
pollable until the story is reset again or deleted.

## Verification

Service and API regressions cover persistence, confirmation, cleanup failure,
retry, state-only reset, forced real-Git cleanup, and selected-lane isolation.
Verifier tests cover targeted cancellation and readmission refusal. Browser
tests exercise the shared menu, confirmation, cancellation, error presentation,
and the real endpoint. Full-suite certification belongs to the central verifier.
