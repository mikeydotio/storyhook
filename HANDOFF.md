# Handoff — SH-173 done, this worktree has nothing left in it

**SH-173 is done.** Nine commits, each green on `make test`, PR open from
this worktree. The run itself is described by
[`HARDENING_PROGRESS.md`](HARDENING_PROGRESS.md) — read its **SH-173** entry
(the last one in the file) for what changed and why; that is the process,
this file is only what comes next.

## There is nothing to take over here

This worktree was created for SH-173 alone. Once the PR merges and the
branch is deleted (verify the merge landed first — never assume), this
directory's job is done. There is no in-progress work, no open question, no
partial commit waiting on a decision.

## What's next, if you're picking up fresh

`story next` currently leads with **SH-196** (`high`, `dashboard, dispatch`)
— the dashboard's dispatch endpoint failing silently-ish when the installed
plugin script predates the daemon's `--project` flag by nine commits. Filed
during the github-sync/setup story (see this file's entry above SH-173's in
`HARDENING_PROGRESS.md`), reproduced exactly, worked around there by cutting
a local plugin release; the underlying code-level defect (a version-skewed
plugin fails with a generic usage message instead of a clear diagnosis) is
still open.

Named successors from SH-173 itself, each filed as its own story rather than
folded into that one:

- The `ChangeBus` 200ms coalescing window, which two dispatch threads and the
  250ms change-token poller already raced before SH-173 touched either.
- `rest::route`'s missing `catch_unwind` — a REST-side panic now kills one of
  `DISPATCHERS` dispatchers rather than the whole daemon's only one, but
  still wedges that one permanently rather than being caught the way
  `rpc::invoke` already is.
- `story daemon status` reporting the in-flight set, since the client-facing
  stalled-command messages now imply more than the status command delivers.

Otherwise: `story load-context` or `story summary` for the live backlog.

## Gate

`make test`, supervised in the background with **log growth as the
heartbeat** and a stall bound — not a fixed wall-clock guess. Budget roughly
40–50s per run on this machine. Do **not** push with
`SKIP_PREPUSH_TESTS=1`. Never bump the version or deploy from a linked
worktree — land the PR and let `main` handle both.
