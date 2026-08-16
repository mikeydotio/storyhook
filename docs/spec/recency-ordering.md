# Recency ordering: an exact tiebreak for a one-second clock

Design of record for **SH-336**, following on from **SH-329**.

## The problem

Every storyhook timestamp is RFC3339 at one-second precision
(`service::Clock::System`, `src/service/mod.rs:106-124`,
`to_rfc3339_opts(SecondsFormat::Secs, true)`). Every ordering built on those
timestamps is therefore blind inside a second: two stories written within the
same second tie on `updated_at`, and each comparator's tiebreak either ran the
wrong direction or did not exist. This tracker's own normal workload — agents
mutating stories in bursts — makes a same-second write routine, not exotic.

SH-329 first surfaced this as a three-day-blocking e2e flake:
`e2e/specs/board-sort.spec.ts`'s "Modified" test created two stories and bumped
one's priority, all three writes landing in one clock second on a warm machine.
The "Modified" board sort ties on `updated_at` and, before this design, fell
back to story number ascending — creation order, exactly the order the test
existed to disprove. SH-329's own fix (`waitUntilStoreClockPasses`,
`e2e/specs/support.ts`) made the test's writes provably land in different
seconds; it fixed nothing about the product, and filed this story to do that.

**Five surfaces carried the defect** — the three SH-336 named, plus two found
by a sibling sweep after the design was chosen:

| Surface | Location | Defect before this design |
|---|---|---|
| SQL `StorySort::UpdatedAt` | `src/store/sqlite/read.rs` | `story_no` tiebreak ran ascending under a descending sort |
| Dashboard board "Modified"/"Added" | `src/web_dashboard.html` `columnCardCompare` | tiebreak not multiplied by `dir` — same inversion |
| Dashboard List view | `src/web_dashboard.html` `renderList` | no tiebreak at all; relied on `Array.sort` stability over an unspecified server order, and `updated` descending is its *default* |
| TUI recent-activity panel | `src/tui/components/dashboard.rs` `recent_stories` | no tiebreak, then `truncate(5)` — a tie could decide *which* stories appeared, not just their order |

## The decision: a council vote

SH-336's own text: "acceptance is a design decision, not a code change... this
wants a spec or council verdict before anyone edits a comparator." A 3-member
council (data-engineer, software-architect, api-designer) was convened between
two candidates:

- **A — an exact recency ordinal.** Expose the store's existing per-project
  monotonic event sequence (`events.global_seq`, allocated inside the write
  transaction by `allocate_global_seqs`) as `stories.head_global_seq`, and
  tiebreak every comparator on it. Exact by construction — writes are
  serialized behind one process-wide write mutex — at the cost of a schema
  column and plumbing through two wire types and a TUI struct.
- **B — sub-second timestamps.** Move `Clock::System` to millisecond
  precision. No schema or wire changes, but only probabilistically exact
  (two writes in the same millisecond still tie), changes a documented
  `--json` wire format, and silently degrades six existing lexical threshold
  filters (`--created-after`, `--updated-after`, `--stale`, `handoff`)
  against legacy second-precision rows.

**Round 1 was unanimous on the mechanism**: all three seats, researching
independently and blind to each other, proposed Candidate A. The vote split
1-1-1 only on which write-up to endorse (each seat voted for its own).
Deliberation surfaced and resolved a real technical dispute — one seat's
proposal would have placed the new field directly on `StorySnapshot`, which
independent verification (predating the council) had already shown is unsafe:
`StorySnapshot` is the fold of a story's events, serialized verbatim into
`stories.snapshot` and compared field-by-field against a fresh
`fold_story()` call by `story doctor`'s `diff_rebuilt` — a non-fold field
there would report every story as divergent. The winning proposal (round-2
IRV, 2-1 majority) stated the mechanism without that placement error.

No seat argued for Candidate B in either round. The verdict is recorded on SH-336; the
fuller trail (proposals, votes, deliberation, IRV tabulation) was written inside a
worktree that has since been torn down and is not recoverable (SH-363).

## The design as shipped

**`stories.head_global_seq`** (`src/store/schema/0015_story_head_global_seq.sql`,
migration 15): the change-feed position of the event `head_seq` already
names — the same event, in the other coordinate. `head_seq` says which event
*within the story*; `head_global_seq` says where that event sits in the
*project's* write order. `NOT NULL DEFAULT 0`, mirroring `head_seq`'s own `0`
("no event backs this row"). Backfilled by joining `seq = head_seq` against
`events`, not `MAX(global_seq)` — the two differ only on a *stale* row, and
joining on `head_seq` keeps both coordinates of that staleness reported
together rather than contradicting each other.

