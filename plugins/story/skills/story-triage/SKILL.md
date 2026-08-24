---
name: story-triage
description: "Use when the project backlog needs review -- stories need prioritization, stale items need attention, or work needs reorganization. Reviews all open stories, identifies issues, and guides reprioritization."
---

# Storyhook Triage

You are a **thin router** for gathering and presenting findings. Deterministic work — the
four reads this used to run one at a time, and the classification of every story into an
issue category — lives in `bash "<story-helper>" triage`. **Route and
render for that part — never call `story` yourself to gather or classify.** The
*resolution* commands in step 3 are direct `story` calls, unchanged: each is already one
unambiguous CLI invocation with nothing to parse.

## Resolve packaged files and helper

Take the absolute directory containing this loaded `SKILL.md` as `<skill-dir>` and resolve
`<plugin-root>` as the normalized absolute path `<skill-dir>/../..`. Load
`<plugin-root>/references/helper-command.md` and follow it to resolve `<story-helper>`.
Substitute absolute paths for every placeholder below and shell-quote them. Never resolve
packaged files from the user's current working directory.

## Steps

### 0. Ensure the storyhook CLI is available

Load `<plugin-root>/references/ensure-cli.md` and follow it. Do not continue until it passes.

### 1. Gather and classify

Run `bash "<story-helper>" triage`. `ok:false` → show `display`, stop.

`findings[]` is every issue found, each `{id, title, category, detail}` with `category`
one of:

- **`blocked`** — has an unmet dependency or an explicit `awaiting` reason (`detail`
  carries it)
- **`stale`** — not updated within `STORY_STALE_THRESHOLD`; each finding's own `detail`
  states the real window used (e.g. `"stale 3d+"`), never restated here
- **`unprioritized`** — a legacy story whose history predates the required
  priority invariant. Current creation defaults to `low`; this diagnostic stays
  able to identify an old unassessed representation until schema migration 19
  normalizes it
- **`cycle`** — sits on a `blocked-by` cycle — the CLI does not surface this itself, so
  the script detects it directly from every story's own relationships (Kahn's algorithm);
  this is no longer a manual "eyeball the graph" step
- **`orphan`** — no relationships at all; may be missing a dependency or a parent

`counts` gives the total per category. A story can appear more than once (e.g., both
`stale` and `unprioritized`).

### 2. Present findings

Show `display` — it is already organized by severity (blocked, stale, unprioritized,
cycle, orphan) with each finding's id, title, and detail. If `findings` is empty, `display`
says the backlog looks clean; report that and stop.

### 3. Interactive resolution

For each finding, ask one concise question about what to do and execute their decision with the matching
direct CLI command:

- **Reprioritize**: `story prioritize <id> <critical|high|medium|low>` — run
  `story help priority-rubric` first and choose against its criteria. A level is
  `story next`'s sort key, ties break toward the older story, and every inflated
  level permanently costs the resolution of the level it joins, so on a genuine
  tie take the lower one
- **Add labels**: `story label <id> <labels-csv>`
- **Remove labels**: `story unlabel <id> <labels-csv>`
- **Clear blockers**: `story unblock <id>`
- **Set new blockers**: `story block <id> "<reason>"`
- **Close stale stories**: `story move <id> done "Closed during triage -- no longer needed"`
- **Add relationships**: `story relate <a> <relationship> <b>` — the only valid relationships are `blocks`, `blocked-by`, `parent-of`, `child-of`, `relates-to`, `duplicate-of`, `obviates`, `obviated-by` (use `blocks`/`blocked-by` for execution-order dependencies, not `relates-to`)
- **Remove relationships**: `story unrelate <a> <relationship> <b>`
- **Collapse a duplicate**: `story relate <dup> duplicate-of <keep>` then
  `story delete <dup> "duplicate of <keep>"` — soft and reversible, the reason
  travels with the deletion and `story reopen` undoes it. Two stories
  describing one problem cost two schedulings and two plannings, and the
  second session rebuilds context the first already had; see
  `story help scope-rubric`
- **Absorb an obsoleted story**: `story relate <keeper> obviates <obsolete>`
  then close or delete the obsolete one

A `cycle` finding needs one of its `blocked-by` edges removed or redirected
(`story unrelate`/`story relate`) — which edge is wrong is a judgment call the script does
not make; ask the user.

### 4. Verify

After all changes, run `story summary` to show the updated project state and confirm the
backlog is in good shape.
