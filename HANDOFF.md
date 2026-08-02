# Handoff — the hardening run, next up: SH-132

*(Supersedes the SH-130 purge handoff. SH-130 is closed: the schema half merged
as #88, the purge as the PR that carries this file, and SH-20 is gone from the
real store.)*

The run itself is described by
[`HARDENING_PROGRESS.md`](HARDENING_PROGRESS.md) — read its **START HERE**
section first. That is the process; this file is only what the next story needs
on top of it.

## What SH-130 left behind, for anyone who touches the store

Two things landed that are easy to trip over and hard to guess:

1. **`events_reject_delete` now names `stories`.** A migration that rebuilds
   `stories` must drop that trigger at the top and recreate it at the bottom, or
   `ALTER TABLE … RENAME TO` fails with `no such table: main.stories`. The
   header of `src/store/schema/0005_purge_story.sql` explains it, and
   `tests/store_migrations.rs` measures both directions. Note the reproduction
   only exists through rusqlite's bundled SQLite 3.46 — the system `sqlite3` at
   3.51 accepts the same batch, so do not "verify" this at a shell prompt.
2. **`story purge <ID>` exists**, refuses a story that is not already
   soft-deleted, and is the second and last operation permitted to delete
   events. `Response::ConfirmationRequired` now carries a `ConfirmationPlan`
   enum rather than a `DeinitPlan`; a third destructive verb adds a variant and
   gets the whole gate for free.

## The next story: SH-132 — delete the 505 fixture projects

`story show SH-132` is the brief and it is complete. The parts that matter most:

- **Back up `~/.local/share/storyhook/store.db`** to a dated file outside the
  data directory, and verify the copy opens, *before the first deletion*. This
  is 505 irreversible deletions against the real tracker.
- **Drive the loop from the explicit keep-list on the story, never from a
  `tmp*` pattern.** Re-verify the keep-list against `story project list` first —
  a real project added since the story was filed has to be added to it.
- The mechanism is `story project deinit <SLUG> --force`, one call per project.
  There is no bulk form, so the loop is written by hand.
- It runs **before SH-119**, which deletes `project_paths` — the only reliable
  evidence of what is junk.

**The store is now at schema 5**, and the installed binary carries migrations 4
and 5. The migration framework took its own verified backup when they ran; that
is not a substitute for the one SH-132 asks for.

## After SH-132

The queue in `HARDENING_PROGRESS.md` is the forecast, re-derived from
`story next` each iteration. SH-131 is next after SH-132, then SH-115.
