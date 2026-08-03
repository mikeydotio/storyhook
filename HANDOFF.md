# Handoff — SH-152: the silent GitHub-sync data loss

**SH-117 is done and merged (#101, #103, #105).** The run itself is described by
[`HARDENING_PROGRESS.md`](HARDENING_PROGRESS.md) — read its **START HERE**
section first. That is the process; this file is only what comes next.

## Not SH-119 — it is blocked

The Phase 2 forecast used to say SH-119 was next. It is **blocked by SH-151**
(two storyhook projects in one git repository share an origin), which is still
open. `story next` leads with **SH-152**, and everything SH-119 will need to
know when its turn comes is now a comment on SH-119 itself rather than here.

## SH-152, and why it is worth taking first

Filed by SH-117's council as the highest-severity finding in that story, and
not fixed there because it is out of C5's charter:

> `src/github/conflict.rs:38-43` is `.interact().unwrap_or(2) // default to
> Skip on error`. With no terminal — which under the daemon is **always** —
> every GitHub sync conflict silently resolves as Skip. The user sees a
> successful sync and loses every conflicting remote-side edit, with no message
> anywhere.

Three sibling sites in `src/github/initial.rs` (`:74`, `:99`, `:220`) call
`Select::interact()` with no terminal check at all. All four are named in
`tests/invoker_seam.rs`'s allowlist, so the exemptions are recorded rather than
silent — and that test is what goes red if a fifth prompting site appears.

**The fix is a design question, so it is a council question:** refuse loudly, or
carry the conflicts back to a client that can ask. Invoke `council:council-vote`
before implementing, and record the verdict as a `story comment` on SH-152.

**The seam allowlist shrinks with the fix.** `tests/invoker_seam.rs` asserts the
allowlist has exactly four entries; removing a violation means editing that
count, deliberately, which is the design.

## Three things that bit during SH-117

- **A background `make test` reported "exit code 0" while `make` exited 2.** The
  command was `(make test > log; echo "MAKE_TEST_EXIT=$?" >> log)`, so the
  subshell's status — and therefore the harness's completion notification — was
  the *echo's*. The `MAKE_TEST_EXIT=` line inside the log is what caught it.
  Read the log, never the notification. Third appearance of this trap in this
  run.
- **`cargo fmt` after committing leaves earlier commits failing the gate.** Run
  `cargo fmt --all` *before* each commit.
- **`AppError::Validation` is HTTP 422, not 400.** `Usage` is 400. A REST test
  asserting the wrong one fails at the gate rather than at design time.

## Gate

`make test`, supervised in the background with **log growth as the heartbeat**
and a 120-second stall bound. Twelfth consecutive story with no wedge; the
streak is worth keeping. Budget ~10 minutes per run and expect to need two.
Do **not** push with `SKIP_PREPUSH_TESTS=1` — I did, and the log records why
that was the wrong call even though the gate was green.
