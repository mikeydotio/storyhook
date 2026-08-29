# Markdown in the dashboard: the description read/edit split, and comment bodies

Design of record for **SH-217**, which absorbs the abandoned **SH-278** ("Linkify story
IDs in the description field's read mode") — SH-278 existed only because linkification
needed a read/edit split the drawer didn't otherwise have, which is the split this story
builds anyway. Written after implementation for the same reason
[`dashboard-dispatch.md`](dashboard-dispatch.md) and [`responsive-dashboard.md`](responsive-dashboard.md)
give: sharper against the actual code and the actual failures it produced than against a
proposal for either.

## Context

The drawer rendered the description as a bare `<textarea>`, and comment bodies as
`white-space: pre-wrap` plain text — including their own headings, bold, tables and
fenced code, all shown as raw punctuation, in a project whose own story bodies (this
document among them) are markdown documents. SH-217 asked for the description to render
as markdown, and to fall back to raw source while focused for editing; SH-197's and
SH-199's own plan comments each deferred "markdown rendering of comment bodies" to this
story.

Scope: GFM-lite — headings, paragraphs, bold/italic/strikethrough, inline code, fenced
and indented code, links, autolinks, nested lists (tight/loose, task items), blockquotes,
thematic breaks, and GFM tables. No images, no raw HTML, no footnotes.

## The constraint that shaped every decision

`src/web_dashboard.html` is one embedded file with **zero third-party JavaScript**, and
cannot gain any: the CSP (`src/api/http.rs::CSP`) is `script-src 'unsafe-inline'` with no
`'self'` and no host source, so an external `<script src>` is dead on arrival, and
`tests/web_test.rs` pins exactly one `<style>` and one `<script>` tag. The file had **zero
`innerHTML` assignments** before this story (its one prior reader, `esc()`, had zero call
sites and was deleted as part of this work) — everything is built through `el()` and
`document.createTextNode`.

`script-src 'unsafe-inline'` means the CSP would **not** stop an injected inline handler.
So the renderer emits **DOM nodes, never HTML strings** — structurally immune rather than
sanitized-and-hoped-for. `tests/web_test.rs` pins the absence of every string-to-markup
sink (`innerHTML =`, `insertAdjacentHTML`, `outerHTML`, `document.write`, `new Function(`,
`eval(`) in the served body, so a future edit that adds one fails loudly.

## The renderer

`renderMarkdown(text, selfId)` returns a `DocumentFragment`. Two passes:

- **Block** (`appendBlocks`): blank, fenced code, thematic break, ATX heading, blockquote,
  GFM table, list (nested, tight/loose, task items), indented code, paragraph as the
  fallback. First match wins per line group.
- **Inline** (`appendInline`), left to right, in precedence order: backslash escape, code
  span, autolink, `![...]` consumed as literal (no image support — the whole construct,
  not just the `!`, degrades to source text, so `![alt](url)` never silently becomes a
  link to `url` with `alt` as its text), link, emphasis/strong/strikethrough, otherwise a
  plain text run flushed through the existing `linkifyStoryIds()`.

**Story IDs are linkified inside rendered markdown.** Every inline text run passes
through `linkifyStoryIds(run, selfId)` — the same function comment bodies and the blocked
banner already used — so `SH-9` inside a description or comment becomes the same lit
`storyRef()` it already was. Code spans and code blocks emit literal text and are never
linkified; this is where "story IDs stay literal inside code" is enforced.

### Link safety, and why the threat model is narrower than it looks

Every `href` is assigned via `el()`'s property path (`a.href = value`), never parsed as
HTML — no HTML parser ever sees these strings, so entity obfuscation
(`&#106;avascript:`) is a no-op here; it only matters when a string is later parsed as
markup, which this file never does. What survives a property assignment is whitespace and
control characters inside the scheme, and case — `safeHref()` strips those, lowercases,
then allowlists by prefix: `http:`, `https:`, `mailto:` only. No relative or scheme-less
links, no protocol-relative `//`. A rejected link renders as its bracketed text with no
`<a>` at all. Every accepted link gets `target="_blank"` and `rel="noopener noreferrer"`
(the existing PR-link anchor elsewhere in the file predates this and keeps its own bare
`rel="noopener"` — harmonizing it is a separate, single-line change, not folded in here).

## The description's read/edit split

Both `.description-view` (rendered markdown, `tabindex="0"`) and the raw
`.description-field` textarea stay in the DOM always; a class on the wrapper
(`.description-section` / `.editing`) picks which is visible. Keeping the textarea always
present is what lets `restoreDrawerFocus()` keep working unchanged — it looks up
`[data-field="description"]` and expects to find it.

**Edit mode is derived from focus, not stored**, for the case that must survive a changed
description section being replaced: `captureDrawerFocus(body)` runs before reconciliation;
if the textarea held focus, `buildDescriptionSection(st, true)` builds its replacement
already in edit mode. There is no mode flag to reset on drawer close, story switch, or a
failed PATCH — the class of bug where a flag outlives the thing it describes cannot occur.

That alone is not sufficient — the day-to-day toggle. Blur only produces a re-render when
the value actually changed (`runFieldMutation` → `handleMutationSuccess` →
`renderDrawer`); focus in, change nothing, click away, and nothing would otherwise flip
the view back on. So entering/leaving edit mode is driven **directly** by the interaction
handlers (`enterEdit`/`exitEdit`, toggling the wrapper's class and re-rendering the view's
content), and the derived-focus mechanism above only exists to **reproduce** that state
across a rebuild. Both halves are needed; neither is sufficient alone.

**Entering edit is activation (click, or Enter/Space), never the view's own `focus`
event.** Triggering on `focus` would build a real trap: the view is `display: none` while
editing, so it's excluded from sequential focus navigation entirely (a `display: none`
element has no focusable area, in both tab directions) — Shift+Tab out of the textarea
lands on whatever precedes the description section once the exit handler makes the view
visible again, with no loop possible. Two clicks deliberately do **not** enter edit mode:
one landing on a rendered link or `storyRef` button (following it is the point), and one
that ends a non-collapsed text selection (so drag-to-copy out of the rendered view doesn't
yank the reader into the editor and discard their selection).

