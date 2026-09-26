# A story submenu was lost on diagonal pointer travel, and its spec was green only by file order

- **Date**: 2026-09-13 to 2026-09-26 UTC
- **Severity/Impact**: In the dashboard, a mouse user who moved from "Set Status" or "Set Priority" toward a submenu row that was not level with the parent lost the submenu on the way (WCAG 2.2 SC 1.4.13, "hoverable"). Travel to the last rows failed in every menu position; travel to the first rows also failed when the menu opened low on the screen. The release gate's browser leg was red on both desktop engines whenever `story-submenu-hover.spec.ts` ran without `dispatch.spec.ts` before it.
- **Status**: fixed on story SH-811 (`src/web_dashboard.html`, the spec). SH-814 tracks a scheduled isolation run, the detector for the order-dependence class.

## Summary

SH-715 (2026-09-13) opened story submenus on hover. Every `pointerenter` on a sibling row switched or closed the open submenu at once, with no pointer-aim logic. `placeMenu` clamps the menu and the submenu into the viewport, so a menu that opens low has its submenu lifted above its parent. On a fresh board the fixture card is third in todo, the menu is pinned to the viewport bottom, and step 10 of the spec's 12-step travel landed in "Set Status" (measured: y 582.7 in a row that ends at 584.5). After `dispatch.spec.ts` moves AA-1 out of todo, the card is second, the menu opens higher, and the travel is exactly horizontal. The sequential leg always ran the files in the same order, so the spec never saw the failing geometry.

## Root cause & trigger

1. **No pointer aim.** `hoverStoryMenuItem` applied every sibling hover at once. A straight path to any submenu row that is not level with the parent crosses siblings. ODC: **Algorithm / Missing / interaction**.
2. **The spec inherited its geometry.** It aimed at the submenu's first row from the parent centre. Level with the parent that path is horizontal, so it tested no crossing at all, and it never aimed at a lower row. ODC: **Test / Missing / precondition**.

The trigger was SH-792: slicing the browser leg changed which files run before each spec, and the one-file-per-slice isolation run showed this spec failing alone.

## What now guards the class

- The story menu holds a sibling hover while the pointer is inside the triangle from its last point on the parent to the submenu's near edge. Leaving the triangle applies it; progress toward the edge re-arms a 300 ms rest window; a rest applies it. The design and the rejected alternatives are decision D2 on SH-811.
- The spec pins the menu to the viewport bottom with a 400 px viewport, and each travel first asserts that its Playwright step path crosses a sibling row (inset and hit-tested). It cannot pass on a horizontal path again. Travels run on a frozen page clock, so load cannot look like a resting pointer.
- Nine travel cases (both sides, first and last rows, rest, slow travel, turning off the aim line, a submenu opened by a rest) were red on the parent on chromium and webkit. A mutation that removes the apex seed turns the last case red.
- SH-814: run the isolation proof on a schedule, so the next order-dependent spec is found before a release.

## Lessons

- A test that moves a pointer must assert the geometry it depends on. Otherwise the board state that earlier tests leave behind chooses what it tests.
- Hover-opened content needs an aim rule from the start. Opening on hover without one trades a click for a submenu that can vanish on the way to it.
- A timer in a hover rule must only apply what a resting pointer is on, and must re-arm on progress, so that a slow hand has no time limit.
