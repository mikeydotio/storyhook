# Story workspace cleanup

Every committed transition into a CLOSED superstate schedules durable resource
cleanup (SH-847). This includes direct completion, abandonment, custom closed
states, verifier landing, batch completion, imports, and computed state changes.
The state change succeeds independently of later resource cleanup.

## Intent and ownership

The store records one `closure_cleanups` request with the final story projection
in the same transaction. A unique token identifies that closed lifecycle.
The transaction compares computed epic states as well as stored story states.
A child write can therefore close or reopen its parent without synthetic events.
Metadata edits and closed-to-closed transitions preserve the token; reopening removes
pending intent, and the next closure receives a new token. A transaction whose
final state is open does not schedule cleanup. Rollback rolls back both writes.
Migration 53 backfills existing closed stories once.

Requests are not destructive authority. Cleanup discovers leases in story
history, engine lanes, and registered worktree markers and requires agreement.
It validates the current closure token and CLOSED superstate while acquiring
the existing durable cleanup reservation. That reservation pins the lease,
filesystem incarnations, pane and process identity. The controller and inherited
workspace locks exclude dispatch, verification, reset, and competing cleanup.
An admitted operation fences state changes until its effects settle.

The existing `dropped_cleanups` storage and process journals retain their names
and serialized representation. They now serve all closed states. Interrupted
legacy dropped cleanup resumes its original reservation, never a replacement.
Completed requests and reservations are receipts, not authority over resources
that later reappear at the same path.

## Resource policy

Cleanup terminates only the exact leased agent process tree and tmux window,
proves its captured writers have exited, then removes the clean, unlocked,
registered worktree with non-forced `git worktree remove`. Ignored build artifacts
inside that worktree are reclaimed with it.

Unverified or abandoned work retains its local branch and every remote branch.
A local branch is removable only with completion evidence for the matching
verification lease from the current open lifecycle (including legacy cleanup
receipts), plus freshly checked
ancestry in the origin default branch. An operator override additionally needs
a recorded merged PR. Deletion uses the inspected OID as an expected value, so
a changed branch fails removal. Dropped stories always retain their branch.
Remote branches remain outside cleanup.

Dirty files, detached or locked worktrees, ambiguous leases or panes, changed
process identities, protected branches, the main checkout, the caller's
worktree, installed executable resources, and foreign repositories are preserved.
No resource path or deletion authority is inferred from a story name.

Before every destructive stage, the controller records its progress. Restart
reconciles partial removal against pinned identities. A settled refusal releases
ownership; uncertain termination or removal retains it. Failure never reverses
closure. Reports distinguish safety refusals (`skipped`) from operational
failures (`failed`); requests retain the latest diagnosis and retry time.
A changed failure produces one story comment, not a comment on every retry.

## Pane termination helper

The binary stops a captured pane with `plugins/story/lib/dropped-cleanup-pane.py`.
It does not use the installed plugin, which can be a different version. It
writes the whole `plugins/story/lib` directory that it embeds, never a list of
files, and runs the helper from that copy. The helper then resolves its imports
as it does in the installed plugin and in the plugin tests. A hand-kept list of
five files missed the `tmux_client`, `tmux_target` and `tmux_server_env` imports
that SH-825 added, and every pane cleanup failed until SH-881.

The copy lives at `dropped-cleanup/<token>.bundle` in the daemon state
directory, beside the `<token>.json` process journal, with mode `0700`. Each
attempt replaces a leftover copy, runs the helper and then removes the copy.
The reservation token does not change between retries, so a helper traceback
that names these files is the same at each retry and does not post a new
comment. A random temporary directory made each retry look like a new failure.

## Scheduling and manual retry

The daemon wakes on project-change notifications and at startup, with a bounded
30-second recovery wake for lost notifications and pending retries. Each project
and story can make progress despite another story's refusal. Lifecycle cleanup
runs even when the verifier is stopped or a closed story carries a reserved label.

The periodic reconciliation sweep remains configurable:

| Setting | Default | Meaning |
|---|---:|---|
| `cleanup.auto` | `true` | Enable the periodic discovery/reconciliation sweep. |
| `cleanup.interval` | `1d` | Minimum time between periodic sweeps. |

These settings do not disable cleanup triggered by closure. The verifier no
longer reaps story workspaces itself; it commits completion and releases its
locks. Batch scratch-resource retirement remains part of verification.

`story cleanup` runs the same controller and can retry before the automatic
backoff expires. `story cleanup --dry-run` performs discovery and preflight without
reserving, terminating, removing, or updating request state. An interrupted
process journal requires a real retry to establish safety.

JSON keeps `removed`, `skipped`, `failed`, reclaimed-byte counts, and the existing
`removed_tmux_window` and `retained_local_branch` fields. Already-clean receipts
are silent. Missing or conflicting authority is diagnosed; it never permits
fallback to guessed ownership. There are no new CLI flags.

Projects without a linked checkout and without discovered resource authority
complete as no-ops. A missing or unreadable configured checkout with resources
remains a failure; it does not establish absence.