**Derived, not passed in.** `put_story`'s own upsert (`src/store/sqlite/write.rs`)
computes it with a scalar subquery over `events` in the same statement that
writes every other column — the same technique `archived` already uses there.
This means it cannot drift from the event `head_seq` names, `rebuild.rs`
needed no change to keep writing a correct value, and `refold_story` (which
re-derives a row with no new event) gets a correct answer with no special
case. The one obligation this places on every caller: the event `head_seq`
names must already be committed in the same transaction — true on every path,
verified by `src/store/conformance.rs`'s
`a_story_row_records_the_feed_position_of_the_event_it_was_folded_from`.

**The wire — two types, not one**, because the dashboard and the TUI are fed
by different views:

- `StoryView.head_global_seq: Option<GlobalSeq>` (`src/output.rs`) — the
  dashboard's wire type, reached via `src/api/rest.rs`. `None` from a view
  built without a row read (`query::bare_view`, used by `search`;
  `transfer::import_project`'s bare-view path) or an older daemon. Additive
  and `skip_serializing_if`-guarded — the same shape `display_state` already
  established on this struct.
- `ProjectSnapshotView.head_global_seqs: BTreeMap<String, GlobalSeq>`
  (`src/output.rs`) — the **TUI's only wire type**; it never sees a
  `StoryView`, only `stories: Vec<StorySnapshot>`, so the ordinal has to
  arrive alongside the snapshots rather than inside one, the same way
  `drafts` already does.

**Every comparator**, in the direction the council's tiebreak-total-order
argument requires (a same-axis tiebreak reverses with the sort direction; a
different-axis tiebreak, like priority's, does not):

| Surface | Comparator | Tiebreak |
|---|---|---|
| SQL | `StorySort::UpdatedAt` | `ORDER BY updated_at DESC, head_global_seq DESC, story_no` — `story_no` kept as the residual total-order key for the all-zero (`extra_rows`) case |
| Board "Modified" | `columnCardCompare` | `compareWriteOrder(a, b) * dir`, then `byNumber * dir` |
| Board "Added" | `columnCardCompare` | `byNumber * dir` only — story numbers are allocated by the same serialized write transaction as creation, so they already *are* creation order exactly |
| List view "Updated" | `renderList` | same as the board's "Modified" |
| TUI recent activity | `recent_stories` | `data.head_global_seqs` lookup, `None` sorting after every `Some` so an unknown ordinal degrades to the incoming order rather than inventing one |

## What guards each piece

| Claim | Pinned by |
|---|---|
| The column is exactly the `global_seq` of the event `head_seq` names | `store/conformance.rs::a_story_row_records_the_feed_position_of_the_event_it_was_folded_from` |
| A same-second write burst orders exactly, not by story number | `store/conformance.rs::the_recency_order_resolves_writes_that_share_a_second` |
| Migration 15 backfills correctly, including a stale-row case and a no-event row | `tests/store_migrations.rs::migration_fifteen_*` (four tests) |
| The recency index still covers the sort it names | `tests/store_migrations.rs::migration_fifteen_leaves_the_recency_index_covering_its_sort` |
| A row that disagrees with its events (fabricated outside `put_story`) is caught and fixable | `tests/doctor.rs::doctor_reports_and_fixes_a_head_global_seq_that_disagrees_with_its_events` |
| The daemon actually emits the field on `/data` | `tests/web_test.rs::web_serve_api_data_carries_head_global_seq` |
| The dashboard JS reads the literal key the wire actually serializes to (not a hand-typed guess on both sides) | `tests/web_test.rs::web_dashboard_js_reads_the_wire_key_head_global_seq_actually_serializes_to` |
| The board's "Modified" sort follows `head_global_seq` in both directions | `e2e/specs/board-sort-tiebreak.spec.ts` |
| The List view's "Updated" sort follows it in both directions, independently of the board's comparator | `e2e/specs/board-sort-tiebreak.spec.ts` |
| The TUI panel orders a tie by write order and does not drop a story from the top 5 | `src/tui/components/dashboard.rs`'s own `mod tests`: `recent_activity_ranks_a_same_second_tie_by_which_story_was_written_last`, `a_story_written_last_in_a_second_is_not_dropped_from_recent_activity` |
| An older daemon (no ordinal known) degrades to today's behaviour rather than inventing an order | `recent_activity_falls_back_to_the_incoming_order_when_no_ordinals_are_known` |

## As built

No deviations from the council's verdict. The one implementation detail the
council's question did not scope — where on the wire the field lives — is
recorded above under "The wire" rather than as a deviation, since no
candidate proposal's specific placement claim survived the runoff unchanged.
