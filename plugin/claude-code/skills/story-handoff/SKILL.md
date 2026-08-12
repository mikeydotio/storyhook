---
name: story-handoff
description: "Use when ending a work session or switching contexts. Generates a comprehensive handoff document showing what was accomplished, what changed, and what remains. Also runs automatically when sessions end via the stop hook."
user-invocable: true
allowed-tools: Bash(story *), Bash(command -v *), AskUserQuestion
argument-hint: "[--since <duration>]"
---

# Storyhook Handoff

Generate a session handoff document for continuity across sessions.

## Steps

### 0. Ensure the storyhook CLI is available

Before running any `story` command, confirm the CLI is installed by running `command -v story`. If it is missing, follow `${CLAUDE_PLUGIN_ROOT}/references/ensure-cli.md`: tell the user, ask permission to install (via `AskUserQuestion`), and if approved use the `story-install` skill before continuing. Do not run `story` commands until this check passes.

### 1. Generate handoff

Run `story handoff --since <duration>` where duration defaults to `2h`. If the user provided a different duration (e.g., `/story-handoff --since 4h`), use that value.

The handoff command analyzes:
- Stories that changed state during the time window
- Comments added during the session
- Git commits that reference story IDs
- Current blockers and open items

### 2. Get current state

Run `story summary --json` to capture the current project snapshot:
- Total open vs. closed stories
- Stories in each state
- How many are ready vs. blocked

### 3. Present the handoff

Synthesize a clear handoff that includes:
- **What was accomplished**: stories completed or progressed during this session
- **Current state**: what is in-progress, what is blocked, what is ready
- **What's next**: the recommended next story from `story next`
- **Blockers**: anything that needs attention or external input

This information helps the next session (whether the same agent or a different one) pick up exactly where this session left off.

Note: This skill also runs automatically via the Stop hook when a session ends. The automatic version uses a 4-hour window. When invoked manually, the user can customize the time window.
