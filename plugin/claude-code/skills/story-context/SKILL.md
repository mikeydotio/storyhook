---
name: story-context
description: "Use at session start or whenever you need to understand project state -- open stories, priorities, blockers, and next recommended task. Shows project overview and surfaces the most important work."
user-invocable: true
allowed-tools: Bash(story *), Bash(command -v *), Bash(bash ${CLAUDE_PLUGIN_ROOT}/bin/story.sh *), AskUserQuestion
argument-hint: "[--full]"
---

# Storyhook Context

You are a **thin router**. Deterministic work — the CLI-availability check and every read
this skill needs — lives in `bash ${CLAUDE_PLUGIN_ROOT}/bin/story.sh <subcommand> …`.
**Route and render — never call `story` yourself for the steps below.**

## Steps

### 0. Ensure the storyhook CLI is available

Follow `${CLAUDE_PLUGIN_ROOT}/references/ensure-cli.md`. Do not continue until it passes.

### 1. Gather context

Run `bash ${CLAUDE_PLUGIN_ROOT}/bin/story.sh context`, adding ` --full` if the user invoked
this skill with `--full` or asked for detailed context. `ok:false` → show `display`, stop.

`display` is the CLI's own comprehensive project-state document (`story load-context`) —
open stories, states, priorities, relationships, ready work, and (with `--full`) the
critical path, every blocked story, and stories stale past `STORY_STALE_THRESHOLD`
(default `3d`).

### 2. Present it

Show `display`. Then synthesize a brief summary highlighting what matters most:
- How many stories are ready to work on
- Any blocked stories and what they are waiting for
- The recommended next story to pick up
- Any critical-priority items that need attention

This synthesis is yours to write — the facts it is drawn from all came from step 1, not
from any command you run yourself.
