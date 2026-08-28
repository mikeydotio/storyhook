# Board ordering and placement: the "Next" sort, "Completed" sort, and blocked column

Design of record for **SH-407**, "Improve sorting" — three asks against the web
dashboard's board:

1. A per-column sort option matching the order `story next --count X` would hand
   this queue out in.
2. The Done column defaulting to completion order rather than priority.
3. A blocked story showing in the Blocked column, not wherever its literal `state`
   happens to be.

## 1. The "Next" sort

### The problem

`COLUMN_SORT_OPTIONS` (`src/web_dashboard.html`) offered Added/Modified/Priority.
`Priority ↓` *coincidentally* resembles `domain::ready_order` (priority ASC, then
story number ASC) but is not the same answer: `story next` also excludes epics
(`!has_children`) and every already-claimed story, and nothing kept the two in
step — a column sorted "Priority ↓" could show an epic first, which `story next`
would never offer at all.

### Why the server computes it

The browser cannot call `story next` itself: `/api/v1/invoke` is loopback-only and
gated on the daemon's master bearer token (`src/api/rpc.rs`), neither of which a
browser tab can present. Re-implementing `ready_order` and the leaf/claimable
filters in JavaScript would be a second, divergence-prone copy of a predicate this
project has already paid for once (SH-240: a second readiness rule written where
the map was inconvenient). So the server computes the queue once, in the same code
path `story next` uses, and ships the order.

### The design

`QueryService::next` (`src/service/query.rs`) used to inline its own filter/sort.
That body became a private helper shared with the dashboard in SH-407, and
SH-450 extends it into
`execution_queue(views, stories, active, phase)`, shared by two callers:

- `next(count, phase)` — truncates the execution order to `count`; its first
  result remains immediately claimable.
- `report_data()` — computes the complete project execution order and stores
  its ids in `ReportData.next_ids: Vec<String>`.

The execution queue is a deterministic Kahn traversal over open `blocked-by`
edges. Its initial frontier is every leaf that passes all non-dependency
`is_claimable` gates. Each selected story virtually completes, reducing its
successors' predecessor counts; newly unblocked work joins the frontier, which
is ordered again by `ready_order`. This means `A blocked-by B` constrains the
answer to `B, A` instead of making A absent. No snapshots or store rows change.
An open predecessor outside the candidate set — claimed, manually blocked,
awaiting, obviated, an epic, outside a requested phase, or in a dependency
cycle — is never virtually completed, so it and downstream work stay unranked.

`next_ids` reaches the wire on `GET .../data` (`src/api/rest.rs`) as a top-level
array, alongside the existing `ready_ids`/`blocked_ids`. It is **not** the same set
as `ready_ids`: `ready_ids` means claimable now, excludes structural epics,
and is not an ordered queue. `next_ids` is the exact ordered answer
`story next --count N` would give and may additionally include future stories
that an earlier entry unlocks.

The dashboard reads it through `nextRank(id)` — the id's index in
`state.data.next_ids`, or `Infinity` if absent. `columnCardCompare`'s `"next"`
branch treats an unranked card as having no position on this axis at all, sorting
it after every ranked card **regardless of `dir`** — the same convention
`"priority"` already established for its own axis (`domain::ready_order`'s fixed
ascending sense, not reversed by the sort direction), and necessary here for a
reason `"priority"` never had to handle: `Infinity - Infinity` is `NaN`, an invalid
`Array.prototype.sort` comparator result, so the unranked/unranked case is handled
explicitly rather than falling out of the arithmetic. Two *ranked* cards can never
tie (an array index is unique), so the reversing `* dir` only ever applies there.

The menu gains `Next ↑`/`Next ↓`, offered only on OPEN-superstate columns
(`columnSortOptionsFor`) — a CLOSED column's contents will never be offered by
`story next` again, so the option would be permanently degenerate there.

### The List Order column (SH-450)