### The swallowed-click hazard, found by the interaction's own test suite

`blur` fires mid-gesture — before `mouseup`/`click` — on whatever element the user's click
is landing on next. The first implementation ran `exitEdit()` synchronously inside `blur`,
which changes the section's height; a real e2e test (clicking the Comments toggle
straight out of an edit) proved this shifts layout out from under the still-in-flight
click and can make it miss its target entirely — the toggle's `aria-expanded` never
flipped, even though the description's own PATCH landed. Fixed by deferring `exitEdit()`
one animation frame from the blur handler (committing the value is **not** deferred,
since nothing about it depends on layout): the frame after the current gesture's
`mouseup`/`click` have already resolved against the pre-swap layout. Verified: 27/27 green
across three repeats of the full interaction suite after the fix, versus a reproducible
failure before it.

### `autoGrowTextarea` on a hidden textarea

`autoGrowTextarea` measures `scrollHeight`, which is `0` on a `display: none` element.
Entering edit mode calls it again, inside `enterEdit()`, after the section becomes
visible and before `.focus()` — otherwise a long description would open into a
collapsed box until the first keystroke.

## Comment bodies

`buildCommentsSection` swaps its direct `linkifyStoryIds(c.text, st.id)` call for
`renderMarkdown(c.text, st.id)` — linkification is inside the renderer now, so no
behaviour is lost. `.comment-text` drops `white-space: pre-wrap`: once content is
block-structured, pre-wrap would double every blank line the block parser already
consumed into real paragraph spacing. `overflow-wrap: anywhere` stays, unrelated to that
change, for long URLs in a 30rem drawer.

Two other `linkifyStoryIds()` consumers were considered and deliberately **excluded**:
the blocked banner's `awaiting` reason and the Referenced By snippets. Both are
single-line or deliberately truncated text where block-level markdown (headings, lists)
would look broken; they keep plain `linkifyStoryIds`.

## Deliberate divergences from CommonMark

**Soft line breaks collapse to a space (CommonMark), not a hard `<br>` (GitHub's comment
convention).** Decided by council vote, unanimous 3-0 after deliberation. That trail was
untracked and worktree-local and is gone (SH-363), and it belonged to no story, so what
follows is the whole of the record. The deciding fact: this project's own
stored descriptions are hard-wrapped near column 88 for terminal display, so a bare `\n`
there is a formatting artifact, not an authorial paragraph break — hard breaks would
render every existing description jagged the moment this shipped, a corpus-wide
regression on real data. The correction that made the vote unanimous: GFM itself
specifies CommonMark's soft-wrap rule; GitHub's hard-break rendering is an undocumented
product choice scoped to its own comment/issue textareas, not a property of the spec this
story is otherwise scoped to follow. Risk accepted, not engineered around: a user typing
casual notes with bare Enters will see lines collapse on blur — CommonMark's own escape
hatch (trailing two spaces, or a backslash) still produces a hard break for anyone who
wants one deliberately.

A real, separate defect surfaced during the vote: `story decompose` drops blank lines and
joins body lines with a literal `\n` (`decompose.rs`), which will show as cosmetic
mid-paragraph breaks in decompose-ingested prose under soft-wrap too. Out of this story's
scope; file it separately if it proves to matter in practice.

