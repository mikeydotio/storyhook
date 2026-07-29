# `/story new <description>` — interrogate, draft, file

Loaded on demand by the `story` router. Turns a braindump into one well-formed
storyhook story. **Never call `story` yourself — filing goes through
`bin/story.sh create`.**

## 1. Anchor

Restate the braindump in one sentence so the user can see what you heard.

## 2. Cheap recon only

If the description names a file, symbol, or area, take **one** look (`Grep` or
`Read`) so your questions are informed. No agents, no deep research — this is a
filing flow, not an investigation. If the story turns out to need investigation,
that is what `/story do` is for once it's filed.

## 3. Interrogate

**Exactly one `AskUserQuestion` call.** Ask only what you genuinely cannot infer;
never pad to four questions. Useful dimensions:

- **Type** — `bug` / `story` / `chore` / `task` / `epic` (the slugs this project
  defines; run `story type list` rather than assuming).
- **Scope** — what's in, and explicitly what's out.
- **Acceptance** — for a bug: expected vs. actual, plus repro steps and the last
  known good state. For a feature: the observable outcome that means "done".
- **Priority** — only if the user signalled urgency.

## 4. Draft

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

## 5. Confirm

**Exactly one `AskUserQuestion`**, showing the drafted title and body:
**File it** / **Edit first** / **Cancel**.

- **Edit first** → revise, then confirm again.
- **Cancel** → stop. File nothing.

Never file without an explicit "File it".

## 6. File

Write the body to a file first — **do not pass markdown through a shell command
string.** Backticks would run command substitution and `$NAME` would expand;
a file avoids both. Use the session scratchpad directory if one exists,
otherwise a temp file.

```
bash ${CLAUDE_PLUGIN_ROOT}/bin/story.sh create \
  --title "<title>" \
  --description-file <path> \
  [--type <slug>] [--priority <level>] [--label <csv>]
```

## 7. Report

Show `display` (it names the new story id).

**On `ok:false`, report and stop — do NOT retry.** A repeated `create` files a
duplicate story; there is no idempotency key to protect you.

## 8. Relate (only if asked)

If the user described a dependency ("blocked by X", "part of epic Y"), say which
relation you'd add and offer it as a follow-up. Relationship edits are
`/story triage`'s job, not this flow's.
