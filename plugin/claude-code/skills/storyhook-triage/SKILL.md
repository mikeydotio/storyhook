---
name: storyhook-triage
description: "Use when the project backlog needs review -- stories need prioritization, stale items need attention, or work needs reorganization. Reviews all open stories, identifies issues, and guides reprioritization."
user-invocable: true
allowed-tools: Bash(story *), Bash(command -v *), AskUserQuestion
---

# Storyhook Triage

Review and organize the project backlog.

## Steps

### 0. Ensure the storyhook CLI is available

Before running any `story` command, confirm the CLI is installed by running `command -v story`. If it is missing, follow `references/ensure-cli.md`: tell the user, ask permission to install (via `AskUserQuestion`), and if approved use the `storyhook-install` skill before continuing. Do not run `story` commands until this check passes.

### 1. Gather project state

Run these commands to get a complete picture:

- `story list --json` -- all open stories with full details
- `story list --stale 3d --json` -- stories not updated in 3+ days
- `story list --blocked --json` -- stories waiting on something
- `story graph --json` -- dependency relationships

Each story in `list`'s `.stories[]` array is double-nested: fields are at `.stories[].story.*` (e.g. `.stories[].story.priority`, `.stories[].story.awaiting`), not `.stories[].*` directly.

### 2. Identify issues

Analyze the gathered data and flag:

- **Stale stories**: open stories with no activity in 3+ days. These may need to be reprioritized, unblocked, or closed.
- **Blocked stories**: stories with a non-null `awaiting` field (set by `story block`). Check if the blocker is still valid or can be cleared with `story unblock`.
- **Unprioritized stories**: stories with `priority: none`. Every open story should have a priority to ensure `story next` gives good recommendations.
- **Orphan stories**: stories with no relationships that might be missing dependencies or could be grouped under a parent.
- **Dependency cycles**: eyeball `story graph`'s output for `blocked-by` chains that loop back on themselves — the CLI does not automatically detect or flag cycles, so this is a manual check.

### 3. Present findings

Show the user a summary of all issues found, organized by severity:
1. Blocked stories (may be stopping progress)
2. Stale stories (may indicate forgotten work)
3. Unprioritized stories (affects task selection)
4. Structural issues (cycles, orphans)

### 4. Interactive resolution

For each issue, ask the user what to do and execute their decision:

- **Reprioritize**: `story prioritize <id> <critical|high|medium|low|none>`
- **Add labels**: `story label <id> <labels-csv>`
- **Remove labels**: `story unlabel <id> <labels-csv>`
- **Clear blockers**: `story unblock <id>`
- **Set new blockers**: `story block <id> "<reason>"`
- **Close stale stories**: `story move <id> done "Closed during triage -- no longer needed"`
- **Add relationships**: `story relate <a> <relationship> <b>` — the only valid relationships are `blocks`, `blocked-by`, `parent-of`, `child-of`, `relates-to`, `duplicate-of`, `obviates`, `obviated-by` (use `blocks`/`blocked-by` for execution-order dependencies, not `relates-to`)
- **Remove relationships**: `story unrelate <a> <relationship> <b>`

### 5. Verify

After all changes, run `story summary` to show the updated project state and confirm the backlog is in good shape.
