# Cross-project create: choosing where a new story lands

Design of record for **SH-439**: "New Story dialog should have a Project dropdown
as the first field, with the current project auto-selected." Directly unblocks
**SH-442** ("Drafts should be globally visible, not project-specific"), which is
`blocked-by` this story — SH-442's own text: "story creation is also global, and so
too should drafting be." SH-442's own scope (the Drafts *popover* itself becoming
project-independent) is not touched here; this story widens only the create/edit
modal.

## The problem

The dashboard's board is scoped to one project at a time (`state.repoId`), and the
create-story modal inherited that scope silently: `openCreateModal()` built every
field from `meta()` — the *open* project's vocabulary — and `submitCreate()`/
`saveDraft()` POSTed to `apiBase()` = `repoApiBase(state.repoId)`, with no project
field in the request body at all. The project a story landed in was purely the URL
path segment the board happened to be showing when `#new-story-btn` was clicked.
Filing a story into a different project meant switching boards first, losing
whatever was already typed.

## Why the project is a URL segment, not a body field

`POST /api/repos/{id}/story` (`route_create_story`, `src/api/rest.rs`) reads only
`title`, `state`, `type`, `description`, `priority`, `labels`, `draft` from its
body and dispatches `Invocation::New` against `ctx`, which the route table already
resolved from `{id}` before the handler runs (`Route::Project`, `src/api/rest.rs`).
This mirrors the CLI/MCP door's own architecture: `Invocation::New` (`src/cli.rs`)
carries no project field on any variant — the project rides beside the invocation,
on the wire envelope's `project: Option<ProjectSelector>` (`src/api/wire.rs`), and
`route_create_story` never touches that envelope at all, since browser requests are
scoped by URL rather than by an explicit selector the way a CLI's `--project` flag
is. SH-439's dropdown therefore changes *which URL* the create modal's own requests
target — `createTargetProject`/`createApiBase()` — not the wire shape of any
request itself.

## No new server route

