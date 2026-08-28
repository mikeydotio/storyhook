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
                     unresolvable id (another project, permanently deleted, a draft)
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

No server change was needed. `GET .../data` already ships every persisted,
non-draft story in the project with its `state`/`superstate`/`relationships`, and
relations are always intra-project (`RelationService::relate` writes both ends in one
transaction) — so any `other_id` resolves client-side through `findStory()` with zero
extra requests. The gaps that follow from that (a draft, a permanently deleted story, or a
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

Deliberately **not** adopted here (filed as follow-on stories rather than folded in,
since each needed its own design pass): the list view's `.state-pill` — colouring it
was an obvious, cheap follow-on that the semantic palette made worth doing, later done
as **SH-277** (its own "As built" entry below) — and the description field, a
`<textarea>` where linkification needs a read/edit split the drawer doesn't otherwise
have (filed as SH-278; that story was later superseded by SH-217, which built the split
as part of rendering the description as markdown — see
[`markdown-in-the-dashboard.md`](markdown-in-the-dashboard.md)).

### Consumer 2 — the card blockers list, then the blocked badge (SH-309)

`populateCard()` originally gained a `.card-blockers` row here: every story still
blocking this one, each with its own light, capped at 3 with a `+N` overflow chip
(mirroring `.card-labels`). SH-309 moved the *live* half of that row into the blocked
badge instead (`.flag-blocked`, `blockedFlag()`) — see its As-built entry below for why
and what `.card-blockers` is left holding.

`openBlockers()` still mirrors the server's own `is_ready` rule (`src/domain.rs`)
client-side for the `blocked-by` half — a relationship only counts while its target
resolves in the loaded project and is still OPEN — reading `st.relationships` directly,
since relations are written on both ends and no reverse index or second fetch is
needed. `blockedFlag()` adds a second, deliberately *unfiltered* half for `obviated-by`:
`is_ready` blocks on any such edge regardless of resolvability, so an edge this client
can't resolve is still a real cause, rendered via `storyRef()`'s "unknown" ring rather
than dropped (council decision, unanimous in round 1; the verdict is on SH-309).

### The cleared-blocker dwell

`populateCard()` rebuilds a card's children from scratch every render (`clear(card)`),
so the dwell — "stay visible, lit green, for a few seconds after closing" — can't live
in the DOM. It's a module-level ledger instead:

- `clearedBlockers["<storyId>|<blockerId>"] = timestamp`, `BLOCKER_CLEARED_DWELL_MS`
  (4000ms).
- `recordClearedBlockers(oldData, newData, now)` scans every `blocked-by` relationship
  in the previous snapshot project-wide (not just the changed story's own — and SH-500
  durably removes that edge from the fresh snapshot) for a target that crossed OPEN →
  CLOSED, called from `fetchData()` alongside the existing `diffSnapshots()` call,
  before the previous snapshot is discarded.
- `dwellingBlockerIds(storyId, now)` is what `populateCard()` reads to fill
  `.card-blockers` (SH-309: this is now the row's *only* content — the live blockers
  `openBlockers()` finds render in the badge instead, never here). An expired entry is
  pruned lazily, at the next ask, rather than swept on a timer.
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
server: under the former soft-delete contract, deleting the spec's blocker while it
sat in `done` 404'd because closing set the store row's `archived` flag. SH-498 later
replaced that contract: permanent deletion accepts OPEN and CLOSED stories alike and
retracts their edges. `e2e/specs/support.ts`'s shared cleanup now force-deletes either
shape directly, so the spec can still leave its blocker CLOSED on purpose and let the
sweep remove it without a reopen dance.

### The blocked badge had to test every cause `is_ready` tests, not just `awaiting` (SH-309)

Filed as "SH-307 shows 'Blocked (no reason)' but SH-308 is the reason." The badge
(`populateCard`, pre-SH-309) tested only `st.awaiting`; `blocked_ids` (`src/service/
query.rs`) is driven by `is_ready`'s five-clause test (`src/domain.rs`), so a story
blocked by an open `blocked-by` edge or an `obviated-by` one got the same
"(no reason)" label as one genuinely parked with none — even while the blocking
story's own chip sat one row below it in `.card-blockers`, printed twice.

`blockedFlag(st, isBlocked)` is now the single owner of every badge sentence: a
comma-joined cause list (open `blocked-by` refs, then `obviated-by` refs — deliberately
unfiltered by resolvability, see the council decision above — then a quoted `awaiting`
reason), falling back to the doctor's own "(no reason)" only when the list is empty and
`state === "blocked"`, and to a bare "blocked" with an explanatory title when even that
doesn't apply (a `blocked-by` edge onto an open draft blocks server-side but is invisible
to this client because `src/api/rest.rs` routes drafts to a separate array). Permanent
deletion retracts the relationship, so it cannot leave this gap.

The live half of `.card-blockers` moved into the badge to stop the double-print; the
row is now the cleared-blocker dwell's home alone (see above). `.card-flags` gained
`flex-wrap: wrap` as a consequence — three `.rel-id` refs alone are 132px of
irreducible width (`--tap-min`, 44px under `pointer: coarse`) inside a 320px-viewport
card that clips nothing.

