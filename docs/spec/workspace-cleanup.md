# Story workspace cleanup

`story cleanup [--dry-run]` cleans dropped-story workspaces and retries the
centralized verifier's reap of completed work. The daemon invokes the same
service once per day by default. Dropping a story does not synchronously
remove its resources; the next scheduled pass or manual cleanup handles them.

For completed work, it is not the primary reaper, and since SH-653 it is not an independent one.
The centralized verifier reaps a story's window, worktree and local branch on
the green path (`verification-workflow.md`, "Green: merge, done, reap") and
retries a failed reap itself; this command is the same retry by hand or on a
schedule, gated on the verifier's own verdict, and with no authority the reap
does not have. Dropped work follows the separate policy below (SH-724).

## Dropped work

A story currently in `dropped` is eligible without verification or merge
evidence, including archived stories and work dropped before submission.
Discovery uses durable lease events, engine lanes, and registered worktree
markers. Missing, conflicting, foreign, or malformed ownership is refused.

Cleanup removes only the exact leased window and clean, unlocked, registered
worktree. Local and remote branches remain for recovery, including unmerged
commits. This path does not fetch or require an origin. Detached worktrees,
dirty files, changed registrations, protected branches, the calling worktree,
and installed executable resources are preserved. Ignored build artifacts
inside an otherwise eligible worktree follow ordinary Git worktree removal.

The window must resolve to one pane on its recorded socket and have the leased
worktree as its working directory. Duplicate windows, extra panes, a changed
process incarnation, or an already-dead pane without captured process evidence
are refusals. Cleanup captures kernel process identity, freezes the complete
owned process tree, closes the exact window, and proves its captured writers
have exited before running non-forced `git worktree remove`.

A durable reservation pins the drop-transition sequence, lease, filesystem
identities, pane, and process incarnation. Its controller and inherited
workspace locks exclude concurrent cleanup, dispatch, verification, and reset.
State changes and ownership transfer are refused while effects remain
unsettled; comments remain available. Process identities are journaled before
signals. Each destructive stage is recorded before execution, so restart can
reconcile partial removal without targeting replacement resources.

Retry with `story cleanup` after a failure. Inspect its named refusal and
diagnostics before repairing retained resources. A settled refusal releases
ownership; uncertain termination or removal retains its reservation until a
retry establishes safety. Cleanup never resets the story or marks it done.
A successful receipt is silent on later passes, even though its local branch
remains. Recreated resources cannot inherit a completed receipt's authority.

JSON removal entries add `removed_tmux_window` and `retained_local_branch`.
Existing worktree and branch-removal fields keep their meanings. Partial
operational failures appear in `failed`; safety refusals appear in `skipped`.
`--dry-run` previews eligible resources without reserving or terminating them.
An interrupted process journal requires a real cleanup retry to establish safety.

## Completed-work ownership and eligibility

Cleanup authority comes only from a versioned `StoryCleanupLease` recorded in
story history or in the worktree's private Git marker. Legacy, malformed, and
conflicting leases are reported and preserved. StoryHook never guesses a path
or branch from a story ID.

A leased workspace is eligible only when all of these facts can be proved, in
this order — the first three from the store, before any Git or network work:

1. The lease names a story of this project (`unknown-story` otherwise).
2. The story is CLOSED (`story-open` otherwise, naming its state).
3. The story's **latest verification generation** carries the verifier's
   `CENTRAL VERIFICATION CLEANUP COMPLETE` or `CLEANUP REQUIRED` comment
   (`not-verifier-released` otherwise). The generation is read by event order,
   the same way the verifier's own retry reads it; a marker from an earlier
   verification of a since-reopened story does not count.
4. The lease names this project and its canonical registered repository.
5. No window with the exact story ID exists on the leased tmux socket.
6. The canonical worktree is clean, unlocked, registered, and checked out on
   the leased branch.
7. After asking origin for its default branch (`git ls-remote --symref
   origin HEAD` — never the local `origin/HEAD` cache; SH-691) and fetching
   it, the worktree tip and local-branch tip are ancestors of that branch.
8. The leased branch is not `main`, `master`, or the repository default.

An unavailable dependency is a refusal, not permission to delete. A missing
tmux socket is the sole exception because it proves that exact server is not
running. Cleanup never closes a window: an open one is a refusal, and closing
it is the verifier's reap's job.

## Removal and recovery

After the complete preflight, StoryHook removes the worktree with
`git worktree remove`, prunes worktree metadata, and deletes the local branch.
It then verifies that the path and the local ref are absent. Reports include
reclaimed worktree bytes and a reason for every skip.

The remote branch is out of scope: the verifier's merge step (`land-pr.sh`)
deletes it, and cleanup neither reads nor writes it, so nothing on the remote
can be lost by this command and a remote-only commit never blocks a local reap.

A lease whose worktree and local branch are already absent is not a removal.
After a CLEANUP COMPLETE it is what the verifier already verified and earns no
report line; after a CLEANUP REQUIRED it is reported as `already-clean`.

The operation is idempotent. If a step fails after an earlier step succeeded,
the lease remains durable and the next pass rechecks the remaining resources.
Fetch authentication and network failures fail closed and appear in output.

`--dry-run` executes the same discovery and preflight without mutation, and
lists every candidate it declined with the reason. It is the recommended first
run in an existing installation.

## Scheduling

Two per-project settings control the daemon:

| Setting | Default | Meaning |
|---|---:|---|
| `cleanup.auto` | `true` | Run automatic cleanup. |
| `cleanup.interval` | `1d` | Minimum time between attempts. |

Each attempt writes a durable per-project timestamp under the store-specific
daemon state directory. Restarts therefore do not repeat a recent pass. A
failure waits for the next configured interval to avoid a destructive
network retry storm; `story cleanup` can retry immediately after repair.

The repository's primary checkout and any shared `target/` beneath it are out
of scope. Build artifacts are reclaimed because they reside inside an
eligible story worktree.
