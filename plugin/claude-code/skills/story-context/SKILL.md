---
name: story-context
description: "Use at session start or whenever you need to understand project state -- open stories, priorities, blockers, and next recommended task. Shows project overview and surfaces the most important work."
user-invocable: true
allowed-tools: Bash(story *), Bash(command -v *), AskUserQuestion
---

# Storyhook Context

Get a comprehensive view of current project state.

## Steps

### 0. Ensure the storyhook CLI is available

Before running any `story` command, confirm the CLI is installed by running `command -v story`. If it is missing, follow `${CLAUDE_PLUGIN_ROOT}/references/ensure-cli.md`: tell the user, ask permission to install (via `AskUserQuestion`), and if approved use the `story-install` skill before continuing. Do not run `story` commands until this check passes.

### 1. Project overview

Run `story context` to get the full project overview in markdown format. This includes open stories, their states, priorities, and relationships.

### 2. Next actionable items

Run `story next --count 3 --json` to get the top three recommended stories to work on. The `next` command considers priority, dependencies (blocked stories are excluded), and staleness.

### 3. Synthesize status

Present a brief summary to the user:
- Total open stories and how many are ready to work on
- Any blocked stories and what they are waiting for
- The recommended next story to pick up
- Any critical-priority items that need attention

### 4. Deep dive (if --full is requested)

If the user invoked this skill with `--full` or asked for detailed context:

- Run `story graph --critical-path` to show the longest dependency chain and identify bottleneck stories
- Run `story list --blocked` to enumerate all blocked stories and their blockers
- Run `story list --stale 3d` to find stories that have not been updated in 3 days
- Present all findings together so the user can make an informed decision about what to work on next
