# Handoff — SH-130, second half: the supported purge

*(Supersedes the SH-113 store-isolation handoff, which merged as #79 and shipped
as v2.0.0.)*

SH-130's first three scope items are done; its fourth is not. The story stays
open and this file is what the next context needs.

## What landed

Three commits on `fix/sh-130-illegal-state-pair`:

1. `feat(store): let a migration declare it needs foreign keys off` — a
   `foreign_keys_off` flag on `Migration`, plus `PRAGMA foreign_key_check`
   inside the transaction for the migrations that set it. Needed because a
   twelve-step rebuild with foreign keys **on** fires every child's
   `ON DELETE CASCADE` from the `DROP TABLE` and empties `story_labels`
   silently, then commits successfully. Measured on SQLite 3.51.0, and pinned by
   a test so the justification cannot be quietly deleted.
2. `fix(domain): a deleted story comes to rest in a closed state` — the fold
   completes its derivation instead of forcing half of it.
3. `feat(store): the illegal state pair becomes unrepresentable` — migration 4:
   the composite foreign key, the `UNIQUE` parent index, the
   `(superstate = 'CLOSED') = archived` CHECK, and the repair of existing rows.

Acceptance criteria met: the tuples are enumerated (in migration 4's header, so
the reader who meets the constraint meets the enumeration), a direct write of
the illegal pair is refused **by the schema**, `story list --state todo` no
longer returns a closed story, `story reopen` was checked and does **not** have
the mirror defect, and the migration repairs existing rows.

**Outstanding, and the whole of what remains: a supported purge exists, and
SH-20 is gone from the store.**

## What to build

The council's D4, already decided — **do not re-run the vote.** It is a comment
on SH-130; the audit trail is `.council/sh130-illegal-state-pair/DECISION.md`.

- **Verb:** `story purge <ID>`, a distinct verb rather than a flag on `delete`.
  A flag that turns a reversible act irreversible is the wrong shape, and
  `project deinit` set the precedent.
- **Precondition:** it **refuses a story that is not already soft-deleted**.
  That is also the answer to what soft delete is now for — the reversible
  tombstone, and the required antechamber to the irreversible act.
- **Guard:** the ID must be typed to confirm, shaped like `main.rs`'s `confirm`
  for `project deinit` — refuse under `--json`, refuse with no TTY, both naming
  `--force`.
- **Migration 5:** narrow `events_reject_delete` by **AND-ing** onto migration
  3's clause rather than replacing it:
  ```sql
  WHEN EXISTS (SELECT 1 FROM projects WHERE id = OLD.project_id)
   AND EXISTS (SELECT 1 FROM stories
               WHERE project_id = OLD.project_id AND story_no = OLD.story_no)
  ```
  Project teardown fails the first conjunct; a purge fails the second;
  everything else still aborts. Delete the `stories` row **before** the events —
  the order `PROJECT_SCOPED_TABLES` already uses.
- **Two failure modes the council named that no other proposal raised:**
  1. Append `StoryRelationshipRemoved` to every surviving claimant **before**
     deleting, or `rebuild.rs::relation_closure` reports a `relations`
     divergence that `doctor --repair` can never fix.
  2. Do **not** roll `next_story_no` back, so a purged number is never reissued.

## Tests it owes

- The narrowed guard still refuses an ordinary attempt: a raw
  `DELETE FROM events` for a live, non-purged story must still `ABORT`. Without
  this, a migration could widen the guard while looking like it narrowed it.
- Purging a story that is not soft-deleted is refused.
- A purged story leaves no divergence — assert `story doctor` is clean
  afterwards, not merely that the row is gone.
- `story_no` is not reissued after a purge.
- `tests/wire_envelope.rs` needs a corpus entry for any new `Response` variant.
- `src/store/test_support.rs::forget_story` already does the raw equivalent and
  is the reference for statement order.

## Then, and only then

`story purge SH-20` against the **real** store — SH-130's proving case. Back up
`~/.local/share/storyhook/store.db` first.

The installed binary is still v2.0.0 and has no migration 4, so the live store
is untouched until it is rebuilt and reinstalled. `make install` is a deliberate
step, not a side effect, and the daemon on :3456 keeps serving old code until
restarted (SH-54).

## Gotchas found the hard way

- **`make x > log; echo $?` reports the echo's status, not make's.** It reported
  "exit 0" over a run with 16 failures. Read the log. (SH-62's log warned about
  this exact trap; it still caught me.)
- Killing a test run leaves daemons behind, and `make gate`'s preflight refuses
  to start until they are gone. `scripts/check-no-orphan-servers.sh` names the
  PIDs and prints the `kill` line.
- `project.json()` appends `--json` itself, and `show`'s payload is doubly
  nested (`["story"]["story"]`) while `list`'s is not.
- `story state set`, not `story state update`; `--super OPEN|CLOSED` uppercase.
- A story's `deleted` field is omitted from JSON when false, so assert it is not
  `true` rather than that it equals `false`.
