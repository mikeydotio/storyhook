# The status light: a reusable colour-coded dot on every story reference

Design of record for **SH-203**, which absorbed **SH-200** ("List blocking stories on
a story card, with a status light on each that matches that story's column"). Written
after implementation for the same reason [`dashboard-dispatch.md`](dashboard-dispatch.md)
and [`responsive-dashboard.md`](responsive-dashboard.md) give: sharper against the actual
code than against a proposal for it.

## Context

Every place the web UI names another story — a relation row, a `referenced_by`
mention, a comment body, a block reason — rendered a bare ID. A reader could not tell
whether `SH-100` was done, in flight, or itself blocked without clicking through.
SH-203 asked for one reusable status-light component: a coloured dot immediately
before the ID, matching that story's own board column, with the status word reachable
as hover/`alt` text rather than occupying space. Its first two consumers, absorbed from
SH-200 rather than shipped separately (the story's own rationale: split, the second
implementation would re-litigate the colour mapping and hover-text decision the first
one settles): every inline story reference, and a story card's blockers list with a
green-flash-then-remove transition when a blocker closes.

## Decisions taken

Two questions this project's own council (`/council-vote`) or the plan's own analysis
resolved before implementation:

1. **The palette had to become semantic, not stay positional.** `stateColor()` indexed
   `STATE_PALETTE` by a state's position in `meta().states` — with the default catalog
   (`todo`, `in-progress`, `blocked`, `done`, in that configured order) `done` landed on
   index 3 and painted every completed story **red**. A status light that colours by
   column has to mean something, and the story's own "a cleared blocker's light turns
   green" would have been flatly false under the old mapping. Resolved by anchoring the
   four `REQUIRED_STATES` slugs (`src/domain.rs`) to semantic tokens — `blocked` →
   `--danger`, any CLOSED state → `--success`, the `role: "active"` state → `--accent`,
   `todo` → `--fg-faint` — before falling back to the original positional palette for
   anything a project adds beyond that floor, which has no slug to anchor to. Still one
   mapping, as the story requires — now a meaningful one, and it re-colours the board's
   own column dots and Set Status submenu as a side effect, deliberately: one mapping,
   not two.
2. **Comment bodies are linkified.** The story's consumer-1 list names "comment bodies
   that name an ID" as a target surface, but comment text was plain text with no ID
   ever treated as a link. Resolved by adding `linkifyStoryIds()`, applied to comment
   bodies and the blocked banner's `awaiting` reason — both free-text fields where an
   author routinely types another story's id (the block-reason placeholder's own
   example is literally "e.g. waiting on SH-9").

## The design

### The component (`src/web_dashboard.html`)

```
storyLight(id)   -> a coloured .dot resolving id against the loaded project's
                     stories (findStory), coloured via stateColor(); an
                     unresolvable id (another project, soft-deleted, a draft)
                     gets a hollow ".unknown" ring rather than being dropped
storyRef(id)     -> storyLight(id) + a clickable .rel-id button (opens id's
                     drawer), wrapped in .story-ref
linkifyStoryIds(text, selfId)
                 -> splits free text into plain strings and storyRef() spans,
                     mirroring the server's own id-finding rule
                     (ids_in_line/derive_comment_mentions, src/domain.rs):
                     the project's prefix (read off selfId), a dash, digits,
                     not preceded by an alphanumeric character. A self-
                     reference and an id that doesn't resolve both stay
                     plain text.
```

No server change was needed. `GET .../data` already ships every non-deleted,
non-draft story in the project with its `state`/`superstate`/`relationships`, and
relations are always intra-project (`RelationService::relate` writes both ends in one
transaction) — so any `other_id` resolves client-side through `findStory()` with zero
extra requests. The gaps that follow from that (a draft, a soft-deleted story, or a
different project's id all resolve to nothing) are exactly what `storyLight()`'s
"unknown" ring exists for.

`storyRef()`'s button calls `e.stopPropagation()` — needed once it started rendering
inside a board card (consumer 2), which is itself a whole-card click target; harmless
everywhere else it renders, since nothing else it sits inside listens for clicks.

### Consumer 1 — inline references

Adopted by:

- The drawer's Relationships section (`buildRelationshipsSection`) — both direct
  `relationships` and derived (`ancestor-of`/`descendent-of`) relations.
- The drawer's Referenced By section's comment mentions (`buildReferencedBySection`).
- Comment bodies (`buildCommentsSection`), via `linkifyStoryIds`.
- The blocked banner's `awaiting` reason (`renderDrawer`), via `linkifyStoryIds`.

Deliberately **not** adopted (filed as follow-on stories rather than folded in here,
since each needs its own design pass): the list view's `.state-pill` — colouring it is
now an obvious, cheap follow-on that the semantic palette makes worth doing (SH-277)
— and the description field, a `<textarea>` where linkification needs a read/edit
split the drawer doesn't otherwise have (filed as SH-278; that story was later
superseded by SH-217, which built the split as part of rendering the description as
markdown — see [`markdown-in-the-dashboard.md`](markdown-in-the-dashboard.md)).

### Consumer 2 — the card blockers list

