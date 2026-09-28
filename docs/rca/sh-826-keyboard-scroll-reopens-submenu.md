# Keyboard scrolling reopened a story submenu

- Story: SH-826; adopted from the independent review of SH-811.
- Found: 2026-09-28, on dev `bc1e13c2` (v3.0.3).
- Reproduction: run `story-submenu-hover.spec.ts` alone in Chromium. Hover
  Set Status, then press End in a story menu that must scroll to its last row.

## Cause

The shared menu renderer used `pointerenter` to select a hovered row.
Keyboard focus scrolled the menu 17 pixels. Chromium then sent `pointerenter`
for Set Priority at the stationary mouse position, with no `pointermove`.
That row opened its submenu and took focus from Delete. Keyboard navigation
correctly closed the prior submenu; the later boundary event reopened one.

WebKit also sent `pointermove` after the scroll, at the unchanged coordinates
(370, 567). Selecting on that event alone repaired Chromium but did not repair
WebKit. The native event trace reproduced this after the first repair.

The existing dismissal test failed both in the complete spec and alone.
An initial narrower test passed before Chromium processed the scroll. The
regression now waits for the native scroll and its following rendering frames,
then checks focus, submenu absence, and the row beneath the stationary pointer.

## Repair and prevention

The renderer selects a hover row on mouse `pointermove` with changed viewport
coordinates, once per row. The menu records the mouse position after its row
handlers, including movement over separators and out of the menu.
Keyboard input and menu leave clear that remembered row, so a real move
within the same row can resume hover. Touch and flat-menu activation stay
explicit. The existing pointer-aim logic still handles movement within a row.

The regression proves both directions: keyboard scrolling retains focus,
and a one-pixel mouse move resumes hover without first leaving the row.
Existing tests cover diagonal travel, slow travel, rest boundaries, submenu
switches, keyboard entry and dismissal, and touch activation.

Sibling review: only the story menu supplies a hover callback to
`renderMenuNode`. The column-sort and verification menus use explicit
activation. A boundary event alone must not be treated as mouse movement.
