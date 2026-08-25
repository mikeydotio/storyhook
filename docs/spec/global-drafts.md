# Global drafts

Design of record for **SH-442**: drafts are dashboard-wide working material,
not a property of whichever project board happens to be open.

## Data source

`GET /api/repos` already reads `report_data()` for every registered project on
each catalog refresh. Each readable entry now includes a `drafts` array filtered
through the same server helper as `GET /api/repos/{id}/data.drafts`: unpublished
and not soft-deleted. This is additive to the catalog wire shape and avoids a
fan-out of one browser request per project merely to draw the global list.

The client derives the count and rows directly from `state.repos`, preserving
catalog order and each project's existing story order. If the catalog has not
settled, or any project entry failed, the button says only `Drafts`; known rows
remain visible beside a retrying warning, but no incomplete global count is
claimed.

## Editing

A catalog draft is a discovery snapshot, not an editing authority. Clicking a
row fetches the owner's current `/data`, finds the same id in its current
`drafts` array, and opens the existing create/edit modal with that response's
vocabulary. The read is ticketed, so an older response cannot win after a later
selection. A draft published or discarded since the catalog snapshot produces
a notice and catalog refresh rather than a stale editor.

The modal pins its Project select to the owner. Global access is not draft
transfer: save, publish, discard, and label edits continue to address
`/api/repos/{owner}/story/...`. Each successful draft mutation refreshes the
global catalog immediately and refreshes board data only when that owner is the
board currently on screen.

## Surfaces and states

The Drafts control is visible on Home, project boards, and Settings. Each row
shows id, title, and `PREFIX · project name`; unbounded project names use the
same one-line ellipsis treatment as the project selector. The surface
distinguishes loading, total catalog failure, partial project failure, and an
earned global empty state (`No drafts.`).
