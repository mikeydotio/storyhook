# File a new story — interrogate, draft, file

Loaded on demand by the `story` router. Turns a braindump into one well-formed
storyhook story. The loading skill has already resolved `<story-helper>` from its installed
`SKILL.md` location. **Never call `story` yourself for filing — use
`bash "<story-helper>" create`.**

## 1. Anchor

Restate the braindump in one sentence so the user can see what you heard.

## 2. Search first

`story search "<key terms>"` and `story list --ready` for anything this could
already be. If an open story already covers it, say so and offer the
alternative to filing — **adopt** (comment the finding on the existing story,
widen its acceptance criteria) or **collapse** (`story relate <dup> duplicate-of
<keep>` then `story delete`) — per `story help scope-rubric`. This step never
overrides the user: if they still want a new story after seeing the match,
file it.

## 3. Cheap recon only

If the description names a file, symbol, or area, take **one** look (`Grep` or
`Read`) so your questions are informed. No agents, no deep research — this is a
filing flow, not an investigation. If the story turns out to need investigation,
that is what provider dispatch or the router's `claim` route is for once it is filed.

## 4. Interrogate

Ask only what you genuinely cannot infer in one concise interaction; use the host's
structured question mechanism when available. Useful dimensions:

- **Type** — `bug` / `story` / `chore` / `task` / `epic` (the slugs this project
  defines; run `story type list` rather than assuming).
- **Scope** — what's in, and explicitly what's out.
- **Acceptance** — for a bug: expected vs. actual, plus repro steps and the last
  known good state. For a feature: the observable outcome that means "done".
- **Priority** — only if the user signalled urgency.

## 5. Draft

Title: imperative, under 72 characters, no type prefix (storyhook has a real
`--type` field — don't duplicate it in the title).

Body:

```markdown
## Context

## Problem            (bugs)   |   ## Goal   (features)

## Proposed approach

## Acceptance criteria
- [ ] …

## Out of scope
```

For a bug, per this project's defect conventions, the body must carry what /
where / when / extent, repro steps, and the last known good state.

## 6. Confirm

Ask exactly one confirmation question showing the drafted title and body:
**File it** / **Edit first** / **Cancel**.

- **Edit first** → revise, then confirm again.
- **Cancel** → stop. File nothing.

Never file without an explicit "File it".

## 7. File

Write the body to a file first — **do not pass markdown through a shell command
string.** Backticks would run command substitution and `$NAME` would expand;
a file avoids both. Use the session scratchpad directory if one exists,
otherwise a temp file.

```
bash "<story-helper>" create \
  --title "<title>" \
  --description-file <path> \
  [--type <slug>] [--priority <level>] [--label <csv>]
```

`<csv>` means comma-separated: `--label backend,api` files two labels,
`backend` and `api`. Comma is always the label delimiter — a single label can
never contain one.

**Read `story help priority-rubric` before choosing a level.** Priority is
`story next`'s sort key, so it decides what the next session picks up. Omitted
priority defaults to `low`; omitted type uses the project's first configured
type. Pass both explicitly when the user's requested classification differs
from those defaults.

## 8. Report

Show `display` (it names the new story id).

**On `ok:false`, report and stop — do NOT retry.** A repeated `create` files a
duplicate story; there is no idempotency key to protect you.

## 9. Relate (only if asked)

If the user described a dependency ("blocked by X", "part of epic Y"), say which
relation you'd add and offer it as a follow-up. Relationship edits are
the `story-triage` workflow's job, not this flow's.
