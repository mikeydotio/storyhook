# Handoff — the hardening run, next up: SH-131

*(Supersedes the SH-132 handoff. SH-132 is closed: 505 fixture projects are gone,
13 real ones remain, and the store is back to a usable size.)*

The run itself is described by
[`HARDENING_PROGRESS.md`](HARDENING_PROGRESS.md) — read its **START HERE**
section first. That is the process; this file is only what the next story needs
on top of it.

## What SH-132 left behind

Nothing in the code changed — SH-132 was data cleanup against the real store.
Two facts about the store are now different, and one is a trap:

1. **The store holds 13 projects, not 518.** `story project list`, the dashboard
   and `/api/repos` all return the same 13. A fixture site in a neighbouring
   repository that used to add junk here cannot any more (store isolation, v2.0.0),
   but SH-95 — retiring the temp-path heuristic — is still open and is the
   origin-fix for how the 505 arrived.
2. **A hand-taken backup expires.** `src/daemon/backup.rs:87` prunes the backups
   directory to the newest seven `storyhook-*.db` files, and cannot tell a daily
   snapshot from a safety net someone took before a destructive operation.
   SH-132's backup dodges this by *not* matching the pattern
   (`store-pre-sh132-cleanup-20260802T165904Z.db`). **SH-130's does not** and is
   roughly five daemon snapshots from silent deletion. Filed as **SH-135**.
   If you take a backup by hand, do not name it `storyhook-*`.

**Verify a backup with `sqlite3 -readonly`, never by opening it through the
CLI.** `STORYHOOK_STORE_PATH=<backup> story project list` works, and also
converts the file to WAL mode permanently — after which nothing can open it
read-only without write access to create the `-shm`. Header bytes 18–19 tell you
which mode a file is in: `1 1` is rollback journal, `2 2` is WAL. Every
storyhook-produced snapshot is `1 1`; the daemon's own verification does not have
this problem.

## The next story: SH-131 — where the store-isolation invariants live

`story show SH-131` is the brief and it is complete. It is a decision story, not
a code story: three invariants currently homeless in `CLAUDE.md` each need one
permanent home chosen from four options (standing rule, spec "As built", doc
comment, or a test that fails loudly).

- **The most valuable output is a gap, not a document.** Invariant 1 —
  `--store-path` becomes `$STORYHOOK_STORE_PATH` in `main` before anything
  resolves — appears to be pinned by no test at all. A refactor that threads the
  flag "properly" would pass the whole suite while silently breaking the git
  hooks, the TUI, `story daemon status` and the spawned daemon. Confirm or refute
  that first; if confirmed, the test is the deliverable.
- **Timing:** SH-131's own text says before **SH-114 and SH-116**, both of which
  rewrite flag resolution in `main`.

**A note on ordering.** `story next` leads with **SH-115** (critical) rather than
SH-131 (high), and SH-115 — C3 Identity, the remotes schema and URL normalizer —
does not touch flag resolution, so taking it first would not endanger these
invariants. The queue in `HARDENING_PROGRESS.md` nonetheless puts SH-131 first,
and START HERE says to pick the first unchecked story in the queue. Follow the
queue unless you have a reason to correct it in place, which the queue invites.

## After SH-131

The queue in `HARDENING_PROGRESS.md` is the forecast: SH-115, then SH-94 and
SH-110 (both gating SH-114), then the epic proper. Two new stories filed during
this run are not yet in it — **SH-134** (`add_type` accepts an unaddressable
slug) and **SH-135** (backup retention, above).