The List view exposes the same `nextRank(id)` as a one-based **Order** column
immediately after ID. A story absent from `next_ids` displays an em dash. The
header sorts in both directions; ranked rows reverse normally, while unranked
rows remain last in either direction because absence from the executable queue
is not a numeric rank to reverse. Two unranked rows fall back to canonical story
number. This is deliberately a consumer of the existing top-level `next_ids`,
not a second per-story field or a JavaScript dependency traversal.

## 2. The "Completed" sort

### The design

A CLOSED column's own default sort changes from
`DEFAULT_COLUMN_SORT = {key: "priority", dir: -1}` to
`DEFAULT_CLOSED_COLUMN_SORT = {key: "completed", dir: -1}` — completion order,
most recently finished first. Priority reads as near-arbitrary for work that is
already done; "what finished last" reads as a log, which is what a Done column
actually is.

`"completed"` keys off `story.closed_at` (`src/domain.rs`), already on the wire
and, until this story, never read by the dashboard. It follows the same-axis,
`dir`-reversing convention `"modified"`/`"added"` already established (`docs/spec/
recency-ordering.md`): lexicographic on the RFC3339 string (no `Date` parsing
needed, the same argument that document already makes for `updated_at`/
`created_at`), tiebroken by `compareWriteOrder` (`head_global_seq`) then story
number, both `* dir`.

**The one case neither "Modified" nor "Added" has to handle**: `closed_at` is
`skip_serializing_if`-omitted for an open story, so it can be `undefined` on the
JS side. That can only happen on a CLOSED column's own default sort for a legacy
row with no `closed_at` at all (a row folded before that field existed). An absent
value sorts **last, regardless of `dir`** — the same "no position on this axis"
convention `"next"` uses for an unranked card, and for the identical reason: a
missing value is not a low or high value on the axis, it is the absence of a
position on it at all, and treating it as an empty string would put it first under
ascending sort — the opposite of "unset sorts last" every other absent-field
convention in this file uses. Two absent values tie on the axis (rather than
short-circuiting to the story-number tiebreak directly) and fall through the
ordinary `compareWriteOrder`-then-`byNumber` chain: two rows that both lack
`closed_at` can still disagree on write order, and that tiebreak is no less
meaningful for them than it is for two rows that tie on a real timestamp.

`head_global_seq` is a story's *most recent* write, not its close event, so this
tiebreak is a correct total order but only an *approximate* completion ordinal for
two stories closed in the same second and edited again afterward (a comment added
post-close, say). Stated here rather than claimed away — no mechanism in this
story corrects it, because doing so would need a per-event ordinal for the close
specifically, which no current field carries.

The menu gains `Completed ↑`/`Completed ↓`, offered only on CLOSED-superstate
columns, for the same reason `"next"` is OPEN-only: an OPEN column's stories have
no `closed_at` at all, so the option would always degenerate to the tiebreak chain.

### Stale persisted sorts

`columnSortFor(slug)` re-validates a persisted `state.columnSort[slug]` entry
against `columnSortOptionsFor(slug)` before using it, falling back to the column's
own default otherwise. This matters because a column's superstate can change after
a sort was persisted — a project reconfiguring a state from OPEN to CLOSED, for
instance — and without re-validation a stale `"next"` or `"completed"` entry would
keep applying to a column that no longer offers it, silently falling through
`columnCardCompare`'s "no position on this axis" case for every card instead of
the default a user would actually recognise.

## 3. Blocked stories in the Blocked column

### The mechanism — server-side, one function

`domain::compute_epic_display_state` (SH-165) computed the Web board's column
override for exactly one case: an epic sitting in the project's neutral default
open state, with at least one child in the active state, promotes to that active
state. SH-407 generalized it to `compute_display_state`, adding a second,
independent promotion under the same eligibility guard (open, non-draft, sitting
in the default open state): a story that is itself `!is_ready` — an `awaiting`
reason, an open `obviated-by` edge, or a `blocked-by` edge onto a story that is
still open — promoted to `"blocked"`.

