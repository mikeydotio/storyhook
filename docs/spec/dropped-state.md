# Dropped abandonment state (SH-663)

`dropped` means work deliberately abandoned. `done` means work completed.
Both have superstate `CLOSED`. Custom CLOSED states remain supported.
The CLI verb `story close <id> "reason"` remains, targeting `dropped`.
Dashboard abandonment actions say “Drop”; the filter says “Show dropped”.

## Compatibility contract

- Rename an existing `closed`/CLOSED catalog entry in place, preserving order,
  role and description. Add `dropped` when no abandonment entry exists.
- Preserve an existing `closed`/OPEN custom entry. Never reinterpret its events.
- Refuse `dropped`/OPEN or a catalog containing both `closed`/CLOSED and
  `dropped`, with the project and conflicting name in the error. Roll back the
  migration. Resolve the conflict with the previous binary before retrying.
- Keep historical events immutable. During replay, resolve `closed` to
  `dropped` only if `closed` is absent and `dropped` is CLOSED. Old catalogs
  remain directly replayable. Imports normalize their catalogs before replay.
- Refuse new states named `closed`: reusing that name could reinterpret old
  history. Existing OPEN entries remain editable.
- Patch only the state in stored rows and snapshots, including surviving
  legacy soft deletes. Preserve timestamps, sequence heads, comments, relations,
  archive flags and visibility. The rebuild oracle must agree after migration.
- Preserve superstate labels, `--include-closed`, GitHub PR closure vocabulary,
  historical migrations, and historical documentation. Explain the rename in
  current help and documentation.
- Fix reopen hook provenance separately: `from_state` comes from the previous
  snapshot, for done, dropped, and custom states alike.

## Validation and handoff

Use failing regressions for catalogs, migration atomicity/idempotence, replay,
imports/exports, CLI close/reopen, and browser abandonment/filter flows. Run the
impacted-test selector on the changed tree and run new/directly impacted tests.
The centralized verifier owns the full suite and merge.

## Execution checklist

- [x] Post the exact approved plan on SH-663.
- [x] Add regression tests and implement the rename and migration.
- [ ] Fix reopen hook provenance in its own commit (in progress).
- [ ] Validate, commit, push, and link one PR.
- [ ] Move SH-663 to verifying as the last action.
