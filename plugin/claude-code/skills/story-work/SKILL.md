---
name: story-work
description: "Use when starting work on a specific story or picking the next available task. Transitions the story to in-progress, shows full context, and sets up the working session. Call with a story ID to work on a specific story, or with no arguments to auto-pick the next ready story."
user-invocable: true
allowed-tools: Bash(story *), Bash(command -v *), AskUserQuestion
argument-hint: "[STORY-ID]"
---

# Storyhook Work

Start working on a story.

## Steps

### 0. Ensure the storyhook CLI is available

Before running any `story` command, confirm the CLI is installed by running `command -v story`. If it is missing, follow `references/ensure-cli.md`: tell the user, ask permission to install (via `AskUserQuestion`), and if approved use the `story-install` skill before continuing. Do not run `story` commands until this check passes.

### 1. Select a story

- **If a story ID was provided** (e.g., `/story-work SH-3`), use that story
- **If no story ID**, run `story next --json` to automatically pick the highest-priority ready story. The response is double-nested: a single ready story is at `.story.story.id` (NOT `.story.id`). If nothing is ready, there is no `.story` key at all — check `.message` for `"no ready stories"`.
- If nothing is ready, inform the user that all stories are either done or blocked, and suggest running `/story-triage`

### 2. Show story details

Run `story show <id> --json` to get the full story details including:
- Title, priority, labels
- Current state and any blocked status
- Comments and history
- Relationships (blockers, parent/child, related stories)

The story fields are under `.story.story` (e.g. `.story.story.state`), not `.story` directly.

Present this information clearly so the context is understood before starting work.

### 3. Read tracking mode

Check `.storyhook/plugin-config.toml` for the `tracking` setting:
- `quiet` -- skip progress comments, only update state
- `normal` -- add a start comment and update state
- `verbose` -- add detailed progress comments throughout

Default to `normal` if the config file does not exist or the key is missing.

### 4. Transition to in-progress

Run `story move <id> in-progress` to mark the story as active.

If the project uses custom states (check `.storyhook/states.toml` for a state with `role = "active"`), use that state slug instead of `in-progress`. If there is no active-role state and `in-progress` does not exist, inform the user and ask which state to use.

### 5. Add start comment

If tracking mode is `normal` or `verbose`:

Run `story comment <id> "Starting work on this story"` to log the session start.

### 6. Present working context

Summarize what the story is about and what needs to be done. If the story has child stories, list them. If it has dependencies that are already done, note what was completed. The agent should now proceed with the actual implementation work.