`populateCard()` gains a `.card-blockers` row: every story still blocking this one,
each with its own light. `openBlockers()` mirrors the server's own `is_ready` rule
(`src/domain.rs`) client-side — a `blocked-by` relationship only counts while its
target resolves in the loaded project and is still OPEN — reading `st.relationships`
directly, since relations are written on both ends and no reverse index or second
fetch is needed. Capped at 3 with a `+N` overflow chip, mirroring `.card-labels`.

### The cleared-blocker dwell

`populateCard()` rebuilds a card's children from scratch every render (`clear(card)`),
so the dwell — "stay visible, lit green, for a few seconds after closing" — can't live
in the DOM. It's a module-level ledger instead:

- `clearedBlockers["<storyId>|<blockerId>"] = timestamp`, `BLOCKER_CLEARED_DWELL_MS`
  (4000ms).
- `recordClearedBlockers(oldData, newData, now)` scans every `blocked-by` relationship
  project-wide (not just the changed story's own — the story *naming* the blocker may
  not itself have changed) for one that crossed OPEN → CLOSED between the previous and
  fresh `/data` snapshot, called from `fetchData()` alongside the existing
  `diffSnapshots()` call, before the previous snapshot is discarded.
- `dwellingBlockerIds(storyId, now)` is what `populateCard()` reads to add the
  still-dwelling entries alongside `openBlockers()`'s live ones. The two sets never
  overlap (a dwelling entry's target is CLOSED by definition; `openBlockers()` requires
  OPEN). An expired entry is pruned lazily, at the next ask, rather than swept on a
  timer.
- Nothing else was guaranteed to trigger a re-render exactly as the dwell ends, so
  `fetchData()` arms one `setTimeout(renderView, ...)` whenever a clearance was found.
- The pulse (`.blocker-cleared .story-light`, reusing `pulse-success`) is gated behind
  `prefers-reduced-motion` like every other card flash animation. The dwell itself is
  not — it's information (which blocker cleared), not decoration.

### The drawer live-refresh gap

A referenced story's light in an open drawer (a relation row, a referenced-by mention)
reads `state.data` via `findStory()` — but `renderAll()` only re-renders the
board/list, never an open drawer; `renderDrawer()` otherwise runs only from
`handleMutationSuccess()`, and only for the drawer's own story. Without a fix, a
blocker (or any referenced story) closing elsewhere would leave every light in an open
drawer stale until it was closed and reopened. `fetchData()` now re-renders the drawer
whenever a live update actually changed something (never on a diff-free poll tick, so
the 25s safety poll doesn't clobber in-progress focus/caret for nothing).

## As built

### `.rel-id` had to stop being `.rel-row`-scoped

`storyRef()` made `.rel-id` a shared building block that now renders in three places
that were never a `.rel-row`: `.referenced-by-text`, `.comment-text` (via
`linkifyStoryIds`), and the card blockers list. The pre-existing CSS —
`.rel-row .rel-id { ... }` and a `.rel-row button` rule carrying the tap-target floor —
only ever matched inside an actual `.rel-row`. Unscoping both (now `.rel-id` and
`.rel-remove`, the delete button, each carrying their own sizing) is what makes a
`storyRef()` in a card or a comment look and behave the same as one in a relation row,
rather than rendering with default browser `<button>` chrome. Also fixed, as a
byproduct: the referenced-by mention's `.rel-id` button had *never* been inside a
`.rel-row` even before SH-203, so it had been getting none of that styling all along —
a latent inconsistency this generalization incidentally closes.

### A CLOSED story is already `archived` at the store row level

Found writing `card-blockers.spec.ts`'s cleanup step, not by reasoning about the
server: deleting the spec's blocker story directly while it sat in `done` 404'd
("story `AA-3` not found"). `StoryService::delete` refuses only when `row.archived` is
true — and closing a story sets that flag (`StoryClosedAndArchived`), independent of
the dashboard's own `hidden_at`/"Archive" UI action, which is a further, separate flag
on top. `e2e/specs/support.ts`'s `cleanUpCreatedStories` already documents and handles
this (reopen a CLOSED stray, then delete); the spec now leaves its blocker CLOSED on
purpose — that's the thing being tested — and lets that shared sweep clean it up
rather than duplicating the reopen dance.

## What guards each piece

| Piece | Structural test (`tests/web_test.rs`) | Behavioral test (`e2e/specs/`) |
|---|---|---|
| Component source/CSS exist | `web_serve_root_html_has_board_list_drawer_markers` (SH-203 block) | — |
| `.story-light`/`.story-ref`/`.card-blockers` CSS values | `web_serve_root_html_styles_the_status_light_and_card_blockers` | — |
| A relation's light matches its target's real colour | — | `story-status-light.spec.ts`: "a relation's status light matches..." |
| Semantic vs. positional palette | — | `story-status-light.spec.ts`: "the Done column's own dot is green..." |
| Comment-body linkification (real / unknown / self) | — | `story-status-light.spec.ts`: "a comment naming a real story renders a lit link..." |
| Card blockers list + cleared-blocker dwell | — | `card-blockers.spec.ts` |

Computed colour, click behavior, and the dwell's timing are all things only a real
browser can prove — the Rust layer is what catches a rename or deletion in seconds,
without one.
