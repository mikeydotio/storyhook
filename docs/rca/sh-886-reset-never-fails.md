# SH-886: A story reset made its story more wedged

## Failure and cause

On 2026-10-05 at 03:19Z a dashboard Reset of SH-801 closed its window and
removed its worktree and branch, then failed its final store write: SQLite's
5 s busy timeout expired at `BEGIN IMMEDIATE` ("timed out waiting for the
project write lock"). The receipt stayed unfinished, the worker exited, the
poll reported `error`, and the reservation refused every later lifecycle
change. The final lever left the story more wedged than before.

Three causes combined:

- The card reset controller opened its own SQLite store, so its writes
  competed with all of the daemon's own write traffic through the busy timeout
  instead of queueing on the store's in-process mutex. The host was heavily
  loaded by a central gate and a measurement campaign.
- No reset write retried contention, and nothing resumed an unfinished reset;
  only a person clicking Reset again could finish it.
- Every reset entry point was a chain of preconditions: about fifty identity
  and lock checks, quiesce deadlines, competing owners and blocker ordering
  each aborted the reset and left the reservation in place.

The owner's requirement (SH-886) is that a reset never fails for any reason.

## Correction

A reset is now a convergent operation with one contract for the dashboard,
`story reset` and `/story reset` (council C1). Once reserved, its only
mandatory effect is the finish transaction; teardown is best effort, keeps
every ownership proof, and reports what it could not remove as residue
instead of refusing. Store contention is waited out. A daemon reset runtime
drives every reservation to completion on the daemon's own store and resumes
unfinished ones at startup and on a sweep. Locks are waited for and then
ignored; competing owners are superseded through their explicit release
operations; the finish ignores blocker ordering and degrades instead of
refusing. A recovery record names the deleted branch tip and how to restore
it. Residue that would collide with the next dispatch holds the story with an
awaiting reason instead of letting Full Auto quarantine it.

A defect found on the way was fixed in its own commit: the shared tmux
inventory rejected every dead pane (its current path is empty), so no reset
could remove a dead story window. One trap is recorded for the next reader:
`git rev-list --exclude` before `--branches` needs the bare branch name, not
`refs/heads/<name>`; the recovery count first got it wrong, and its test
caught it before commit.

Two defects found on the way were already present on the base and are filed
separately: SH-887 (`test-engine-reset.sh` verifying-story assertions) and
SH-888 (the e2e real-dispatch spec reads the host's default tmux server).

## Regression evidence

- `tests/story_reset/busy.rs` refuses every other write with `Busy`, as
  `BEGIN IMMEDIATE` does; all three cases failed with the SH-801 message
  before the fix.
- `tests/story_reset/residue.rs`, one scenario per refusal class.
- `src/daemon/reset/tests.rs`: startup resume, panic release, stand down.
- `tests/engine_reset.rs`, `tests/landing_intents.rs` and
  `tests/batch_landing.rs`: supersession of Stop Now, landing and batch
  members.
- `resources::tmux::tests::a_dead_pane_is_identified_by_its_start_directory`.

## Defect class

A write made after an irreversible side effect must not be lost to transient
contention, and an operation whose purpose is recovery must not have a
precondition that the condition it recovers from can violate. Reset's writes
go through `store::patience`; Stop Now's finish and diagnostic writes were
swept for the same class.
