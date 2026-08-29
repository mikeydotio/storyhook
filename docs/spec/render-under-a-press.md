# Rendering under a press

**Status: as built (SH-401).** Design of record for the dashboard's press gate.
Settled by a unanimous three-seat council; `story show SH-401` carries the verdict
and the alternatives it refuted. (Never cite the council's own directory — it does
not survive worktree teardown, SH-363.)

## The defect

Per the UI Events click-dispatch algorithm, if the element under `mousedown` is
disconnected from the document before `mouseup`, **no `click` is dispatched
anywhere** — not even at an ancestor — because the two targets no longer share a
common inclusive ancestor. `src/web_dashboard.html` can replace rendered subtrees
from asynchronous sources: a `/data` reply whose diff reports a change,
`handleMutationSuccess`, the drawer's own detail fetch, or the dispatch poll. The
drawer now retains output-identical top-level sections (SH-423), but a genuinely
changed section can still be replaced. One such paint landing mid-press destroys
the pressed node, and the user's action is silently swallowed — no error, nothing
to retry against.

It surfaced from the browser suite rather than a bug report for a reason worth
keeping: **Playwright reports the gesture as successful.** Its hit-target check
runs once, before the gesture, against the node that existed then.

SH-397 closed the first board occurrence by pointing the pointer at a node that
survives an unchanged-position reconcile — `.card` itself — and making its
presentational descendants `pointer-events: none`. SH-422 identified the boundary:
a real order change makes `reconcileColumnCards` disconnect and reinsert the whole
card. The press gate closes that case because `renderView` is gated, and
`card-reposition-click-race.spec.ts` is the exact witness. **SH-397's shape cannot
be extended to the drawer**: the button *is* the destroyed node, and delegating to
a surviving ancestor changes nothing, because no `click` is dispatched at all.

## The gate

While a press is in flight the renderers below defer their **paint**, and flush once
the press can no longer produce a `click`. Only the paint waits: `state.data` is
still replaced on arrival, `boardFetchFloor` still rises, `applyStory` still runs.
No handler can read stale data through this gate — it can only see stale pixels, for
the length of a press.

**Arm.** Capture-phase `pointerdown` on `document`, only when `e.isPrimary &&
e.button === 0`. Capture so no `stopPropagation()` can hide a press. Primary-button
only because `click` is a primary-button event: a secondary press has no click to
protect, which is why the context menu needs no carve-out — it is never gated.

