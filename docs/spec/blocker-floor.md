# The blocker floor: a blocker sorts at the level of the work it holds up

Design of record for **SH-788**: "promote the priority of any lower-priority
story that blocks a higher-priority story, for the duration of the blockage",
shown in the CLI as `high (critical)` and on the web board by sorting in the
higher level's position with a two-colour barber-pole accent.

## The problem

Priority is `story next`'s sort key. Until this story, `blocks` / `blocked-by`
carried nothing into it. `story next` already placed a blocker before its own
dependent (the SH-450 execution queue), but the blocker competed for its turn
on its *own* level: a low story that a critical one waited on sorted behind
every unrelated medium and high story, and the critical work waited with it.

The priority rubric knew this and told a person to fix it by hand — the
"blocker floor": raise the blocker's *stored* level to its dependent's. Nothing
applied that rule, and a stored raise has two costs the rubric itself names:
it outlives the blockage, and it saturates the level ("every prerequisite of a
critical becomes critical"). `tests/service_query.rs` (SH-450) and
`tests/engine_graph_progress.rs` (SH-609) pinned the unfloored order.

## The rule

The floor is **derived, never stored** — no event, no store column, no
snapshot field. It is recomputed from the live graph on every read, so it ends
by itself when the blockage ends.

| Term | Meaning |
|---|---|
| Node | An OPEN story. Closed stories neither lend nor receive a level. |
| Blocking edge | U → B when B is in U's `blocked-by` list and B is OPEN — the side `domain::is_blocked` reads. No other relation carries a level. |
| Source | A node that lends its own level: OPEN, not a draft, no `obviated-by` edge. |
| Relay | Drafts and obviation-flagged stories genuinely block work, so they pass on a level they receive; they lend none of their own. |
| Epic relay | An epic closes only when its children close, so a *blocking* epic hands every level it receives to each open child, at every depth. An epic's own level never reaches its children. |
| Floor | The most urgent level that reaches a story. Reported only when strictly more urgent than the story's own level. |
| Effective priority | The floor where one raises the story, else its own level. |

Consequences: an equal level raises nothing; nothing is ever lowered; the
floor never exceeds the most urgent dependent, so the rubric's inflation error
cannot occur; a chain carries the level to its root; a cycle settles on its
most urgent member.

### Algorithm

`domain::BlockerFloors::compute` (`src/domain/blocker_floor.rs`) is a worklist
fixed point. Every source is queued once for its own level; a story is queued
again only when the level it has received becomes more urgent. With five
levels no story improves more than four times, so the cost is linear in
stories plus edges and a cycle terminates. A property test checks it against a
plain reachability reference.

### One story set per ranking

`domain::ReadyRanking` (`src/domain/ready_ranking.rs`) pairs a `StoryIndex`
with the floors computed from that same index, and `domain::ready_order` takes
the ranking rather than a bare index. No caller can rank with lookups from one
story set and floors from another — the invalid pairing the TUI's `Readiness`
type already rules out for readiness. `StoryIndex` gained `stories()` for the
walk.

## Where each level is used

| Uses the **effective** level (scheduling) | Keeps the **stored** level (classification) |
|---|---|
| `story next`, `story claim --next`, the Full Auto engine (`execution_queue`) | `story list --priority` |
| `ready_order`: context/summary ready lists, the session "Next:" line, the TUI Ready panel | `story summary` / `report` counts by priority |
| The parent-epic tie-break (the epic's effective level) and a parentless story's own second key | `story export`, the snapshot, `ProjectSnapshotView` |
| The verifier queue (`VerificationCandidate.priority`) and its "higher effective priority" comment | The dashboard's priority filter |
| The dashboard board Priority sort and List priority column; the HTML report table | The TUI graph's colours |

A parentless story's second key must be its *effective* level: its stored
level there would sort a floored story after every other story at its floor.

## How it is shown

The floor is always the **last** parenthetical, after the stored level:

| Surface | Shape |
|---|---|
| `list`, `search` lines | `SH-1 [todo] (low (critical)) [normal] Title` |
| `show`, `next`, `claim`, mutation echoes | `priority: low (critical)` |
| Legacy unassessed story | `priority: none (not assessed) (low)` |
| Summary / load-context ready lists, session `Next:` | `(low (critical))` |
| HTML report | badge `low (critical)` in the floor's colour |
| JSON | view field `blocker_floor`, absent unless it raises the story; `load-context --format json` rows likewise |
| TUI | stored glyph then floor glyph, `.(!!!)`; words in the detail view |
| Plugin ready picker | `[low (critical)]`, and `blocker_floor` on each row |
| Web card | the 3px accent becomes a static diagonal stripe of the two colours |
| Web List | split dot and `low (critical)` |
| Web drawer | "Sorts as critical while it blocks more urgent work" beside the stored-level select |

The card stripe is a background layer under a transparent left border,
written in longhands (the card's own `background:` shorthand would otherwise
reset its colour), clipped by the card radius, the same 3px wide, and static.
The pattern is the non-colour cue; the card's base aria-label says it in words
("priority low, sorted as critical while it blocks more urgent work"), and
`forced-colors` falls back to a solid stripe. The class and colours are set
before `populateCard`'s SH-399 rebuild guard, whose fingerprint covers only
the card's children.

`blockerFloor(v)` in the dashboard shows a floor only while it is strictly
more urgent than the stored level: `applyStory` replaces a view's `.story`
alone after a mutation, and the two can disagree until the next `/data` reply.

Two views stay bare by design: the import/decompose echo (no project-wide
read, like its `display_state`) and any view from an older daemon. Both read
as "no floor". `search` is bare otherwise but carries the floor, because it
prints the priority and computes the floors from the story map it already
reads.

## Decisions

Recorded in full, with context and rationale, as comments on SH-788:

- **D1** transitive, not direct-only — direct-only leaves the chain's root
  behind medium work, the stall the floor exists to prevent.
- **D2** drafts and obviation-flagged stories relay but do not lend.
- **D3** a blocking epic relays to its open children; `parent-of` still
  carries no epic's own level.
- **D4** stored level for classification, effective level for scheduling and
  display (table above).
- **D5** the epic tie-break and a parentless story's second key read the
  effective level.
- **D6** the verifier queue orders by the effective level.
- **D7** the detection carve-out now governs the stored level only; a detector
  that blocks its critical defect sorts at critical. This overturns the
  project's earlier "carve-out wins" precedent: the story says "any", and the
  parenthetical keeps the carve-out's severity statement visible.
- **D8** JSON `blocker_floor`, present only while it raises the story.
- **D9** the SH-450 and SH-609 pinned orders change; Full Auto dispatch order
  changes with them.
- **D10–D14** search carries the floor; import echoes do not; the floor is the
  last parenthetical; the verifier comment says "effective priority"; the TUI
  shows `own(floor)` glyphs.

## Tests

| Claim | Test |
|---|---|
| The rule, case by case, and against a reachability reference | `src/domain/blocker_floor.rs` unit and property tests |
| `ready_order` ranks by the effective level; parentless and epic tie-breaks | `src/domain.rs` `ready_order_*` tests |
| `story next` / engine / verifier / session / TUI order | `tests/service_query.rs`, `tests/engine_graph_progress.rs`, `tests/verification_queue.rs`, `tests/service_session.rs`, `src/tui/components/dashboard.rs` |
| Every printed surface, JSON shape, stored-level filters and counts | `tests/story_priority.rs`, `tests/story_report.rs`, `src/output.rs` `priority_label_tests`, TUI render tests, `plugins/story/tests/test-list.sh` |
| The board, List and drawer in a browser | `e2e/specs/priority-floor.spec.ts`, with static guards in `tests/web_test.rs` |