**SH-487 narrowed that third clause.** Measured on this project's own live
backlog the day SH-487 was filed: **16 of 16 cards sitting in the Blocked
column were plain dependency chains that clear themselves as the backlog is
worked** — e.g. a five-link chain of ordinary `todo` stories, and a story
blocked by another that was already `in-progress`. Not one needed a person.
The column's promise ("this needs special intervention, not the natural
procession of the backlog") was false for every card in it, which is the same
failure mode as a gate that never fires: coverage that exists but that nobody
can trust says anything.

`domain::needs_intervention` replaces `!is_ready` as the third clause's test.
It agrees with `is_ready` on every signal except the last: a `blocked-by` edge
onto a story that is itself open no longer blocks *placement* on its own —
it recurses, and only counts if the blocker (transitively, through as many
hops as the chain has) is closed, literally `blocked`, carrying an `awaiting`
reason, carrying an open `obviated-by` edge, is a draft, or sits in an
unresolvable `blocked-by` cycle. `needs_intervention` is strictly narrower
than `!is_ready` by construction (every case it calls `true` is also a case
`is_ready` calls `false`), so the promotion can only ever remove cards from
the Blocked column relative to the SH-407 rule, never add one. `is_ready`
itself is unchanged and remains the work-allocation predicate: `story next`
and claiming still correctly refuse a story whose dependency is open,
regardless of whether that dependency needs a person. This story changes
**where a card is drawn**, never what work is handed out.

**Narrow by design, not by oversight**: `blocked_ids` (`report_data`, driving
the card's `● blocked (…)` badge and the list row's red left border),
`summary.blocked_count`, `story list --blocked`, the TUI's `blocked` filter
chip, and `story context`'s `## Blocked` section all keep reading `is_ready`
unchanged. A card whose only blocker is ordinary open work now sits in its
own column (Todo, most often) while still carrying the badge naming the real
dependency — the badge answers "is this unblocked," the column answers "does
this need a person," and SH-487 is only about the second question.
`docs/spec/blocked-causes.md`'s own title — "an edge that clears itself
versus prose that doesn't" — is the same dividing line this narrowing puts
into board placement: an edge that resolves itself is not what the Blocked
column is for.

The epic case gets the identical narrowing, through a different mechanism:
since SH-446, an epic's own state is *computed and projected onto
`story.state` itself* by `apply_computed_epic_states`, not overlaid by
`display_state` — `blocked_for_epic`'s `blocked-by` clause changes from "the
blocker's computed superstate is Open" to "the blocker needs intervention,"
recursively. This is the *only* way SH-487 reaches the TUI: the TUI groups a
board column on `story.state` directly (`src/tui/data.rs`) and never reads
`display_state` at all, so narrowing `compute_display_state` alone would have
left every epic-of-blocked-children on the TUI showing blocked forever for a
dependency that was really just next up in the queue.

Both halves share one recursive walk (`walk_needs_intervention`, over an
owned `BlockFacts` value) rather than two independent implementations of the
same signals — this project has paid for that duplication shape before
(SH-136, SH-198, SH-258, SH-260/276, SH-360, SH-364) — with the display side
and the epic rollup differing only in how a blocker's *effective* state is
resolved (already-projected map vs. a `computed_epic_state` call). It reuses
`is_ready`'s clauses rather than a second implementation of them wherever the
two predicates genuinely agree; the function's own eligibility guard already
rules out the cases (`state == "blocked"` literally, draft, closed) that
would otherwise need separate handling.

**Why server-side rather than a board-local JS rule** (council verdict,
`story show SH-407`, unanimous in round 1): `display_state` is already the single
mechanism every board renderer reads through the `display_state || story.state`
idiom — column placement, the state pill and its title, `sortValue`'s `"state"`
case, `storyLight`'s color, the drag-drop no-op guard, diff animations, and now
`filteredStories`'s state filter (below). A board-local placement rule would only
teach `renderBoard` the new fact, leaving every other renderer showing the
story's literal (now-wrong) state — exactly the divergence SH-277's own council
was convened to close for the epic case. Tracing a board-local rule's actual
mechanics surfaces a second, compounding failure it doesn't account for on its
own: an epic already promoted to "in-progress" by the SH-165 rule keeps a literal
state equal to the default-open slug (the promotion never touches `story.state`
itself), so a board-local guard shaped "literal state equals default open" would
fire on it too, silently overriding the already-shipped in-progress placement.

