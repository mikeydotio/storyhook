# Mobile visual tune-up

Design of record for the **SH-610 audit**, approved on 2026-09-08 against
**v2.4.2**, checkout `8189f1c24`. Implementation belongs to epic **SH-612**.
This document specifies future behavior; SH-610 changes no dashboard code.

## 1. Evidence and intent

The dashboard should make finding, reading, and acting on stories the primary
mobile experience. Project identity and operational status stay visible; secondary
navigation and filter editing should not permanently consume the reading area.
Desktop should share the visual language and behavior, with more simultaneous
information where there is room.

| Source | What it establishes | Limits |
|---|---|---|
| [Toolbar screenshot](https://mw-dropshare.s3.amazonaws.com/1788889425.jpg) | Project/view row, separate navigation row, search row, then a summary and three rows of expanded filters. Run and New compete for attention. | Cropped image, 1320×1176 image pixels. Device scale and full usable viewport are unknown; do not convert this into a measured viewport percentage. |
| [List screenshot](https://mw-dropshare.s3.amazonaws.com/1788889710.jpg) | The first title breaks into fragments such as “Attac” / “hmen” / “ts:”. Its row dominates the crop while Order contains only a dash. Far metadata is offscreen. | Cropped image, 1320×1295 image pixels; not a current browser measurement. |
| [Dashboard source](../../src/web_dashboard.html) | At the audit baseline: topbar at line 248, filter layout at 494, list at 820, drawer at 907, mobile rules at 1476. `populateListRow` and `syncFilterToggle` establish current data and interaction behavior. | Static inspection explains likely mechanisms; it does not certify rendered contrast, accessibility, or device behavior. |
| [Existing responsive specification](responsive-dashboard.md) | SH-235 previously measured a 251px topbar plus a 145px expanded filter bar at 390px. Its later collapsed layout measured 232px. | Historical measurements, not measurements of this audit's baseline. |
| [Responsive browser tests](../../e2e/specs/responsive.mobile.spec.ts) | Tests currently cap collapsed chrome at 40% of a 375×667 viewport and verify the table can scroll to Updated. | Neither assertion establishes expanded-panel geometry or readable title width. |

No fresh browser/device measurements were taken for this documentation change.
The acceptance budgets below are **proposed product requirements**, not achieved
results. Each implementation child must reproduce its defect and capture its own
before/after evidence using the isolated production dashboard harness.

## 2. Visual critique

The restrained palette, consistent outlined controls, native-feeling typography,
state pills, and explicit Board/List switch already form a coherent utility UI.
Preserve them. The failure is hierarchy and allocation of space: administrative
controls occupy the strongest positions, while the story title gets the remainder.

| Impact / owner | Surface and finding | Recommendation |
|---|---|---|
| Medium / SH-613 | **Navigation:** the screenshot gives secondary Home/Settings/Drafts their own row. The source wraps the topbar and makes search a full-width third item. Icon-only navigation saves labels but still pays for every touch target. | Move secondary navigation into More; keep project selection and New directly available. Use an intentional three-row shell that includes the current filter-summary responsibilities. |
| Medium / SH-613 | **Search and filters:** search has useful scope copy, but filter editing permanently expands the shell. The flat collection mixes story predicates with board display controls. | Keep search visible; use a transient mobile filter sheet with two labeled groups and a persistent result count/active indicator. |
| Medium / SH-613 | **Automation:** Run is mounted inside `.filter-summary`, although it acts on project work rather than changing what is visible. Its filled treatment competes with New. | Put automation in its own header group with a descriptive accessible name. Keep urgent states explicit and the existing start/stop confirmations intact. |
| Medium / SH-614 | **List:** metadata uses `white-space: nowrap`; the title alone permits `overflow-wrap: anywhere` without a width floor. The screenshot makes prose a narrow vertical strip. Horizontal reachability alone is insufficient. | Give phone titles their own full-width line. Put identity/status beneath; disclose secondary metadata. Protect the desktop title column too. |
| Low / SH-615 | **Board:** cards have useful state/priority signals, visible actions, and a deliberate next-column peek. Badges, pills, shadows, and small labels can compete with the title when a card is busy. | Make title, operational exception, and metadata the reading order. Keep next-column peek and actions. Align spacing and secondary type; retain semantic colors and complete messages. SH-611 owns verification-text containment. |
| Low / SH-615 | **Detail drawer:** the peer panel preserves surrounding context and has a scrollable body. At narrow widths, repeated 20px padding and 20px section gaps spend substantial space; tiny uppercase labels weaken the hierarchy between labels and values. | Use the shared mobile inset and section rhythm; sentence-case functional labels. Preserve peer-panel behavior and its focus/Back contracts. |
| Low / SH-615 | **Forms:** single-column mobile grids and input font/tap tokens are strengths. Functional labels currently use `.field label` / `.section-label` at 11px with `--fg-faint`, creating a readability and contrast risk. | Increase functional labels to the specified metadata size and use a contrast-validated text role. Keep native controls, explicit select heights, error associations, and unsaved edits. |
| Low / SH-615 | **Home and Settings:** mobile grids/forms already stack. Project paths and registry metadata deserve less emphasis than project names and actions; section styling differs from drawer/forms. | Reuse the same heading, label, inset, and section spacing roles. Keep paths complete and wrapped; retain read-only/unavailable distinctions and destructive confirmations. |
| Low / SH-615 | **Overlays:** viewport-bounded modals, toast widths, and the shared modality system already solve real problems. New compact surfaces must not introduce a second focus model. | Reuse modality/return-focus registration and notice-layer behavior. Normalize padding and headings; let long bodies scroll without hiding their close/action controls. |
| Low / SH-615 | **Empty and error states:** “No stories match” and the connection dot are useful but visually subordinate. On mobile, connection text is visually hidden. | Keep quiet healthy state, but show labeled reconnecting/disconnected/error feedback; preserve error detail, retry behavior, and live announcements. Retain distinct no-project, no-story, filtered-empty, and failed-read states. |

Color findings are risks identified from source roles and screenshots, not a new
WCAG conformance verdict. Measure actual foreground/background combinations before
changing tokens. A confirmed functional accessibility failure should be recorded
and prioritized by the shipped priority rubric, rather than hidden in cosmetic work.

## 3. SH-613 — compact navigation and filters

Use the existing **768px inclusive** breakpoint for layout, independent of pointer
type. Touch target sizing remains pointer-dependent. The narrow desktop window gets
the same compact organization; a wide touch device retains generous targets.

Proposed phone shell (schematic; text labels are not pixel measurements):

```text
┌─────────────────────────────────────┐
│ SH · project…          + New   More  │  Identity / creation / secondary navigation
│ Search stories…          Filters ●  │  Search / active-filter indicator
│ [ Board | List ]       95/607  Run   │  View / results / separate automation group
├─────────────────────────────────────┤
│ Stories or the existing detail peer │  Reading area starts here
└─────────────────────────────────────┘

Filters opens over that shell; it does not push the story region down:
┌─────────────────────────────────────┐
│ Filters                        Done │  Labeled modal, explicit close action
│ Stories                             │
│ Priority · Assignee · Type · State   │  Scrollable controls; live changes
│ Show closed · Show epics · Archived │
│ Board display                       │  Only relevant in Board view
│ Columns · Hide empty columns        │
│ Clear filters                       │  Existing reset semantics
└─────────────────────────────────────┘
```

| Area | Required behavior |
|---|---|
| Header | Use 12px mobile outer insets and 8px row gaps. Project label flexes and elides with its complete accessible name. New remains filled; More is labeled. Home, Settings, and Drafts retain their names, commands, and a visible draft-count indicator in More when nonzero. |
| Results | Keep the existing visible/total meaning, including its empty-state behavior. Keep active filtering recognizable while the sheet is closed. Clear moves into the mobile sheet; desktop retains its always-visible Clear control. Do not represent filter activity using color alone. |
| Automation | Give Full Auto a separate group in the third row. Run uses a quieter outlined treatment and the full accessible name “Run Full Auto”. Preserve its confirmation dialog. Running/paused states keep status and Pause/Resume/Stop available, with one additional status row if needed. Lane details may open the existing operational surface; alerts and acknowledgement are never buried in More. |
| Filter sheet | Fixed, bottom-aligned, at most 85dvh with the existing vh fallback and safe-area padding. Header/Done stays visible; body scrolls. At most one live set of filter controls is mounted. Reuse current dropdown options, selection logic, and persistence. Changes apply immediately; Done, Escape and backdrop dismissal close without rollback. |
| Grouping | Stories contains Priority, Assignee, Type, State, Show closed, Show epics and Show archived. Board display contains Columns and Hide empty columns, hidden in List without resetting them. Do not add an Apply transaction or alter Clear's existing treatment of display preferences. |
| Focus | Register the sheet with the existing overlay stack, backdrop, inertness and return-focus system outside the covered app shell. Initial focus goes to its heading; Done is tabbable. Preserve the existing notice-layer exception and nested authorization dialog behavior. A closed sheet has no reachable controls. |
| Persistence / resize | Filter values persist exactly as today. Mobile sheet-open state is transient and starts closed even if the old desktop disclosure preference was true. Crossing the breakpoint closes the mobile sheet and any filter popover, preserves values, restores desktop disclosure preference, and transfers focus to the equivalent visible filter trigger when necessary. |
| Desktop | Above 768px retain the inline disclosure and exposed secondary navigation. Share filter grouping, labels, active-summary semantics, and automation separation. The desktop header may wrap under content pressure; it must not create page overflow. |

Default-size idle header limits, measured from viewport top to story-region top:
**192px at 375×667 and 390×844; 208px at 320×568**. The allowance covers three
44px control rows, segmented-control padding, 24px outer padding, and row gaps;
the narrowest width has extra pressure allowance. A running/paused status row may
add at most **56px**. Error/incident banners are excluded from that size budget
because complete diagnostics outrank density, but must leave a reachable scroller.
At enlarged text, allow wrapping instead of enforcing these default-type limits.
For every state, opening the sheet must leave the underlying story-region top and
height unchanged (within the browser geometry measurement's precision).

Rejected: shrinking targets/fonts; leaving the expanded panel in normal flow;
a permanent bottom navigation bar that spends another content row; hiding search
behind an icon; or putting live filter changes behind an unnecessary Apply step.

## 4. SH-614 — readable mobile list

At <=768px, present one semantic list of stacked story rows. Keep the desktop
table above that boundary. Use the existing filtered/sorted collection and shared
metadata/action builders; do not fork query, sort, or mutation logic. Only the
active presentation is exposed to focus and the accessibility tree.

```text
Sort: [Order ▾] [Ascending ▾]         Mobile list controls, within content scroller
┌─────────────────────────────────────┐
│ Attachments: drawer thumbnail strip │  Full-width, complete title / open detail
│ + modal viewer                      │
│ SH-390   [in-progress]   ● low   ⋯  │  Identity / state / priority / actions
│ Details ▸                           │  Order, Labels, Assignee, Updated, type name
└─────────────────────────────────────┘
```

| Contract | Implementation requirement |
|---|---|
| Readability | Title uses the row's entire inner width at 16px/1.4 on mobile, with natural word wrapping and overflow wrapping for uninterrupted tokens. No line clamp or ellipsis on story titles. Metadata may wrap between complete items, never squeeze the title beside it. |
| Semantics | Use list/listitem structure with a native title button that opens story detail. Actions and Details are sibling buttons, not descendants of the title button. Preserve current arrow navigation, Enter, Shift+F10/Menu-key actions and roving focus behavior against the active presentation. |
| Metadata | ID/type, displayed state and priority are visible. Details reveals labeled Order, Labels, Assignee, Updated and the type's text name inline, without entering edit mode. Preserve empty values, computed/displayed state explanations, blocked indication and Full Auto lane information. |
| Sort | Offer ID, Order, Title, State, Priority, Assignee and Updated, with ascending/descending direction using existing `state.sort`. Labels remains unsortable. Defaults, stable tie behavior and carried preferences stay unchanged. Desktop header sorting updates the same state. |
| Actions / polling | Use `openStoryMenu` and look up current data by ID on activation. Reconcile by story identity; unrelated updates retain the focused element and open metadata disclosure. Keep disclosure state per project/story for the current session, clear it when leaving the project, and do not persist it across reloads. |
| Resize | Preserve selected story, filter/sort state and any open detail peer across the breakpoint. Transfer focus to the matching story/control in the new presentation; if the story disappears, use the existing nearest-row fallback. Do not retain focus inside a hidden representation. |
| Desktop table | Give `.col-title` a 20ch minimum inline size, retain complete wrapped prose and the scrollable table container. Keep every current column. Never apply the title width floor to the mobile stacked layout. |

The exact screenshot title, **“Attachments: drawer thumbnail strip + modal viewer”**,
must occupy at most four lines at 320px using default type size, with the complete
title visible and no horizontal scrolling. Treat this as a readable-width fixture,
not a universal row-height cap: longer titles and enlarged type may grow naturally.
Long unbroken titles, labels, project names and custom state names must stay inside
their own content regions, without page-level horizontal overflow.

Rejected: hiding columns without an alternative; retaining horizontal scrolling
as the primary phone reading workflow; title truncation that forces opening every
story; or globally removing metadata nowrap and reintroducing broken identifiers.

## 5. SH-615 — shared visual hierarchy

Keep the current palette family, 8px/6px radius scale, semantic colors, and SVG
icon style. This is a tune-up, not a new theme or animation system. Introduce shared
CSS roles for the following values and use them across the affected surfaces;
do not scatter another set of per-component literals.

| Role | Chosen value / application |
|---|---|
| Outer inset | 12px at <=768px; retain 20px on desktop for main panels/forms. |
| Spacing | 4px label/value gap, 8px related controls, 12px card/row padding, 16px between sections. Keep larger spacing only for distinct destructive/confirmation groups. |
| Titles | Mobile story titles 16px, weight 600, line-height 1.4. Preserve the drawer's existing larger title. Keep desktop story-title sizing unless it fails the readable-width contract. |
| Functional labels | 13px, weight 600, sentence case; use readable foreground rather than faint decoration. Mobile editable controls retain their existing >=16px tokens. |
| Metadata | 13px/1.4 for state, priority, assignee, labels and result counts. Keep identifiers monospaced. Do not raise a whole form's weight merely to improve contrast. |
| Emphasis | Title first, operational exception second, ordinary metadata third. New is the primary filled creation action; navigation and disclosures are neutral. Retain colored state/priority meaning and warning/error treatments. |
| Borders / surfaces | One raised surface per row/card; avoid adding extra containers around every metadata item. Preserve state pills where they encode status. Keep focus outlines distinct from selected/hover decoration. |
| Contrast | Changed ordinary text must meet 4.5:1; identifying control graphics/boundaries and focus indicators must meet their applicable 3:1 contract. Measure explicit light/dark plus both system-resolved themes, including hover/focus/active/error states. Prefer a semantic functional-text role over globally darkening `--fg-faint` and changing unrelated decoration. |
| Feedback | Healthy connection remains quiet with an accessible status. Nonhealthy connection shows short visible text and retains its detailed error/retry path. Empty states retain their correct meaning; never replace failed loading with “no stories”. |

Do not restructure the board card's nested action accessibility model as aesthetic
cleanup; the existing keyboard path and related technical constraints remain in
force. Do not add touch dragging, new navigation destinations, new settings, or
change story/automation semantics. SH-611 owns verification-text containment.

These choices use [W3C reflow guidance](https://www.w3.org/WAI/WCAG22/Understanding/reflow.html),
[target-size guidance](https://www.w3.org/WAI/WCAG22/Understanding/target-size-minimum.html),
[text contrast guidance](https://www.w3.org/WAI/WCAG22/Understanding/contrast-minimum.html),
and the [modal dialog pattern](https://www.w3.org/WAI/ARIA/apg/patterns/dialog-modal/).
The project's 44px coarse-pointer target is retained; it is not a claim that WCAG's
minimum criterion universally requires 44px. Desktop remains at least the existing
24px target floor. Real iOS browser chrome, software keyboard, touch and pinch-zoom
remain device-validation limitations, as recorded in the earlier responsive spec.

## 6. Acceptance and regression matrix

Each child owns its new tests and directly impacted regressions. First demonstrate
its reported defect against production rendering, then implement. Run the repository
impacted-test selector against the actual diff before choosing direct commands.
Use the isolated `scripts/run-e2e.sh` harness with explicit spec/project selection;
do not exercise production tracker data. The central verifier owns the full suite.

| Dimension | Required cases and assertions |
|---|---|
| Geometry | 320×568, 375×667, 390×844, 768×1024, 1280×800; 390×400 short viewport; 768/769 breakpoint transitions. Assert shell budgets, story-region bounds, target sizes, full title visibility, and document overflow. |
| Content | Screenshot title; long unbroken title; long ID/project name; custom long state; many labels; missing assignee; no Order rank; empty/filtered-empty/loading/error. Measure text/element containment, not screenshot pixels alone. |
| Filters | Closed/open, no predicates, multiple predicates, all results/zero results, Clear, reload, project switch and Board/List switch. Prove that board preferences retain their current reset/persistence semantics. |
| Automation | Idle, running, paused, draining, halted/error and acknowledgement-required. Long lane/error content stays reachable; Pause/Resume/Stop/confirmation and state updates remain functional. |
| Interaction | Tap/click menus, keyboard navigation, focus return, nested token dialog, disclosure state, polling during focus, updated story actions, project change with detail open, and resize with an overlay open. |
| List parity | All sortable columns/directions and all metadata fields; desktop/mobile order equality; far desktop columns reachable; no mobile table-scroll requirement; unchanged rows retain focus through unrelated updates. |
| Appearance | Explicit light/dark and system-light/system-dark, default and 200% text, narrow reflow equivalent to 400% desktop zoom, fine/coarse pointer, reduced motion. Default-type size budgets do not cap enlarged text. |
| Engines | Both Chromium and WebKit for changed behavior; both mobile projects for mobile contracts. Emulation does not prove real-device browser chrome or software-keyboard behavior. Record that limitation without blocking this documentation PR. |

Extend relevant coverage in `responsive.mobile.spec.ts`, `zoom.mobile.spec.ts`,
`list-row-reconcile.mobile.spec.ts`, filter/sort/menu/focus and engine specs instead
of asserting CSS strings as a substitute for the interaction. The current mobile
test demanding horizontal table overflow must be replaced when SH-614 lands;
retain a desktop table overflow/reachability case. Preserve tap-target measurement
settling and precision handling rather than copying naive floating-point thresholds.

Capture representative before/after views for each changed surface and note the
viewport, engine and theme. Store non-text review assets under
`~/Enderchest/storyhook/mobile-visual-tune-up/` when permitted; the committed spec
must remain understandable from its text and diagrams without those local assets.

## 7. Delivery and compatibility

| Story | Type / priority | Deliverable and relationship |
|---|---|---|
| SH-610 | Normal / medium | This audit/specification PR; no UI implementation. |
| SH-612 | Epic / medium | Organizes the three children; related to SH-610. No executable steps on the epic. |
| SH-613 | Bug / medium | Section 3 and its tests. Child of SH-612; blocked by SH-610 until the design lands. |
| SH-614 | Bug / medium | Section 4 and its tests. Child of SH-612; blocked by SH-610 until the design lands. |
| SH-615 | Normal / low | Section 5 and its tests. Child of SH-612; blocked by SH-610 until the design lands. |
| SH-611 | Existing separate work | Related to SH-612; verification-text containment, not a new child or duplicate fix. |

The children are independent of one another; they share this design contract and
must reconcile concurrent edits without broad refactoring. Completion of SH-610
delivers the requested audit/backlog, **not** the visual fixes. SH-612 stays open
until its children complete. Its children each ship focused commits and regression
tests; do not put their executable steps on the typed epic.

This design supersedes SH-235's all-width inline filter disclosure **on narrow
layouts only**, its always-visible mobile Clear control, its persisted mobile
panel-open preference, and its horizontal-table-only phone strategy. It retains
desktop disclosure persistence, filter/sort value semantics, dynamic viewport
fallbacks, zoom protection, action-menu parity, target sizing, and column peek.
The shared aesthetic changes apply as SH-615 lands. No public API, stored story
schema, or release/version work is required. Record implementation deviations in
the affected section and in the owning story at decision time.

### As built — SH-610 audit

The screenshots and source/test contracts were inspected, and SH-612 through
SH-615 were created with the relationships above. Dashboard behavior is unchanged.
Runtime and visual acceptance for the proposed design remain the children's work;
this audit provides no new runtime pass or device-certification claim.
