# SH-558 Handoff

## Audit result

- Audited the 41 stories that were TODO when SH-558 planning finished.
- Closed duplicate attachment stories SH-379 through SH-385 in favor of
  canonical SH-387 through SH-393.
- Closed browser-flake stories SH-349, SH-375, SH-378, and SH-395 as obviated
  by SH-501's later diagnosis.
- Closed SH-518 as obviated by SH-523 and SH-544.
- Retained 28 actionable TODO stories without comments, preserving their age.
- Left SH-559 untouched because another session claimed it after the snapshot.
- Added `human-only` to SH-327 and `no-auto` to SH-553.

## Repository record

- Updated the project Mini-roadmap in both currently mirrored locations.
- No public API, schema, CLI behavior, or production logic changed.
- Story comments and relationships are the durable audit record.

## Submission contract

- Branch: `worktree-SH-558`.
- Tracker JSON assertions passed; `story doctor` reported zero integrity
  findings and only pre-existing operational/history advisories.
- `cargo fmt --check`, the focused scope-rubric pin, all 19 service-system
  tests, and all 12 scaffold tests passed. The scaffold test required its
  expected local-socket sandbox escalation.
- One linked PR remains open for centralized verification.
- The verifier owns the full suite, merge, completion, and cleanup.