The dropdown needs a project's states, types, labels and defaults to repopulate the
form on switch — exactly what `GET /api/repos/{id}/data`'s `.meta` already returns
(`meta_json`, `src/api/rest.rs`). A dedicated `/meta` route was considered and
rejected: `repos_json` (the `GET /api/repos` handler backing the header's project
selector and Home's summary cards) already calls `report_data()` once per project
on **every** catalog fetch, which SSE's `repos-changed`/`repo-changed` events fire
on nearly every mutation anywhere in the store. A per-project vocabulary GET on
dropdown change adds no new server-side cost class — only wire bytes, on demand,
over loopback.

## Reset, not preserve, on a project switch

Changing `#create-project` resets `#create-state`/`#create-type` to the *newly
selected* project's own `meta.defaults`, rather than carrying over whatever slug
was previously chosen. This was a deliberate call, not an oversight:

Two projects can spell a state the same way — `review`, say — and mean different
points in two entirely different workflows. Preserving the slug across a switch
preserves the *spelling*, not the *meaning*, and does so silently: nothing about
the UI would say a carried-over `review` in project B is describing a different
place in B's own pipeline than it did in A's. This project has already paid for
exactly that shape twice:

- **SH-364**: a fixture and a migration each independently reused the same
  misspelled `events.kind` literal, agreed with each other, and matched *zero*
  real rows — "the gate was right; the fixture lied to it," in that story's own
  words. Matching on a shared string is not the same as matching on shared
  meaning.
- **SH-281** (`src/web_dashboard.html:9011-9028`'s own comment on the relation-kind
  select): a `<select>` silently changing its own value on the user's behalf
  recorded the wrong relationship, with no error and a UI that looked perfectly
  plausible afterward.

Resetting to the new project's own defaults is also simply this modal's *existing*
rule (SH-44: preselect the project's own first-configured OPEN state/type from
`meta.defaults`, the same selection `story new` applies server-side) applied
consistently on a later switch, rather than a second, competing rule invented for
it. `#create-priority` is unaffected by any of this: it is a **global**, closed
enum (`PRIORITIES`, `src/api/rest.rs`), identical in every project, so it is built
once in `openCreateModal()` and never needs rebuilding. Title, description, and
already-typed labels are project-independent and always carry over unconditionally.

## Safety begins when the selection changes

The modal's pre-existing in-flight guard (SH-312) reflected `createModalInFlight`
onto three buttons' `disabled` attributes. That is not, itself, a request guard:
`bindEnterSubmit($("create-title"), submitCreate)` calls `submitCreate()` directly
and never consults `disabled` at all. Once a vocabulary fetch could be in flight
at the same time as a potential submit, that gap became reachable — pressing Enter
while `#create-state`/`#create-type` still displayed a *previous* project's slugs,
mid-switch to a new one, could submit against the new project's URL while the form
visibly, and briefly, disagreed with itself.

`createModalBusy()` (`createModalInFlight || createVocabPending`) is checked as the
literal first line of `submitCreate()`/`saveDraft()`, not merely reflected onto a
button. The safety transition starts synchronously in `#create-project`'s `change`
handler: it cancels the prior debounce timer, advances `createVocabTicket` to
invalidate scheduled and in-flight work, and raises `createVocabPending` whenever
the visible selection differs from `createTargetProject`. The 150 ms debounce wraps
only the vocabulary GET. Consequently the selector can name the candidate project
immediately while every mutation remains blocked until that exact project's
vocabulary has loaded successfully and made it `createTargetProject`.

Returning to the already-loaded `createTargetProject` advances the ticket and
clears pending without refetching vocabulary. Modal open and close cancel the
timer as well as advancing the ticket, so delayed work from one singleton-modal
instance cannot revive pending state or alter a later instance.

`syncCreateModalButtons()` is the one place both flags drive the shared `disabled`
attributes, so the two reasons can never disagree about a control's final state.
`#create-project` itself is deliberately **never** disabled by either flag — it is
the control the user is actively operating — with a focus rescue to the modal
itself (`$("create-modal").focus()`) run before a currently-focused control is
disabled, since `trapOverlayTab()`'s own candidate filter excludes disabled
controls and would otherwise silently drop focus outside the overlay entirely.

Every create-modal action captures `createTargetProject` into a local once, at the
same point it captures its request base — not merely for symmetry, but because the
dropdown stays enabled during an in-flight *mutation* (only a vocabulary fetch
disables it), so the module variable could in principle move again before that
mutation's own `.then()`/`.catch()` runs. Reading the captured value there, rather
than the live module variable, is what keeps a toast or an error message naming the
project a request actually addressed.

## Disabled options, and why the open project's own option never is

`#create-project`'s options are disabled only when a project is **not** the one
already open **and** cannot be written to (`!r.available` — no checkout, or
unreadable; both causes are named in `repos_json`'s own doc comment,
`src/api/rest.rs`). The currently open project's own option is never disabled,
even when it is itself the read-only one. A submit against it still reaches the
server's existing `pathless_refusal` (422) with its own helpful message, rather
than being silently retargeted by the browser's option-list reset algorithm, which
prefers the first non-disabled option once the selected one becomes unavailable or
the list is rebuilt — a hazard specific to disabling the *selected* option, not to
disabling options in general.

## Cross-project confirmation

Two questions were settled with the user before implementation began (recorded here
verbatim, since the session implementing this story ran unattended afterward):

1. **The board stays put.** A story filed into a project other than the one on
   screen does not navigate the dashboard there. A success toast names the created
   story and its project instead — "`BB-4` created in `BB · Beta Project`" — via
   `toastCrossProjectFiling()`. A same-project create or draft-save stays exactly as
   silent as it always was: SH-127's verdict (the entering card's own animation is
   the confirmation) still holds for the case it was written for, and this is
   additive, in the same sense SH-358's priority warning already is, for the case
   that animation says nothing about — a card that enters a board nobody is
   looking at.
