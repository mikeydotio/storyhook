# Story reset — SH-664, SH-717, SH-886

A reset is the final lever for a wedged story, the equivalent of
`git reset --hard` on the story. **Once a reset is reserved it never fails.**
It applies only to open ordinary stories; epics and closed stories are
ineligible, and a request for one is refused before anything is reserved.

## One contract for every entry point

| Entry point | Confirmation |
|---|---|
| Dashboard card Reset | The exact canonical story ID, typed |
| `story reset <id> [--force]` | The positional story ID |
| Plugin `/story reset <id> [--force]` | Delegates to `story reset` |

Every entry point does the same thing (council C1 on SH-886):

- closes the story's tmux window;
- discards its worktree, including dirty, untracked and locked work;
- deletes its local branch;
- clears its awaiting reason and returns it to `todo`;
- preserves remote branches, pull requests, story content, relationships
  and discussion.

`--force` is accepted and changes nothing. Agents run reset only on an
explicit user request; `story unclaim` releases a story and keeps its work.

Before anything is removed, the reset records a **recovery record** on its
receipt and in its completion comment: the deleted branch's tip SHA with the
`git branch <name> <sha>` command that restores it, the number of commits on
no other branch, tag or remote, the counts of discarded changed and untracked
paths, and the awaiting reason it cleared. Branch deletion and worktree
removal also delete their reflogs, so without the record unpushed commits
would be recoverable but undetectable.

## A convergent operation

A reservation is durable intent. Its only mandatory effect is the **finish
transaction**: the story returns to `todo`, its engine lanes go idle, its
verifier incident and pending block deliveries are retired, and the receipt
completes. Everything before it is best effort.

**Teardown never refuses (D1).** Each identity check withholds authority over
only the resource it protects; the resource is left in place as **residue**
with its reason, and the reset still finishes. Reset never destroys what it
cannot prove the story owns: the primary checkout, a protected branch (`main`,
`master`, the primary checkout branch, or the cached `origin/HEAD` target), a
worktree holding installed StoryHook artifacts, the caller's own worktree or
tmux window, a window whose pane identity changed, and any path whose pinned
device and inode changed. Transient failures get three attempts before they
become residue. An unregistered worktree directory whose pinned identity still
matches is removed; a registration whose directory is gone is removed through
`git worktree remove`.

**No new wedge (D7).** When residue overlaps what the next dispatch uses — the
window, the worktree path, the local branch, or an `origin/<branch>` that the
default branch does not contain (a fresh branch's push would be rejected) —
finish sets an awaiting reason naming the residue and the remedy, so Full Auto
does not claim the story into a quarantine.

**Contention is waited out (D2).** Every reset write retries
`StoreError::Busy` with capped backoff (`store::patience`). The dashboard's
reservation waits up to 60 s, below its 75 s mutation deadline, and the
dashboard retries a `409`.

**Locks and running work never refuse (D5).** Dispatch and verifier quiescence
keep their deadline; on expiry the reset proceeds and records it. The story's
workspace lock is waited for (`WORKSPACE_PATIENCE`, 60 s) and then ignored, and
the reset records that it ran without exclusion. A second request, or a second
executor, joins the unfinished reset.

**Competing owners are superseded (D4).** In its reserve transaction a reset
releases a Stop Now engine reset, a pre-upgrade native reservation and a
pending landing intent through their explicit release operations, and names
them in a comment. A batch member whose batch is landing is recorded as
withdrawn, so the batch stays valid and lands its other members. Stop Now
treats a superseded lane as deferred and never recreates its reservation (D6).

**Finish degrades instead of refusing (D3).** The return to `todo` ignores
blocker ordering. A catalog without an open `todo` state keeps the story's
state and records why. A finish that fails three times completes the receipt,
idles the lanes and leaves the story editable, with the error in its comment.

## The reset runtime

The daemon's reset runtime (`daemon::reset`) drives every reserved reset to
completion **on the daemon's own store**, so its writes queue on the store's
in-process mutex instead of competing through SQLite's busy timeout. It
resumes every unfinished receipt at startup and on a 30 s sweep, adopts
reservations that `story reset` recorded before this contract (with no more
authority than they recorded: the branch is kept, and dirty or locked work is
discarded only if that request was forced), drives at most four resets at once
and queues the rest. A worker panic releases its slot for the next sweep. The
daemon's stand-down stops the runtime's patient waits; the next daemon resumes
the work.

The dashboard's `POST /api/repos/{project}/story/{id}/reset` reserves and
queues; it returns `202` with `reset.handle`. `GET .../reset/{handle}` reports
`running` (with the last obstacle in `detail`) or `ok` (with `residue` and
`recovery`). It never reports `error` for an unfinished reset, and no request
is refused for capacity.

`story reset` hands its reservation to the runtime and waits up to 90 s, below
the served deadline (decision D11). It then prints the story with the reset it
ran under `reset`: `completed`, `removed` (window, worktree, branch),
`residue` and `recovery`, from the same derivation as the completion comment.
`story show` reports an unfinished reset there until the daemon finishes it.
The plugin's `/story reset` maps it to `removed.worktree`, `removed.branch`,
`closed_window`, `residue` and `recovery`. Without a daemon
(the TUI), the reset runs inline. The receipt records the requester's tmux
pane, working directory and hook policy, so a resumed reset keeps those
protections and fires state-change hooks from the requester's checkout.

## Verification

Regression suites: `tests/story_reset/` (busy, residue, card, compatibility,
delivery, orphan, quiescent and the native CLI), the runtime unit tests in
`src/daemon/reset/tests.rs`, the controller unit tests in `src/api/reset.rs`,
`tests/api_reset.rs`, the Stop Now supersession cases in
`tests/engine_reset.rs`, the landing and batch supersession cases in
`tests/landing_intents.rs` and `tests/batch_landing.rs`, the plugin's
`test-reset.sh`, and the dashboard's reset browser specs.

## As built (SH-886)

The SH-801 incident: a card reset removed its window, worktree and branch,
then lost its final write to SQLite's busy timeout and stayed reserved, so it
blocked every later lifecycle change. Origin: the reset controller opened its
own store, no reset write retried contention, and every refusal or transient
error aborted the reset. See `docs/rca/sh-886-reset-never-fails.md`. The
decisions D1–D11 and council C1 are recorded on SH-886.
