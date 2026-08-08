# Handoff — SH-177 done, this worktree has nothing left in it

**SH-177 is done.** Three commits, a merge reconciling against `main`'s
concurrent SH-146/SH-147 tailnet-reprobe work, each state green on
`make test`, PR #165 merged, branch deleted. The run itself is described by
[`HARDENING_PROGRESS.md`](HARDENING_PROGRESS.md) — read its **SH-177** entry
(the last one in the file) for what changed and why; that is the process,
this file is only what comes next.

## There is nothing to take over here

This worktree was created for SH-177. The PR merged and the branch is
deleted (verified via `gh pr view`'s `mergedAt` before deleting — never
assumed). There is no in-progress work, no open question, no partial commit
waiting on a decision.

## What's next, if you're picking up fresh

`story next` currently leads with **SH-196** (`high`, `dashboard, dispatch`)
— the dashboard's dispatch endpoint failing silently-ish when the installed
plugin script predates the daemon's `--project` flag. Unchanged since the
last story that touched this worktree; still open.

No successors were filed out of SH-177 itself — the story's two named
redesign triggers (replace `tiny_http`, or add a connection cap) both landed
together, since investigation found the cap alone could never have closed
the gap on its own (see the HARDENING_PROGRESS.md entry).

Otherwise: `story load-context` or `story summary` for the live backlog.

## A note for the next session in *any* worktree

`git stash` is shared across every worktree of this repository — pushing and
popping within one worktree can silently apply or drop another worktree's
staged changes if a push happens on the same stack in the interim. Hit this
during SH-177's own landing (recovered cleanly; documented in its
HARDENING_PROGRESS.md entry). `CLAUDE.md` now says so directly: never `git
stash` inside a worktree.

## Gate

`make test`, supervised in the background with **log growth as the
heartbeat** and a stall bound — not a fixed wall-clock guess. Budget roughly
5–10 minutes per run on this machine when other sessions are also running
gates concurrently (port-reservation tests can flake under that contention;
confirm with an immediate clean re-run before treating a failure as real).
Do **not** push with `SKIP_PREPUSH_TESTS=1`. Never bump the version or
deploy from a linked worktree — land the PR and let `main` handle both.
