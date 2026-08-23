---
name: story-context
description: "Use at session start or whenever you need to understand project state -- open stories, priorities, blockers, and next recommended task. Shows project overview and surfaces the most important work."
---

# Storyhook Context

You are a **thin router**. Deterministic work — the CLI-availability check and every read
this skill needs — lives in `bash "<story-helper>" <subcommand> …`.
**Route and render — never call `story` yourself for the steps below.**

## Resolve packaged files and helper

Take the absolute directory containing this loaded `SKILL.md` as `<skill-dir>` and resolve
`<plugin-root>` as the normalized absolute path `<skill-dir>/../..`. Load
`<plugin-root>/references/helper-command.md` and follow it to resolve `<story-helper>`.
Substitute absolute paths for every placeholder below and shell-quote them. Never resolve
packaged files from the user's current working directory.

## Steps

### 0. Ensure the storyhook CLI is available

Load `<plugin-root>/references/ensure-cli.md` and follow it. Do not continue until it passes.

### 1. Gather context

Run `bash "<story-helper>" context`, adding ` --full` if the user invoked
this skill with `--full` or asked for detailed context. `ok:false` → show `display`, stop.

`display` is the CLI's own comprehensive project-state document (`story load-context`) —
open stories, states, priorities, relationships, ready work, and (with `--full`) the
critical path, every blocked story, and stories stale past `STORY_STALE_THRESHOLD` — the
window itself is stated in `display`'s own "Stale (…+) stories" heading, never restated
here, so this file can't drift from whatever the script's own default is.

### 2. Present it

Show `display`. Then synthesize a brief summary highlighting what matters most:
- How many stories are ready to work on
- Any blocked stories and what they are waiting for
- The recommended next story to pick up
- Any critical-priority items that need attention

This synthesis is yours to write — the facts it is drawn from all came from step 1, not
from any command you run yourself.