**Emphasis matching is pragmatic, not CommonMark-complete.** `_`/`*` only open after
start/space/punctuation and only close before end/space/punctuation (keeps
`snake_case_name` from italicising); a single-character marker search skips over a
same-character run longer than itself, so `*a **b** c*` correctly finds strong nested
inside em rather than treating the first `*` of `**b**` as the em's own close. Full
CommonMark delimiter-stack resolution was not implemented — out of scope for a
dependency-free renderer whose actual content is this project's own prose, not
adversarial markdown.

**No setext headings, no footnotes, no images, no raw HTML rendering.** `![alt](url)`
degrades to its literal source text (not a link to `url` with `alt` as link text — the
whole construct is consumed as one unparsed span). A literal `<b>` or `<script>` tag in
source renders as visible text and creates no element — it can only ever become a text
node, by construction of `el()`.

## The `.md` CSS block and its one real collision

Rendered markdown (`.description-view` and `.comment-text`, both carrying `.md`) styles
headings, code, tables, blockquotes, lists and `hr` from existing tokens only — no new
colour literal, so dark mode and the `data-theme` overrides apply for free. A code block
and a wide table each scroll their own `overflow-x: auto` rather than widening the drawer
or wrapping code onto new lines (the same lesson `responsive-dashboard.md`'s defect D2
names for the list view's own table).

**The collision:** the list view styles bare `table`/`thead th`/`tbody tr` as its own
sortable, clickable board. Without an override, a rendered markdown table would silently
inherit `cursor: pointer` and a hover highlight meant for board rows. `.md table`/`.md
thead th`/`.md tbody tr` reset those. One coarse-pointer tap-action property rides along
from that same shared selector and is deliberately left as-is — a Rust test pins the
sheet to exactly one such declaration, on the tap-targets list, and the property does
nothing harmful on a table.

**No `--tap-min` floor on `.md a`.** WCAG 2.2 SC 2.5.8 exempts a target inline within a
sentence or block of running text — a `min-height: 44px` on an inline link would wreck
the paragraph's own line layout for no accessibility gain. `.rel-id` (a standalone story
reference, not inline in prose) is unaffected and keeps its own floor from SH-235.
`e2e/specs/responsive.mobile.spec.ts`'s coarse-pointer tap-target sweep excludes any
`a[href]` inside `.md` for the same reason.

## What guards each piece

| Piece | Structural test (`tests/web_test.rs`) | Behavioral test (`e2e/specs/`) |
|---|---|---|
| No markup built from strings | the sink-pin assertions in the marker test | — |
| Renderer/link-safety source markers | `web_serve_root_html_has_board_list_drawer_markers` (SH-217 block) | — |
| Read/edit swap mechanism, `.md` CSS | `web_serve_root_html_styles_the_description_read_edit_swap_and_rendered_markdown` | — |
| The grammar (block + inline constructs) | — | `markdown-rendering.spec.ts` |
| Unsafe link schemes never become `<a>` | — | `markdown-rendering.spec.ts` |
| Safe links: new tab, no referrer | — | `markdown-rendering.spec.ts` |
| Story IDs linkified in rendered text, literal in code | — | `markdown-rendering.spec.ts` |
| The read/edit interaction contract, incl. the SH-218 race and the swallowed-click fix | — | `description-edit-mode.spec.ts` |

A renderer with no exported seam (the whole dashboard script is one IIFE with nothing on
`window`) is proved through comment bodies in `markdown-rendering.spec.ts` — a read-only
surface with no focus/edit machinery in the way, rendering through the exact same
`renderMarkdown()` call the description's read view uses. The description path then needs
only the cases that are about *it*.

## Out of scope, named rather than assumed

- **SH-283** — filed as a pre-existing defect in `captureDrawerFocus`/`restoreDrawerFocus`
  found while specifying this story's exact focus-capture semantics: the filed premise
  was that WebKit doesn't move focus to a clicked `<button>`, so a focused description
  left uncommitted in story A could have its stale value written into story B's drawer
  after a `storyRef` click. Investigation found that premise doesn't hold in either
  WebKit or Chromium — `mousedown`'s default action blurs the old field before the
  click event (and this app's `onClick` handlers) ever run, in both engines, so no
  click- or keyboard-driven path was found that actually reaches the leak. The
  snapshot's lack of story identity was real regardless, though, so it was closed as
  defensive hardening rather than a live-bug fix — see SH-283's own comment for the
  full forensic record and `src/web_dashboard.html`'s `renderDrawer()`. Not caused or
  widened by this story's own surfaces either way (the description view's `storyRef`
  links are only clickable in read mode, where the textarea isn't focused).
- Harmonizing the pre-existing PR-link anchor's bare `rel="noopener"` with this story's
  `rel="noopener noreferrer"` convention.
- Rescoping the list view's bare `table`/`thead th`/`tbody tr` selectors so `.md`
  wouldn't need its own override block at all — a cleaner repair, but a surface this
  story has no business changing.
