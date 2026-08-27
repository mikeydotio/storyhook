# The dashboard's shape at phone and tablet widths

Design of record for **SH-235**. Written after implementation, the same reason
[`dashboard-dispatch.md`](dashboard-dispatch.md) gives for the same choice: sharper
against the actual measurements and the actual code than against a proposal for
either. SH-235 itself was filed as a punch list of follow-ups deferred out of
**SH-256** (mobile zoom prevention, PR #316) — that story fixed the two ways a phone
*zooms itself* (focus-zoom on sub-16px controls, double-tap-zoom via missing
`touch-action`); this one is everything else SH-256's own comment named: `100vh`,
tap-target sizing, and touch drag-and-drop.

## Context: what was actually measured

The story's ask was open-ended ("perform a series of visual tests... ensure text is
not clipped, viewports are sized appropriately, all elements are still accessible").
Before deciding fixes, the dashboard was measured in Chromium at representative phone
widths (320/375/390px) with real seeded data. Every defect below is a number, not a
guess:

| # | Defect | Measured | Consequence |
|---|---|---|---|
| D1 | `.app { height: 100vh }` + `body { overflow: hidden }` | code | iOS's URL bar makes the *true* visible area shorter than `100vh`; the app shell overflows, and its bottom (footer, board floor) sits behind the browser chrome, unreachable |
| D2 | `.list-table-wrap { overflow: hidden }` | 630px table in a 348px box at 390px wide | ~45% of every list row — Labels, Assignee, Updated — invisible *and* unscrollable. The story's own "text is clipped," verbatim |
| D3 | Tap targets under WCAG 2.2 SC 2.5.8's 24px minimum | `.label-chip button` 7.6×12, `.rel-row button` 8.1×19.5, `.column-archive-btn` 49×18.5, `.filter-clear` 65×18, `.section-toggle` 350×16.5, `.back-link`/`.rel-id` 19.5 tall, `.dispatch-history-dismiss` 17.8×20, `.filter-toggle` 18 tall — plus, found only by the later comprehensive sweep (see "As built" below), every `.btn` variant, `.projsel-btn`, `.view-toggle button`, `.fdd-btn`, and several form `select`s | Fails the WCAG floor on mouse *and* touch |
| D4 | The filter bar itself | 145px tall at 390px, on top of the topbar's own 251px on the board screen with a project open | Close to 60% of an iPhone SE's screen before a single card renders |
| D5 | `.toast-stack`/`.dispatch-history`: `position: fixed; right: 1rem` with a bare `max-width` | 22rem (352px) / 26rem (416px), no matching left margin | Runs off the left edge on any viewport narrower than max-width + 2rem |
| D6 | `.modal`/`.drafts-list`: `max-height: 90vh`/`60vh` | code | Same `vh`-vs-visible-area gap as D1 |
| D7 | No `text-size-adjust` | code | iOS's automatic post-rotation text-inflation heuristic fights this dashboard's own responsive rules |
| D8 | `.column { flex: 0 0 18rem }` = 288px | 320px viewport leaves a 32px (10%) sliver of the next column | Not enough to read as "more content this way" |
| D9 | HTML5 native drag-and-drop (`card.draggable` + `dragstart`) | never fires on touch at all | The only way to move or act on a card without a mouse was an undocumented long-press |

## Decisions taken

Four questions the measurements alone couldn't answer, resolved with Mikey before any
code changed (the plan posted to SH-235's own comments before implementation has the
full record; this is the summary that matters going forward):

1. **The filter bar collapses behind a disclosure at every viewport size** — not a
   `max-width` breakpoint. A narrow *desktop* window wraps exactly the same way a
   phone does; gating it by pointer type or screen width would mean two code paths to
   keep in sync for what is really one problem. See "The filter disclosure" below.
2. **The list view's fix is minimal: make the wrap scroll.** No columns hidden, no
   information lost — `overflow-x: auto` on `.list-table-wrap`, nothing more
   ambitious.
3. **Cards and rows get a visible actions affordance** (a "⋯" button) reusing the
   *existing* story context menu (`openStoryMenu`) rather than a second, bespoke
   touch menu. See "Touch drag-and-drop: the decision D9 asked for" below.
4. **Tap targets: 24px base, 44px on a coarse pointer**, via a `--tap-min` token —
   the same `@media (pointer: coarse)` shape SH-256 already established for
   `--control-font-*`. 24px (not a touch-only floor) because WCAG 2.2 SC 2.5.8
   applies to mouse input too.

## The design

### Dynamic viewport units (D1, D6, D7)

`.app`, `.modal`, and `.drafts-list` all carry a `vh`-then-`dvh` fallback pair —
`height: 100vh; height: 100dvh;` and the `max-height` equivalents. The `vh` line
stays first as the fallback for a browser old enough to reject `dvh` outright (CSS
drops a declaration with an unsupported unit entirely, so without the `vh` line first
that browser would get no height at all, not just the old imprecise behavior); the
`dvh` line, declared second, wins in every browser that supports it, tracking the
*actual* visible area as the URL bar shows and hides rather than the largest
possible one.

`html` also sets `-webkit-text-size-adjust: 100%; text-size-adjust: 100%;` — disabling
only the automatic post-rotation inflation heuristic, not pinch-zoom or the OS's own
accessibility text size (the same distinction SH-256 drew for rejecting
`user-scalable=no`).

Headless Blink has no dynamic toolbar to hide, and neither does headless WebKit
(SH-348) — `dvh` and `vh` compute identically under both, whichever engine is asked —
so `tests/web_test.rs` is the layer that can guard the fallback-pair *mechanism* (and
its declaration order) at all; only a real device with a real URL bar proves the
behavior itself.

### The list table (D2)

`.list-table-wrap` reads `overflow-x: auto; overflow-y: hidden` (was `overflow:
hidden`). `overflow-y` stays `hidden` on purpose — the wrap never scrolls vertically,
it's sized to its content, and that's what keeps the table's square cell corners from
poking out past the wrap's own `border-radius`. `overflow-x: auto` is the entire fix:
every column is still present, reaching a value the wrap doesn't have room for is now
a horizontal scroll rather than `display: none`.

### Tap targets (D3)

`--tap-min` (24px in `:root`, 44px under `@media (pointer: coarse)`) is read by every
selector this story's own measurement — first the hand-counted list, then the
comprehensive e2e sweep described under "As built" — found undersized. `min-width`
and/or `min-height`, plus `inline-flex` centering where a glyph-only button needed
both axes. Two spots needed `max(<original>, var(--tap-min))` instead of a bare token
reference: `.field textarea` and `.description-field` both had a taller **rest**
height than the token's own 24px desktop floor before the token existed (36px, 40px);
a bare `var(--tap-min)` would have *shrunk* either on a fine pointer instead of only
raising it where it was too short.

**`<select>` is the one exception to `min-height`, and reads an explicit `height`
instead (SH-377).** WebKit ignores `min-height` entirely on a default-appearance
(menulist) `<select>` — measured at ~20-23px on that engine regardless of
`--tap-min`'s own value, against `min-height` correctly resolving on Blink. `height`
is the one sizing property WebKit does honour there, so every select in the sheet
carries `height: max(2.5rem, var(--tap-min))` — one bare `select` selector, so a
select added anywhere in this file is covered with no further edit, rather than
repeating the rule across each of the (seven, not four) selectors that place a
`<select>` in context. `2.5rem` mirrors `.description-field`'s own rest-height
constant above, chosen because it clears every select's tallest measured natural
render (37px, this sheet's own `.modal-body select` on desktop Chromium) with
margin, giving `max()` the same "only ever raises, never compresses" guarantee this
section's other two `max()` spots rely on. `appearance: none` plus a replacement
caret was considered and rejected: this page's CSP (`src/api/http.rs`) declares no
`img-src`, so a `data:` URI SVG background-image would be silently blocked by the
browser at runtime with no test in this repo able to catch it, and a `linear-gradient`
caret would still need its own disabled/hover/focus treatment and a matching
`measureFocusIndicator` call site the moment it grew a `:focus` rule of its own. An
explicit `height` needed none of that: WebKit's native chevron, native disabled
rendering, and native focus ring are all kept, on every engine.

**What the sweep measures, and when (SH-420).** The sweep walks
`getBoundingClientRect()` — the box composited through the live ancestor
transform chain, which is what a finger is actually aimed at, and the reason
`offsetHeight` was rejected as the measure (it is transform-blind, so a target
halved by a `scale()` would report its intended size forever). Two consequences
had to be handled explicitly.

*It waits for the surface to stop moving.* The sweep used to measure
immediately after `toHaveClass(/open/)`, a few milliseconds into a 0.15s
transition: `.modal` animates `translate(-50%, -46%)` → `translate(-50%, -50%)`,
`.drawer` animates `translateX(100%)` → `translateX(0)` over 0.2s. Measured over
six openings of the create modal on `mobile-webkit`, settled, every select's box
sat at the same coordinates run after run — every coordinate on the 1/128 grid,
every height exactly 44, zero variance; mid-transition, not one coordinate was
dyadic and one iteration reproduced SH-420 outright. (SH-439 added a fourth
select, `#create-project`, ahead of the original three, which shifts every
coordinate below it — the settled *property* this measurement demonstrates,
not any one of its own numbers, is what carries forward.) An interpolated
percentage is the only thing on this surface that puts a non-dyadic offset into
a coordinate at all. `settleAndReadTapMin` therefore polls
`getAnimations({ subtree: true })` on the **swept root** — never `document`,
which would block a sweep of one surface on a toast animating elsewhere. This
reaches a class no tolerance could: `.card.entering` interpolates from
`scale(0.97)`, so a 44px target inside an entering card measures 42.71 —
**1.3px** under, forty thousand times the float32 residue below.

*It compares against the bound on its own measurement error, not a bare `<`.*
WebKit returns rect coordinates as float32 (measured: `Math.fround(r.top) ===
r.top`), and `height` is `bottom - top`. When the two endpoints land in
different binades — 468 in [256,512), 512 in [512,1024) — they round on grids a
factor of two apart and their difference misses the specified height by one
ulp. Swept across 64 consecutive sub-ulp offsets, a control specified at exactly
44px read **under** the minimum at 16 of them, over it at 16, exactly at it at
32. An element is therefore flagged only when its shortfall exceeds
`(|a| + |b|) * 2**-24`, the bound on a difference of two correctly-rounded
float32 endpoints (each within half an ulp, and `ulp(x) <= |x| * 2**-23`). At
those coordinates that is 5.8e-5 CSS px — roughly 1.8e-4 device px at dpr 3,
four orders of magnitude below one device pixel. It is a statement about the
instrument's precision, not a relaxation of the criterion. `shortBy` travels
with every report and the 1dp rounding is gone, because that rounding is what
let this gate print `height: 44` directly beneath "measure under the 44px
coarse-pointer minimum" — a message contradicting its own verdict.

Device-pixel quantization (`Math.round(v*dpr)/dpr`) was the leading alternative
and was **rejected on measurement**: it errs in both directions. At Pixel 7's
dpr of 2.625, `44 * 2.625 = 115.5` sits exactly on a rounding boundary, so an
exact-44px box's one-ulp residue rounds down and is reported as 43.81 — a false
positive manufactured by the fix; and at dpr 3 it silently passes anything in
[43.8333, 44). The threshold itself is no longer the literal 44 in the sweep: it
is read from what `--tap-min` computes to in a real coarse-pointer engine (which
`web_test.rs`'s stylesheet grep cannot establish) and pinned to 44 there, so
lowering the token fails this suite rather than quietly lowering its bar.

**What this narrows, stated rather than left to be discovered.** After settling,
the sweep asserts nothing about a target's size *while* it animates. A control
that is undersized only mid-transition is out of coverage. That is the promise
this suite should make — a user taps a settled control — but it is narrower than
what the bare walk accidentally claimed. The residual race is real rather than
hypothetical: the same measurement caught five animations already running again
on one iteration's settled read, because the dashboard polls and re-animates
cards; the failure message names it so the next reader does not re-derive it.

Decided by a three-seat council (accessibility, QA, challenger). Ballot 2-1, but
the substance was unanimous — every seat, including the author of the proposal
that lost, described this same merged design. `story show SH-420` carries the
verdict; per SH-363 no tracked file names the council's own directory, which
survives no fresh clone.

### Overlay widths (D5)

`.toast-stack`/`.dispatch-history` both read `max-width: min(<rem-ceiling>,
calc(100vw - 2rem))` — the viewport minus both the existing `right: 1rem` and its
unstated matching left margin, capped at the original rem ceiling everywhere wider.

### The filter disclosure (D4)

`#filter-panel` (the dropdowns, checkboxes, and board-sort buttons) collapses behind
`#filter-toggle-btn`, defaulting closed and persisted via `state.filtersOpen` —
the exact `sectionOpenDefault`/`localStorage` shape SH-169 already established for the
drawer's own collapsible sections, not a new mechanism. `#filter-summary`
(`#filter-toggle-btn`, `#filter-count`, `#filter-clear`) stays outside the panel's
`hidden`, always visible: a reader never has to open the panel to see whether a
filter is active or to clear one. The toggle also carries an `.active` class, driven
by the same `visible`/`total` pair `renderView()` already computes for
`#filter-count`, so "something is filtered" is visible even collapsed.

`closeAllPopovers()` previously reset every `[aria-expanded="true"]` element on any
outside click — correct for the transient popovers it exists to dismiss (a filter
dropdown, the project selector), wrong for a *persistent* disclosure like this one or
the drawer's own section toggles: an outside click that doesn't happen to trigger
that disclosure's own re-render (opening a card's drawer while the filter panel is
open, say) would silently flip its `aria-expanded` to `"false"` while the panel
stayed visibly open — a real, silent ARIA-state mismatch for assistive tech, not
merely cosmetic. The reset is now scoped with
`:not(.section-toggle):not(.filter-toggle-btn)`, fixing both the new disclosure and a
latent SH-169 sibling defect it shared the same shape with.

### Touch drag-and-drop: the decision D9 asked for

SH-256's own comment framed this precisely: "the context menu's Set Status is the
workaround today; that should be a deliberate decision, not an accident." Three
shapes were on the table:

| Option | Verdict |
|---|---|
| A visible actions button, opening the existing story context menu | **Taken** |
| Document-only: record that drag is pointer-only, leave discovery to the reader | Rejected — leaves the only touch path an undocumented long-press, exactly the accident the story exists to turn into a decision |
| Implement real touch dragging (Pointer Events, auto-scroll, drop targets) | Rejected as disproportionate to this story's scope — a second drag implementation to keep in step with the HTML5 one, its own e2e suite, ~200 lines of new state machine |

`.card-actions-btn` (board) and `.row-actions-btn` (list) both call `openStoryMenu` —
the *same* function right-click already calls, with the *same* `storyMenuModel` build
— rather than a parallel touch-only menu that could drift out of item-for-item parity
as entries are added or removed later. `responsive.mobile.spec.ts`'s own parity test
asserts this equality directly, not just that each button opens *some* menu.

Both buttons are `display: none` outside `@media (pointer: coarse)` — right-click
already reaches this exact menu on a fine pointer, so a mouse-only affordance on
every card would be pure visual noise, and desktop's rendering is byte-identical to
before this story.

**The accessibility trade-off, stated plainly.** `.card` is `div[role="button"]`; a
nested interactive element inside an ARIA `button` role is *presentational* to
assistive tech regardless of markup, so `.card-actions-btn` ships `tabIndex: -1` —
deliberately unreachable by Tab. Shift+F10 / the Menu key (already implemented for
SH-197's own context menu, already announced by `#board-kbd-hint`) is the keyboard
and AT path to this same menu, and stays that way. A `<tr>` carries no such
constraint (`buildListRow`'s own comment records why it is not `role="button"`), so
`.row-actions-btn` is a real, natively tabbable button with no override.

Restructuring `.card` so its actions button gets a genuine place in the accessibility
tree — a wrapper element, the button as a true sibling rather than a descendant of
the `role="button"` node — would touch `card.draggable`/`dragstart` (D9's own root
cause), the roving-tabindex machinery (SH-197), FLIP (`playFlip`), and
`reconcileColumnCards`'s keyed reconciliation all at once. Weighed against this
story's actual scope, that redesign was rejected as disproportionate; the trade-off
above is the accepted, documented alternative — not a silently accepted gap. A future
story that wants full keyboard reachability for the board's own action button should
start there, not by re-litigating whether the gap is real.

### The next board column peeks (D8)

Under the existing `<=768px` layout block: `.column { flex-basis: min(18rem, 85vw);
max-width: min(18rem, 85vw); }`. Only bites below ~339px (18rem / 0.85) — 375px and
wider already peek comfortably (23%+) at the original 288px and are unaffected; this
is additive at the narrowest supported phones, not a universal shrink.

## As built

### The topbar needed the same treatment the filter bar did

Not part of the original nine-defect list — found by
`responsive.mobile.spec.ts`'s own "chrome budget" test, written as part of the
filter-bar disclosure's own verification (the plan called for measuring "topbar +
collapsed filter bar" together, not the filter bar alone). Once the filter bar
collapsed, the *topbar itself* still measured 251px tall at 375px wide on the board
screen with a project open — four buttons' worth of prose text (Home, Settings,
Drafts, the connection status) and a full "STORYHOOK" wordmark, none of which shares
a row with anything else at that width.

Home/Settings/Drafts went icon-only under the same `<=768px` block (a real `<span>`
label stays for assistive tech, hidden from sighted readers via the same
visually-hidden CSS recipe `.sr-only` already uses elsewhere in this file — not
`display: none`, which would also remove it from the accessibility tree); the
wordmark and the connection status text hide the same way, since the document's own
`<title>` and the colored dot already carry that information without them.

**The chrome-budget number itself was revised, not just the topbar.** The plan's
first cut set an arbitrary 25% ceiling (167px at 375×667) before any of this was
measured. That turned out unreachable without cutting something a reader actually
needs on a phone — the search bar or the Board/List toggle, both kept at full
size/text on purpose. Four irreducible control groups (identity+project, search,
view toggle, actions) each anchored by a coarse-pointer 44px tap target is a real
floor, not a tuning parameter: three stacked rows of it is ~170px before the filter
bar's own 61px collapsed row is added. Measured, with the topbar compaction applied:
312px (disclosure alone, unfixed topbar) → 232px. The test's guard is 40% (267px) —
comfortably above the achieved 232px, well below the pre-compaction 312px, so a real
regression (a row that stops merging, a label that stops hiding) still fails it
without chasing a number the topbar's actual content can't reach.

### The comprehensive tap-target sweep found real gaps the hand-count missed

The plan's own verification section committed to proving *every* button, link and
select meets the minimum, not just the defects found by manual measurement — so
`responsive.mobile.spec.ts`'s sweep test (`button, a[href], select` across Home,
Board, List, an open Drawer, the create-story modal, Settings, and the statuses
editor, under a coarse pointer) was deliberately broader than the hand-counted D3
list, and it found real, systemic gaps the manual pass missed: `.btn` (every
primary/ghost/warn/danger button, the topbar through every modal footer),
`.projsel-btn`, `.view-toggle`, `.fdd-btn`, and every `select` in the create modal,
the drawer's field grid, and the statuses editor. Each was fixed as the sweep found
it — RED, fix, GREEN, repeat — which is what actually verified the plan's
"comprehensive" claim rather than merely asserting it.

### A genuine Chromium/mobile-emulation bug, found and fixed along the way

Adding the list table's eighth column (D9's `.row-actions-btn`, in its own trailing
`<td>`) pushed `responsive.mobile.spec.ts`'s "tap-target minimum" test into an
obscure but real Chromium behavior: `window.innerWidth` would silently balloon from
390 to 605 the instant the list view rendered a table wide enough to cross some
internal viewport-fit threshold — even though `.list-table-wrap`'s own `overflow-x:
auto` properly contained the table's *rendered* box the entire time
(`document.documentElement.clientWidth` stayed correctly at 390 throughout).
Confirmed by a minimal, bisected repro run against both the pre- and post-D9
dashboard: the divergence appeared at exactly "switch to list view," and only with
the wider (8-column) table — a 7-column table never triggered it.

`contain: layout` on `.list-table-wrap` fixes it, isolating the wrap's internal
layout from whatever outer measurement Chromium's mobile viewport-fit heuristic was
keying off of. Verified both that the divergence is gone with the fix in place and
that the table's own `scrollWidth` (and its horizontal-scroll behavior) is
unaffected. Recorded here because it cost real investigation time and the next
person hitting `window.innerWidth` disagreeing with `document.documentElement
.clientWidth` under `mobile-chromium` device emulation should not have to re-derive
this from scratch. Blink-specific, as far as this suite can tell: `mobile-webkit`
(SH-348) runs the identical eight-column list-view sweep and passes clean, so
whatever internal viewport-fit heuristic caused the divergence is not one WebKit
shares -- the `contain: layout` fix stays in place regardless, since it is harmless
where the bug it targets doesn't exist.

### An icon is a shape the page draws, never a character (SH-444)

Found independently of SH-235, on the same controls this section names: the topbar's
Home/Settings/Drafts icons (and, once swept, the board's column-sort control, the
card/list-row actions menus, and the Settings-statuses back link) were single Unicode
characters, rendered through whatever fallback font the platform picked for a
codepoint `--sans` doesn't cover. U+2302 HOUSE and U+270E LOWER RIGHT PENCIL are not
emoji at all; U+2699 GEAR is emoji-capable but shipped *unqualified* (no U+FE0F), so
its text-vs-colour presentation was undetermined per platform on top of that. All
three rendered at an arbitrary weight unrelated to the 600-weight text beside them —
exactly what the reported screenshot showed.

**The rule going forward:** every control icon in this file is an inline `<svg
class="icon" stroke="currentColor">`, reusing the pattern the search box's icon
already used (`.search-wrap svg`). `currentColor` inherits the button's own colour,
so the icon themes and hovers with the rest of the control for free, across all four
theme resolutions this file supports. The three topbar icons are static markup; a
JS-constructed control builds one through the `svgIcon()` helper beside `el()` (SVG
needs `createElementNS`, which `el()`'s own `document.createElement` can't provide).
`tests/dashboard_icon_glyphs.rs::every_btn_icon_span_holds_a_shape` fences the
topbar's own `.btn-icon` class; the other four controls are covered behaviorally, by
`e2e/specs/icon-shapes.spec.ts` and `icon-shapes.mobile.spec.ts`.

**The boundary against typographic marks was narrowed by SH-447.** Sort/reorder
arrows, checks, bullets and close marks (`▲ ▼ ↑ ↓ ● ✓ ×`) stay characters: they are
covered by UI fonts, have deterministic text presentation, and several remain pinned
as exact text by existing e2e contracts. A disclosure indicator is different even
though its presentation is also deterministic. The reported Filters and dropdown
triangles sat in 9–10px font boxes and their visible ink occupied only a fraction of
that box, making every adjacent hidden-content affordance look undersized. All controls
whose indicator means “reveals hidden content” therefore use one 14px inline SVG
chevron: project and filter dropdowns, the Filters and drawer-section disclosures, and
context-menu submenus. `data-direction=right|down` names their visual state without
making the decorative SVG part of the accessible name; the owning button's existing
`aria-expanded` remains authoritative where the control is persistent.

`tests/dashboard_icon_glyphs.rs::no_raw_disclosure_triangle_is_left` fences the source
against restoring U+25B8/U+25BE through either static markup or JS construction.
`filter-bar-disclosure.spec.ts` and `icon-shapes.spec.ts` verify the live SVG geometry,
direction, inherited colour, accessible name and ARIA state. Native `<select>` carets
remain browser-owned for the cross-engine reasons in “Tap targets (D3)” above.

**The same undetermined-presentation defect, generalized:** any pictographic
character anywhere in this file — not just an icon control — must carry a trailing
U+FE0F or it doesn't belong here at all. `tests/dashboard_icon_glyphs.rs::
no_pictographic_character_is_left_unqualified` fences the whole file for exactly this,
with U+2713 CHECK MARK as the one documented, deliberate exception (not an emoji,
universal font coverage, pinned e2e text). It caught a second, independent instance of
the same defect during this same investigation: the archived flag/banner's U+1F5C4
FILE CABINET shipped with no U+FE0F, unlike this file's other emoji (U+1F3F7 LABEL,
`typeGlyph()`'s fallback), which was already correctly qualified — the convention
already existed and simply wasn't applied everywhere. Fixed by qualifying it
(`🗄` → `🗄️`) rather than converting it to a shape: it sits inline inside prose, not
a standalone icon control.

## What guards each defect

Every behavioral test below runs under both `mobile-chromium` (Blink) and, since SH-348,
`mobile-webkit` (WebKit) — both mobile emulation, neither a real device — except where a
row says otherwise.

| Defect | Structural test (`tests/web_test.rs`) | Behavioral test (`e2e/specs/`) |
|---|---|---|
| D1, D6, D7 (viewport units) | `web_serve_root_html_sizes_the_shell_to_the_dynamic_viewport` | `responsive.mobile.spec.ts`: "the app shell and an open modal fit inside a squeezed viewport height" |
| D2 (list table) | (covered by the CSS itself; no literal-value regression to pin beyond D3's coverage of the wrap) | `responsive.mobile.spec.ts`: "the list table scrolls sideways to its far columns instead of clipping them" |
| D3 (tap targets) | `web_serve_root_html_meets_wcag_tap_target_size` | `responsive.mobile.spec.ts`: "every button and link meets the coarse-pointer tap-target minimum", "every select meets the coarse-pointer tap-target minimum" (both `mobile-chromium`/`mobile-webkit`, since SH-377) |
| D4 (filter disclosure) | `web_serve_root_html_has_a_collapsible_filter_panel` | `filter-bar-disclosure.spec.ts` (desktop — not a mobile-only behavior) |
| D5 (overlay widths) | `web_serve_root_html_clamps_overlay_widths_to_the_viewport` | `responsive.mobile.spec.ts`: "toast and dispatch-history overlays never exceed a narrow viewport" |
| D8 (column peek) | `web_serve_root_html_lets_the_next_board_column_peek_on_narrow_phones` | `responsive.mobile.spec.ts`: "the next board column peeks on the narrowest supported phone" (plus its own "stays at 18rem" companion) |
| D9 (actions menu) | `web_serve_root_html_has_coarse_pointer_actions_buttons` | `responsive.mobile.spec.ts`: "the card and list-row actions menus have the same items as right-click", "...is deliberately not a Tab stop..." |
| Chrome budget (topbar + filter bar) | — | `responsive.mobile.spec.ts`: "the topbar and collapsed filter bar together stay within a measured chrome budget" |

## Verification this design can't cover

Every mobile spec now runs under both `mobile-chromium` (Blink) and `mobile-webkit`
(WebKit) mobile *emulation* (SH-348) — so iOS Safari's own 16px zoom-avoidance threshold
is, for the first time, asserted against the engine whose rule it actually is, not only
simulated on Blink. That closed the engine gap this section used to describe. It did not
close the *emulation* gap: `devices["iPhone 15"]` under either engine is still a headless
browser told to pretend it's a phone, not a real one, and three things stay true no matter
which engine drives it. It has no dynamic toolbar to hide, so the `dvh` fallback pair's
*mechanism* (declaration order, that both lines are present) is pinned on both engines but
its *behavior* — the visible area actually shrinking and growing as a real URL bar shows
and hides — is not. `hasTouch: true` sets a capability flag; it is not a finger, so nothing
here proves a real tap lands where a click did. And WebKit-under-emulation surfaced one
genuine engine-specific defect of its own rather than closing every remaining question:
WebKit ignores `min-height` on a default-appearance `<select>`, so every select in the
dashboard measured ~23px against the intended 44px on that engine (SH-377) — the emulated
environment found a real gap the design didn't anticipate, which is itself evidence that
emulation and reality still diverge. Fixed by giving every `select` an explicit `height`
instead of relying on `min-height` a second time (`min-height` is what every other
tap-target selector in this file still reads, and stays correct there — only a menulist
`<select>` ignores it); see "Tap targets (D3)" above for the full rule and why `height`
was chosen over `appearance: none` plus a replacement caret.

A real device pass is what SH-256 already flagged as needed and SH-235 inherited, and
SH-348 does not change that: on an iPhone, over Tailscale, against the real daemon — the
footer reachable with the URL bar shown, the filter disclosure's state surviving a reload,
the actions menu opening on a genuine tap, the list table's horizontal scroll working under
a real finger, and confirmation that iOS Safari actually stays unzoomed rather than merely
receiving CSS that computes to the right numbers under emulation.