2. **`#new-story-btn` stays hidden off the repo screen.** The modal remains
   reachable only from an open board; the dropdown lets that board send a story
   somewhere else, but does not itself become a way to create a story with no
   project open at all (that would be a materially larger change, deliberately
   out of this story's scope).

`describeMutationFailure()`'s ambiguous-outcome sentence ("Check the board, then
try again") is false once a story can be filed outside the board on screen — that
board cannot answer for a write that never addressed it. The create modal's three
actions now pass their captured `targetProject`; when it differs from
`state.repoId`, the sentence names that project instead and `fetchReposOnce()`
refreshes its summary counts (visible in the project selector and on Home) even
though its own board isn't rendered.

## As built

No deviations from the plan approved on SH-439 before implementation began. Two
pre-existing derived fences caught real mistakes during implementation, both fixed
by changing the code rather than loosening the fence:

- `tests/create_modal_project_scope.rs` (new in this story) correctly flagged the
  vocabulary GET's own `repoApiBase(target)` call as not naming a captured base —
  and it shouldn't have been exempted, because `target` is a *candidate* project,
  not yet `createTargetProject` (which only updates once that fetch succeeds).
  Fixed by naming the local `base` there too, honestly: it is a project base, for
  the project being probed.
- `tests/dashboard_error_reporting.rs`'s pre-existing
  `every_loading_line_comes_from_the_one_generator` (SH-301) correctly flagged a
  first draft of the failure message for reusing the literal "Couldn't load"
  outside `readinessNote()`. That generator's "Retrying…" half would have been
  false here — a failed vocabulary fetch never auto-retries; a re-selection of the
  dropdown does — so the fix was different, honest wording ("Failed to load…"),
  not routing an outcome through a generator whose contract doesn't match it.

## Global draft follow-up

SH-442 subsequently made draft discovery global and made the existing editor
reachable from Home, Settings, or any project board. Editing still pins
`#create-project` to the draft's owner: global visibility does not redefine a
story's project identity or introduce a transfer operation. See
`docs/spec/global-drafts.md`.

## Tests

- `tests/create_modal_project_scope.rs` — a derived fence (in the style of
  `tests/dashboard_deadline_knobs.rs`) proving every request the CREATE MODAL
  section of `src/web_dashboard.html` makes names its captured project base,
  never the board-scoped `apiBase()`. Mutation-checked in both directions.
- `tests/web_test.rs::web_serve_root_html_closes_the_create_modal_when_its_project_vanishes`
  — a small adjacent fix (`fetchReposOnce()`'s deleted-elsewhere branch now closes
  an open create modal too, matching the drawer's own SH-290 treatment), found and
  fixed in the same session while touching this code path.
- `e2e/specs/create-story-project.spec.ts` — the dropdown is the modal's first
  field and preselects the open project; a project switch repopulates state/type
  to the new project's own defaults and drops the old project's own state from the
  option list; submitting files the story in the selected project and not the open
  one, with a toast naming both; a same-project create raises no toast; a
  no-checkout project's option is disabled; and editing a draft pins the dropdown.
  SH-485 adds same-renderer-task proof that selection immediately disables all
  guarded controls and that immediate Save Draft/Enter dispatch no request; a
  released vocabulary read enables one Beta POST; Alpha→Beta→Alpha cancels the
  read and reuses Alpha vocabulary; a held Beta response delivered inside Delta's
  newer debounce window cannot clear pending, replace vocabulary, or become the
  mutation target; and current-request failure still reverts, restores controls,
  reports the existing wording, and preserves rescued modal focus. The original
  in-flight Enter regression remains as a second boundary around the same guard.

This is a **wiring** fence in the sense SH-360 draws that distinction: it proves
every create-modal request is addressed to the project the UI names, never that the
named project is the intended one — a defect in what the user *meant* to select is
outside what any of the above can catch.
