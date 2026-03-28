---
name: storyhook-work
description: "Use when starting work on a specific story or picking the next available task. Transitions the story to in-progress, shows full context, and sets up the working session. Call with a story ID to work on a specific story, or with no arguments to auto-pick the next ready story."
user-invocable: true
allowed-tools: Bash(story *), AskUserQuestion
argument-hint: "[STORY-ID]"
---

# Storyhook Work

Start working on a story.

## Steps

### 1. Select a story

- **If a story ID was provided** (e.g., `/storyhook:work SH-3`), use that story
- **If no story ID**, run `story next --json` to automatically pick the highest-priority ready story
- If `next` returns nothing, inform the user that all stories are either done or blocked, and suggest running `/storyhook:triage`

### 2. Show story details

Run `story <id> --json` to get the full story details including:
- Title, priority, labels
- Current state and any awaiting status
- Comments and history
- Relationships (blockers, parent/child, related stories)

Present this information clearly so the context is understood before starting work.

### 3. Read tracking mode

Check `.storyhook/plugin-config.toml` for the `tracking` setting:
- `quiet` -- skip progress comments, only update state
- `normal` -- add a start comment and update state
- `verbose` -- add detailed progress comments throughout

Default to `normal` if the config file does not exist or the key is missing.

### 4. Transition to in-progress

Run `story <id> is in-progress` to mark the story as active.

If the project uses custom states (check `.storyhook/states.toml` for a state with `role = "active"`), use that state slug instead of `in-progress`. If there is no active-role state and `in-progress` does not exist, inform the user and ask which state to use.

### 5. Add start comment

If tracking mode is `normal` or `verbose`:

Run `story <id> "Starting work on this story"` to log the session start.

### 6. Present working context

Summarize what the story is about and what needs to be done. If the story has child stories, list them. If it has dependencies that are already done, note what was completed. The agent should now proceed with the actual implementation work.