`isBlocked` (`blocked[st.id]`, the server's whole-project `blocked_ids`) is threaded
through but deliberately does **not** gate the cause list itself, only the two
fallback branches: `blocked_ids` is a project-wide aggregate the server recomputes on
its own `/data` fetch, while a relate/block mutation's own response patches just the
one story's `relationships`/`awaiting` in place and can render before the next `/data`
reply lands — `stale-data-response.spec.ts`'s own scenario, and how this was caught: a
first pass that gated the whole badge on `isBlocked` went red there, because a sealed
board fetch left `blocked_ids` stale while the mutation had already landed. Since every
clause the cause list derives from is one `is_ready` already tests, a non-empty list
implies the server would agree once its own aggregate catches up, so trusting the
fresher, locally-derived signal is strictly more correct than waiting on the aggregate
— exactly the property `.card-blockers`/`openBlockers()` already had, and would have
silently lost if the badge had swallowed it behind a single `blocked[st.id]` gate.

### SH-277: the list view's `.state-pill`, and a second renderer found reading the wrong slug

SH-446 later replaced the epic-specific display promotion described below with
structural epics whose `state` is recursively computed before this renderer sees
them. The SH-407 leaf-story `display_state` override remains; the SH-165 details
in this section are retained as the history of why every renderer was unified.

Colouring `.state-pill` turned out to need a prerequisite fix first, not just new CSS.
Every other renderer in the file already read `display_state || state` — the board's
column placement, the drag-drop no-op guard, the render diff, and this story's own
`storyLight()` — specifically so a display-promoted epic (`compute_display_state`,
named `compute_epic_display_state` at the time this story shipped and generalized to
its current name by SH-407, which added a second promotion beside the epic one below —
SH-165: an active child pulls a `todo` epic's *card* into `in-progress` without
touching its own literal `state`) never disagrees with the column its card actually
sits in. `populateListRow` was the one holdout, reading the literal `st.state` in two
places: the pill's text and `sortValue`'s `"state"` case. Colouring the pill from the
literal state would have painted it the wrong colour for exactly the epics this
story's own reasoning cares about, so both were fixed to read `display_state || state`
first, each shipping its own e2e proof, ahead of the colour commit (two hats). The
promoted pill also carries a `title` naming the literal recorded state, so the table
never silently disagrees with `story show`. The state *filter* (`filteredStories`)
kept reading the literal state on purpose at the time — filters and rendering were a
deliberate two-rule split in this file — until SH-407 gave `compute_display_state` a
second promotion (a story blocked on its own record, not just an epic with an active
child) that a literal-state filter would have hidden from a "blocked" filter the
instant it fired: the card visibly sits in the Blocked column but a filter reading
`st.state` alone would still see `todo` and drop it. `filteredStories` now reads
`display_state || st.state` too, closing that gap by extending the same rule rather
than inventing a third one; see `docs/spec/board-ordering-and-placement.md` for the
full SH-407 design.

The colour itself is carried by a `--state-color` custom property — the same idiom
`.card`'s own `--card-accent` uses — because one write has to drive three things at
once (the pill's `color-mix()` tint, its ring, and a `.dot` inside it matching the
board's own dots), unlike every single-declaration `stateColor()` consumer elsewhere,
which sets an inline `background` string directly. The plain `background`/`border-color`
declarations stay first in the CSS, with the `color-mix()` pair after them, so a
browser without `color-mix()` support silently keeps today's uncoloured pill.

## What guards each piece

| Piece | Structural test (`tests/web_test.rs`) | Behavioral test (`e2e/specs/`) |
|---|---|---|
| Component source/CSS exist | `web_serve_root_html_has_board_list_drawer_markers` (SH-203 block) | — |
| `.story-light`/`.story-ref`/`.card-blockers` CSS values | `web_serve_root_html_styles_the_status_light_and_card_blockers` | — |
| A relation's light matches its target's real colour | — | `story-status-light.spec.ts`: "a relation's status light matches..." |
| Semantic vs. positional palette | — | `story-status-light.spec.ts`: "the Done column's own dot is green..." |
| Comment-body linkification (real / unknown / self) | — | `story-status-light.spec.ts`: "a comment naming a real story renders a lit link..." |
| Cleared-blocker dwell | — | `card-blockers.spec.ts` |
| Blocked badge derives from one function, never a hand-written cause | `every_blocked_badge_sentence_comes_from_the_one_deriver` (SH-309 fence, models `every_loading_line_comes_from_the_one_generator`) | — |
| `.card-flags` wraps | `web_serve_root_html_styles_the_status_light_and_card_blockers` | — |
| Badge names blockers/two-blocker comma-join/awaiting+blocker/obviated-by, and its ref click-throughs to the *blocker's* drawer | — | `status-flags.spec.ts` (SH-309 cases) |
| List view `.state-pill` reads `display_state`, and its `color-mix()` mechanism | `web_serve_root_html_has_board_list_drawer_markers` (SH-277 block), `web_serve_root_html_colours_the_list_state_pill` | `list-state-pill.spec.ts` |

Computed colour, click behavior, and the dwell's timing are all things only a real
browser can prove — the Rust layer is what catches a rename or deletion in seconds,
without one.
