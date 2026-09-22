# SH-752: Remove assignees and members

StoryHook is a single-user tool. Remove team members and story assignment from
the domain, commands, services, storage, APIs, reports, and both user interfaces.
Keep agent claims, dispatch ownership, and actor provenance.

## Data transition

Purge structured assignment data from the live database in one new migration.
Delete assignment and assignment-clear events. Remove the member table, the
assignee column and index, and the assignee snapshot key. Preserve surviving
event bytes and sequence numbers. Preserve the global allocation counter.
Recompute story heads and activity timestamps from the surviving history.
Use the existing pre-migration backup and transactional rollback mechanisms.
Restore event guards before commit. Do not modify prior migrations.

Existing backup files, input files, and free-text comments remain unchanged.
Legacy migration and restore discard retired events before typed decoding.
Imports ignore retired member and assignee fields. Unknown future event kinds
retain their current preservation rules. New exports omit retired fields.

## Interfaces

Remove assignment commands, flags, routes, tools, and controls. Reject retired
assignment fields in current story mutation payloads. Normalize old dashboard
preferences by removing assignee filters and resetting assignee sorting to the
normal default. Preserve other selections, including exact empty selections.

## Validation

Test fresh and existing stores, rollback, reopening, replay agreement, trailing
assignment events, subsequent writes, multiple projects, and old imports.
Test removed command surfaces, output contracts, TUI undo, and browser flows.
Run the changed-tree selector and only new and directly impacted tests.
The central verifier owns the full suite and submission.