### Precedence when both promotions apply

> **Superseded for epics by SH-446:** epic state is now recursively computed
> from children before board placement. The following precedence discussion is
> retained as the SH-407 design history for the leaf-story display override.

An epic can be eligible for both promotions at once: it has an active child (SH-165
wants "in-progress") and it is itself blocked (SH-407 wants "blocked"). Council
verdict (`story show SH-407`, unanimous after one round of reasoning that
reversed one seat's own initial proposal): **blocked wins**. `compute_display_state`
checks the blocked arm before the active-child arm.

Reasoning: a multi-child epic typically has *some* active child for most of its
life — a near-permanent, low-information condition — while a structural blocker on
the epic's own record is the rarer, more actionable fact a viewer needs surfaced.
It also reaches further than the epic's own card: `storyLight`/`storyRef` color
every *reference* to a story elsewhere on the board (a blocker chip on someone
else's card), so letting "in-progress" win would paint a genuinely-stuck epic as
active everywhere it is cited as a dependency, not only on its own card.

The blocked badge (`report_data`'s `blocked_ids`, and the dashboard's
`blockedFlag()`) needs no change either way — it already derives purely from
`is_ready` on the story's own relationships/`awaiting`, with zero reference to
`has_children` or `display_state`, so it fires independently of which promotion
wins the column.

**Named limitation, not actioned**: an epic that becomes blocked while a child is
still active loses its only board-level signal that work continues underneath
it — there is no progress rollup on the card itself. Visible only by opening the
drawer or finding the active child directly. Left as a known gap; no mitigation
was in scope for this story.

### The filter fix adopted with it

`filteredStories`'s state filter matched `st.state` literally. A filter for
"blocked" would have missed every card SH-407's own promotion just relocated into
that column, since their literal `state` is still e.g. `todo`. Fixed to read
`display_state || st.state`, the same expression every other renderer in the file
uses — adopted into this story under `story help scope-rubric` (the same
one-line-away defect the promotion itself creates, not a separate story).
`f.showClosed`'s superstate check is untouched: `display_state` never crosses
OPEN→CLOSED, so there is nothing for that check to reconcile.

## What guards each piece

| Claim | Pinned by |
|---|---|
| `next_ids` is exactly `story next`'s own dependency-aware answer, by construction (shared helper) | `tests/service_query.rs::report_datas_next_ids_agrees_with_next_by_two_different_routes` |
| A blocked successor follows its blocker and is reprioritized when unlocked | `tests/service_query.rs::next_walks_blockers_and_reprioritizes_each_unblocked_frontier`; `tests/story_next.rs::next_count_orders_a_blocked_story_after_its_blocker` |
| Unexecutable blockers, phase boundaries, and cycles do not manufacture a rank | `tests/service_query.rs::next_phase_does_not_walk_through_an_out_of_phase_blocker`; `::next_leaves_intrinsically_blocked_and_cyclic_subgraphs_unranked` |
| `next_ids` excludes a parent the way `story next` does | `tests/service_query.rs::report_datas_next_ids_excludes_parents_the_way_next_does` |
| The daemon emits `next_ids` on `/data` | `tests/web_test.rs::web_serve_api_data_carries_next_ids` |
| The dashboard reads the literal wire key (not a hand-typed guess on both sides) | `tests/web_test.rs::web_dashboard_js_reads_the_wire_key_next_ids_actually_serializes_to` |
| List renders one-based ranks and sorts both directions with unranked rows last | `tests/web_test.rs::web_serve_root_html_has_board_list_drawer_markers`; `e2e/specs/list-order.spec.ts` |
| The sort menu offers the right options on the right columns | `tests/web_test.rs`'s SH-305/SH-407 block; `e2e/specs/board-sort.spec.ts` |
| A story blocked by an ORDINARY open story does NOT promote (SH-487's own regression test) | `src/domain.rs::a_todo_story_blocked_by_an_ordinary_open_story_is_not_promoted`, and end-to-end via `tests/service_query.rs::a_todo_story_blocked_by_an_ordinary_open_story_is_not_display_promoted` |
| A blocker that itself needs a person promotes its dependent, transitively through a whole chain, a diamond, and a cycle | `src/domain.rs::a_blocker_that_itself_needs_a_person_promotes_its_dependent`, `::intervention_travels_the_whole_blocked_by_chain`, `::a_diamond_reports_intervention_once_and_only_through_the_stuck_arm`, `::a_blocked_by_cycle_needs_a_person_because_it_never_resolves` |
| `needs_intervention` is strictly narrower than `!is_ready` — no story newly enters the Blocked column | `src/domain.rs::needs_intervention_implies_not_ready` |
| A closed blocker does not promote | `src/domain.rs::a_blocker_that_is_closed_does_not_promote` |
| An `awaiting` reason and an `obviated-by` edge each promote on their own; a draft is never promoted as a subject but always needs intervention as a blocker | `src/domain.rs::a_todo_story_with_an_awaiting_reason_is_promoted_to_blocked`, `::a_todo_story_with_an_obviated_by_edge_is_promoted_to_blocked`, `::a_draft_story_is_never_promoted_even_when_blocked`, `::a_draft_blocker_needs_a_person_to_publish_it` |
| The epic rollup and the display promotion agree about who needs intervention (the wiring fence between the two halves of one predicate) | `tests/service_query.rs::the_epic_rollup_and_the_display_promotion_agree_about_who_needs_intervention` |
| An epic whose child merely waits its turn stays at the default open state — the TUI's own regression test, since it never reads `display_state` | `tests/service_query.rs::an_epic_whose_child_merely_waits_its_turn_stays_todo`, `::an_epic_whose_child_is_blocked_by_a_stuck_story_rolls_up_to_blocked`, `::an_epic_of_epics_propagates_a_transitive_block`, `::a_blocked_by_cycle_under_an_epic_rolls_up_to_blocked_without_hanging` |
| A display-promoted story never appears in `next_ids`, tying the predicate to the story's own words ("will eventually be pulled from the backlog … without manual intervention") | `tests/service_query.rs::a_display_promoted_story_is_never_in_next_ids` |
| `blocked_ids`/`summary.blocked_count` stay broad — SH-487 narrows placement only | `tests/service_query.rs::report_data_still_reports_a_naturally_unblocking_story_as_blocked` |
| `filteredStories` filters by the displayed state, not the literal one | `tests/web_test.rs`'s SH-407 assertion on `filteredStories`'s body |
| The promoted-card tooltip names no cause it did not test (the SH-487 adopted fix) | `tests/web_test.rs::the_promoted_state_pill_title_names_no_cause_it_did_not_test` |

## As built

No deviations from either council's verdict. Both trails are on `story show
SH-407` (SH-363: a council's own directory slug resolves on no fresh clone, so it
is never the citation — the verdict is).

**SH-487's own deviation from its first design pass**: the epic-side adapter
(`blocked_for_epic`) gives its intervention walk a fresh, per-call memo and
`visiting` set rather than threading one shared memo through the whole
`apply_computed_epic_states` pass. The shared-memo shape was considered first
and abandoned on a concrete borrow-check failure, not a style preference: the
resolver closure a shared memo would need to capture also has to call
`computed_epic_state` (a second, independent recursion over `parent-of`
edges), and that closure's captured borrow of the shared map is live for as
long as the walk holds it — which collides with also passing the same map
into the walk as a plain argument at the same call site (Rust's ordinary
"cannot borrow more than once" rule, not a special case of anything). The
per-call memo sidesteps the conflict entirely, at the cost of memoizing the
intervention walk only within one child's own call rather than across the
whole project pass. Accepted rather than engineered around further: real
blocking chains in this tool are a handful of hops, and the display side's
own `needs_intervention` already pays the identical per-call cost with no
measured problem.
