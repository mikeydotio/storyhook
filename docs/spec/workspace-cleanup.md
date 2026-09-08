# Story workspace cleanup

`story cleanup [--dry-run]` reclaims disk space from inactive, merged
StoryHook story workspaces. The daemon invokes the same service once per day
by default.

## Ownership and eligibility

Cleanup authority comes only from a versioned `StoryCleanupLease` recorded in
story history or in the worktree's private Git marker. Legacy, malformed, and
conflicting leases are reported and preserved. StoryHook never guesses a path
or branch from a story ID.

A leased workspace is eligible only when all of these facts can be proved:

1. The lease names this project and its canonical registered repository.
2. No window with the exact story ID exists on the leased tmux socket.
3. The canonical worktree is clean, unlocked, registered, and checked out on
   the leased branch.
4. After fetching `origin/HEAD`, every existing worktree, local-branch, and
   origin-branch tip is an ancestor of that default branch.
5. The leased branch is not `main`, `master`, or the repository default.

An unavailable dependency is a refusal, not permission to delete. A missing
tmux socket is the sole exception because it proves that exact server is not
running.

## Removal and recovery

After the complete preflight, StoryHook removes the worktree with
`git worktree remove`, prunes worktree metadata, deletes the local branch, and
deletes the origin branch. It then verifies that the path and both refs are
absent. Reports include reclaimed worktree bytes and a reason for every skip.

The operation is idempotent. If a step fails after an earlier step succeeded,
the lease remains durable and the next pass rechecks the remaining resources.
Remote authentication and network failures fail closed and appear in output.

`--dry-run` executes the same discovery and preflight without mutation. It is
the recommended first run in an existing installation.

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
