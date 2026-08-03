# Handoff — SH-64: the id-ordering split, unblocked by SH-63

**SH-63 is done, PR open from this worktree.** The run itself is described by
[`HARDENING_PROGRESS.md`](HARDENING_PROGRESS.md) — read its **START HERE**
section first, and SH-63's own log entry for what changed. That is the
process; this file is only what comes next.

## Take SH-64

SH-64 was `blocked-by SH-63`; SH-63's merge clears that. It is `low` priority,
so `story next` will not lead with it — pick it up deliberately, the way
SH-63's own entry records this run doing for SH-119.

**What's actually left, now that SH-63 narrowed it.** SH-63 fixed the
*ready-list* comparator (`next`, `summary`, `report`, `context`) to rank by
`domain::ready_order` — priority, then story number, a total order. SH-64 is
the *other* half of the same defect family: two different id orderings ship
in the same binary.

- **Numeric** (`SH-2` before `SH-10`): `list`, `search`, `epic list`,
  `phase show` — via `service::query::sort_story_views`, which itself now
  delegates to `domain::story_number` (folded there in SH-63's refactor
  commit).
- **Lexicographic** (`SH-1, SH-10, SH-11, SH-2, …`): `graph`'s roots/leaves
  and `handoff`'s created/updated/closed sections — both iterate
  `story_map`'s `BTreeMap<String, StorySnapshot>` directly, keyed by the id
  *string*. `query.rs`'s module doc comment names both explicitly as the
  remaining defect; `tests/service_query.rs`'s
  `graph_reports_roots_and_leaves_in_lexicographic_id_order` and
  `handoff_lists_open_stories_then_archived_ones_each_lexicographically`
  pin the current (wrong) bytes and are the two tests this story flips.

A sibling SH-64 names on its own story: `story phase list` sorts phases by
**label text** (`BTreeMap<String, _>` keyed on `phase:<n>`), so phase `10`
sorts before phase `2`. Decide that one in the same story, per SH-64's text.

**The golden CLI corpus moves.** Unlike SH-63, this one's own description
says so: "Fixing it will move snapshots, deliberately." `graph_human`,
`graph_json`, `narrative_human` and `narrative_json` all carry the
lexicographic ordering today; expect all four to need `INSTA_UPDATE=always`
plus a reviewed diff, not a red gate to chase down.

## What SH-63 leaves you

- **`domain::ready_order(a, b) -> Ordering`** and **`domain::story_number(id)
  -> u64`** are the two primitives SH-63 added, both in `src/domain.rs`
  beside `is_ready`/`has_children`. `story_number` is very likely the
  right tool for making `graph`/`handoff` numeric — it already sorts an
  unparseable id last rather than first, which is the same edge case a
  `BTreeMap`-to-`Vec` conversion in either of those two functions will need
  to handle.
- **Read `query.rs`'s module doc comment first** — it states the ordering
  contract precisely, including which two commands are still deliberately
  lexicographic and why the golden corpus is what makes that deliberate.
- **`priority_rank`/`StorySort::Priority`** (`src/store/sqlite/read.rs`,
  `src/store/types.rs`) already do this numerically at the SQL layer, if
  `graph`/`handoff` end up wanting to read through the store rather than sort
  a `Vec` in the service.

## Gate

`make test`, supervised in the background with **log growth as the
heartbeat** and a 120-second stall bound. Budget ~4–10 minutes per run. Do
**not** push with `SKIP_PREPUSH_TESTS=1`. Never bump the version or deploy
from a linked worktree — land the PR and let `main` handle both.
