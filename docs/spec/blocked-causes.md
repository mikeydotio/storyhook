# Blocked causes: an edge that clears itself versus prose that doesn't

Design of record for **SH-398**, filed against two defects on the same root cause,
both surfaced by an autonomous session blocking SH-394 *on* SH-397 with nothing but
prose: the dashboard drawer's blocked banner rendered a long reason as a run of
narrow, unreadable columns, and — the deeper defect — nothing recorded SH-397 as
the blocker at all, so nothing would have cleared when it closed.

## Three signals, one derived predicate

`is_ready` (`src/domain.rs`) treats a story as blocked for any of three independent
reasons:

1. The reserved `blocked` **state** (SH-125/SH-126).
2. An open `blocked-by` (or unconditional `obviated-by`) **relationship** —
   `src/service/relation.rs`.
3. A free-text `awaiting` **reason** — `StoryService::set_awaiting`, historically the
   only thing `story block <id> "<reason>"` could write.

Only the relationship is a fact the store can act on: it clears itself the instant
the blocker's superstate goes `CLOSED` (`is_ready`'s own `blocked-by` clause re-tests
that every time), it is visible from both stories, and `story doctor` can audit it.
A prose reason is inert — nothing watches whether the story it names has closed, so
it survives exactly as long as somebody remembers to run `story unblock`.

Before this story, `story block` could only write the third signal. A blocker that
was itself a story — the common case, and the one on SH-394 — had no way to become
the second, so it stayed prose, and prose is precisely the class of "silent,
cross-story" defect this project has paid for before (see CLAUDE.md's own precedent
list on SH-136, SH-198, SH-258).

## What changed

### `story block <id> [--on <blocker>]... ["<reason>"]`

`--on` is repeatable and records a `blocked-by` edge onto each named blocker. A
reason is required only when no `--on` was given — matching the pre-SH-398 contract
for every caller that never learns the new flag (REST's `route_block_story`, the
TUI's `Action::SetAwaiting`, and MCP's `story_block`, none of which changed). Both
may be given together: the edges and the reason commit in **one transaction**
(`RelationService::block_on`, a generalisation of `RelationService::relate` from one
target to N — each blocker takes one append for its own inverse edge, and the
subject takes exactly one append carrying every new edge plus the optional
`StoryAwaitingSet`). The alternative — a loop of `relate` calls followed by a
separate `set_awaiting` — is the exact half-write hazard `relation.rs`'s own module
doc already names (SH-60): "blocked by A and B" landing with only A recorded, or
edges landing without the reason that explained them.

`story unblock <id> [--on <blocker>]...` is the inverse. No `--on` clears the prose
reason (unchanged) — and, new, warns if an open `blocked-by`/`obviated-by` edge is
still there, since reporting bare success while the story stays blocked is the
SH-312 "comforting falsehood" shape. One or more `--on` removes just those edges and
leaves the reason untouched, so a story can be unblocked from one dependency while
still waiting on another.

### The nudge

`src/block_notice.rs` warns — never refuses, since `story block` runs
non-interactively from agents and the daemon — when a written `awaiting` reason
names a story id that resolves in the project, is still `OPEN`, isn't the subject,
and has no `blocked-by` edge from the subject. Wired at every dispatch arm that can
set `awaiting` (`SetAwaiting`, `SetState` via `story move <id> blocked --reason`,
`SetFields` via `story set --blocked`/`--json`), fenced by
`tests/block_notice_paths.rs` deriving that door list from an exhaustive match
rather than a hand-kept one — the shape this project has been burned by before.

### The detection layer

The nudge only fires at authoring time. `story doctor` gained a sibling notice
(`unlinked_blocker_notices`, beside the existing `blocked_without_reason_notices`)
that sweeps the whole project for the same condition, so a reason typed before this
story existed — or edited by hand — still surfaces.

### The dashboard

`blockCauses(st)` is now the one place that reads `st.relationships` for
`blocked-by`/`obviated-by` edges; both `blockedFlag()` (the card badge, unchanged in
its own rendering) and the new `blockBanner()` (the drawer banner) derive from it,
so the two surfaces cannot silently disagree about which edges block.

The banner used to be gated on `st.awaiting` alone — a story blocked purely by an
open `blocked-by` edge got no banner at all, and was shown the "Reason for
blocking…" form as though nothing were wrong, while its own card read `● blocked
(SH-397)`. It now renders whenever `blockCauses` finds anything, with the blocker
and obviator lists uncapped (the drawer has the width the card's badge does not, so
nothing folds into a `+N`). The block form still renders whenever there is no
`awaiting` reason yet, alongside the banner rather than instead of it — a
relation-only-blocked story keeps the ability to add a note.

The banner's layout is `.banner-head` (a wrapping flex row: headline, blocker/
obviator chips, the Unblock button) above `.banner-body.md` (ordinary block flow,
rendered through the same `renderMarkdown()` descriptions and comments already use).
Before this, `.banner` itself was the flex row, and `linkifyStoryIds()`'s own mixed
array of text nodes and `storyRef()` elements became one anonymous flex item per
fragment — a long reason rendered as a run of narrow columns rather than wrapped
prose. `e2e/specs/blocked-banner-layout.spec.ts` pins the fix by geometry (bounding
boxes), since `toContainText` cannot distinguish the two renders — both contain
exactly the same characters.

## Deliberately out of scope

Two surfaces carry the identical pre-SH-309 blindness this story's dashboard fix
closes for the card and drawer — reason derived from `awaiting` alone while
membership is the full `is_ready` predicate — and are named here rather than
silently left, per this project's own scope-rubric:

- The TUI board's `BLK` badge (`src/tui/components/board.rs`).
- `story context`'s `## Blocked` section (`src/service/query.rs`).
- `plugin/claude-code/bin/story.sh`'s `ready_gate_reason()`, a jq reimplementation of
  `is_ready` that has already drifted from it (missing the `draft` and
  `state == "blocked"` clauses).

Each is a different rendering surface from the one this story's screenshots
reported, and each is large enough (a jq rewrite; a TUI rendering pass) to be its
own story rather than riding along.