**Gated.** `renderAll`, `renderView`, `renderDrawer`, plus the description editor's
layout-changing `exitEdit` paint. Each defers a *thunk*, keyed by surface, last
writer wins. The render thunks re-read live state, so a flush always paints the
newest world and coalescing several deferred renders into one pass is sound. The
description thunk carries only the text already committed by its blur; the PATCH
itself never waits. SH-423 added that fourth surface after a deterministic WebKit
witness proved a still-connected Comments button could nevertheless move out from
under mouseup when the editor collapsed during a held press.
`renderAll` and `renderView` are deliberately **both** gated even though either
alone covers the `fetchData` path (`renderAll`'s body is the sub-renderers): they
are independently load-bearing on different paths — `renderView` for the direct
`setTimeout(renderView, …)` the blocker-cleared dwell schedules, `renderAll` for the
`state.data`-dependent surfaces beside the view. Measured, not assumed: removing
either one alone leaves the witness specs green; removing both turns them red.

**Not gated, deliberately.** `updateConnection`/`markDataSettled` (attribute and
text writes only, so the connection indicator stays honest during a press),
`openStoryMenu` (appends, destroys nothing under the pointer), `toast` (additive).
`renderDrawerFooter` is not gated *separately*: its only asynchronous door is
`renderDrawer`, which is gated, and a second gate whose flush had to re-derive its
`st` argument would be a second way of saying one state fact — the thing SH-302's
note in this file already warns against. The footer is covered, and it has its own
witness spec proving it.

**Release, first evidence wins.**

| Trigger | Why |
|---|---|
| bubbling `click` at `document` | Provably after the pressed control's own listener. **Opportunistic only** — this file calls `stopPropagation()` on `click` at ten sites, three of them controls this fix exists to close (`.card-actions-btn`, `.rel-id`, `.row-actions-btn`), so a document listener never hears them. |
| `dragstart` | The UA dispatches no `click` for a gesture promoted to a drag, so there is nothing left to protect. This is what makes native HTML5 drag-and-drop a non-problem rather than a carve-out, without depending on whether `pointerup`, `pointercancel` or `dragend` arrive. |
| `pointercancel` | Gesture taken over. |
| two `requestAnimationFrame`s after `pointerup` | The load-bearing fallback, for the `stopPropagation()` sites above and for genuinely click-less presses. |
| `pointermove` with `e.buttons === 0`, `blur`, `visibilitychange` | A release delivered to another application. `visibilitychange` is load-bearing rather than defensive: `requestAnimationFrame` does not fire in a hidden tab, so a tab backgrounded between `pointerup` and the frame release would otherwise strand the drain. |

**Never released inside the `pointerup` handler.** `pointerup` precedes `mouseup`
precedes `click`, all in one input task, so a teardown from any handler in that task
destroys the click by the identical mechanism the gate exists to prevent. This is the
most review-invisible way to write the gate wrong — one line, and the truth table is
otherwise identical — so it is the mutation the tests check in both directions, and
the ordering itself is **measured** by a contract spec rather than argued from the
spec text (`interception-contract.spec.ts` is this suite's precedent for measuring a
plausible claim instead of believing it).

## The failsafe, and why it is not a ceiling

`PRESS_GATE_FAILSAFE_MS` is **not** a bound on how long a human may hold a button.
That was rejected: a wall clock is the only release in the set that can fire during a
**live** press, and firing there re-opens the exact defect the gate closes, for
exactly that press.

It is a bound on the **release set**. Under a correct release set it is unreachable,
which makes its firing a *detector for a hole* rather than a recovery from one — so
it is reported, never silent (SH-306), and derived from `SAFETY_POLL_INTERVAL_MS`
rather than picked (SH-394): a gate may not hold a paint back longer than the interval
at which the board resyncs anyway. `tests/press_gate_failsafe.rs` pins both properties
and carries a positive control.

Staleness is reported as a **count of `/data` arrivals** a hold spanned, not a
duration, so no second wall clock enters the product.

## `pendingAnimations` is unioned, not overwritten

Gating `renderView` would otherwise lose an animation batch: `pendingAnimations` is
consumed-and-nulled there, so a second reply arriving during one press would overwrite
the first's. Unioning is **provably** harmless rather than plausibly so — every
consumer was traced. `exited` and `moved` reach no renderer at all (read only by
`fetchData`'s own `hasChanges`); `entered` is read only inside the branch for a node
that did not already exist, so a stale entry is structurally ignored; `changed` is
keyed by id, so the newer value wins. Worst case is one extra decorative flash.
Outside a press `pendingAnimations` is always null at that point, so it is a no-op on
every ordinary path.

## What watches it

`window.__storyhookPressGate.swallows` records any removal that disconnects the
**live** press target — a swallowed click in waiting, whatever function removed the
node: `clear()`, a bare `.remove()`, or code written next year. It is observed rather
than derived from a list of call sites, and its limit is the mirror image of a static
scan's, stated rather than glossed: **complete over removal mechanisms, incomplete
over surfaces**, since it only sees what a run actually exercises. The static half —
funnelling the file's bare removal sites through one `detach()` door so a scan can be
derived over idioms — is **SH-421**, severed from this story by the council's own
unanimous motion.

`window.__storyhookPressGate.deferred` lists each surface currently held by the gate
and is cleared before release flushes them. It is both a narrow diagnostic and a
deterministic browser-test synchronization point: the SH-423 witness waits for the
real PATCH reconciliation to defer `drawer` before releasing the mouse, rather than
guessing at browser timing with a sleep.

## Coverage

`e2e/specs/drawer-body-click-race.spec.ts`, all verified red on the pre-SH-401 tree
and green after: a drawer `.section-toggle`, a drawer-footer button, a list row
(`populateListRow`'s `clear(row)` — a surface nothing had filed, and one with no
`pointer-events` rule anywhere in the sheet, so every cell is a live press target),
the paired no-interposition control, and the ordering contract above. SH-423's
later section/footer reconciliation means an output-identical drawer target now
survives that particular reply even without the gate; genuinely changed sections
remain protected by the gate.

`e2e/specs/description-edit-mode.spec.ts` carries SH-423's exact two-part witness:
the real description PATCH response is held until its write lands, delivered
between mouse-down and mouse-up on Comments, and must both retain the target and
complete the save-plus-toggle gesture in Chromium and WebKit. Its paired identity
test proves a title-only mutation retains the output-identical Comments and footer
nodes, while the retained Close handler reads the current title.

`e2e/specs/card-reposition-click-race.spec.ts` adds SH-422's exact whole-card
witness: old order `[A, B]`, held `/data` reply producing desired order `[B]`, and
a real press on B spanning delivery. It proves the old paint remains during the
press, B's drawer opens, and only then does the deferred render move A away.

Not covered, stated rather than implied: `.card-actions-btn` and `.rel-id` — SH-397's
own two declared residues — are covered *by construction* (they sit under
`populateCard`, reached through the gated `renderView`) but have no witness spec of
their own here, because `.card-actions-btn` is `display: none` on a fine pointer
(SH-235) and so is unpressable in the desktop projects where these specs run.
