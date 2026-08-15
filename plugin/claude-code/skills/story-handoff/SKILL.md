---
name: story-handoff
description: "Use when ending a work session or switching contexts. Generates a comprehensive handoff document showing what was accomplished, what changed, and what remains. Also runs automatically when sessions end via the stop hook."
user-invocable: true
allowed-tools: Bash(story *), Bash(command -v *), Bash(bash ${CLAUDE_PLUGIN_ROOT}/bin/story.sh *), AskUserQuestion
argument-hint: "[--since <duration>]"
---

# Storyhook Handoff

You are a **thin router**. Deterministic work lives in
`bash ${CLAUDE_PLUGIN_ROOT}/bin/story.sh <subcommand> …`. **Route and render — never call
`story` yourself for the steps below.**

## Steps

### 0. Ensure the storyhook CLI is available

Follow `${CLAUDE_PLUGIN_ROOT}/references/ensure-cli.md`. Do not continue until it passes.

### 1. Generate the handoff

Run `bash ${CLAUDE_PLUGIN_ROOT}/bin/story.sh handoff`, adding ` --since <duration>` only if
the user gave one (e.g., `/story-handoff --since 4h`) — pass nothing and the CLI's own
default window applies; never restate a number here (a hand-copied default is exactly what
went stale before: this file used to say `2h` after the CLI's real default had been `24h`
for a full release, because the fix that corrected it never touched this file). `ok:false`
→ show `display`, stop.

The result carries two things:
- `display` — the handoff document itself: stories that changed state during the window,
  comments added, git commits that reference story IDs, and current blockers.
- `summary` — a current-state snapshot (open vs. closed counts, stories per state, ready
  vs. blocked) for step 2 below.

### 2. Present the handoff

Show `display`. Then synthesize a clear summary that includes:
- **What was accomplished**: stories completed or progressed during this session
- **Current state**: what is in-progress, what is blocked, what is ready (from `summary`)
- **What's next**: the recommended next story — if you don't already know it, ask
- **Blockers**: anything that needs attention or external input

This information helps the next session (whether the same agent or a different one) pick
up exactly where this session left off.

Note: This skill also runs automatically via the Stop hook (`hooks/stop-handoff.sh`), which
calls `story handoff --since 4h` directly rather than through this skill — see that script
for its own window. When invoked manually here, the user can customize the time window via
step 1's `--since`.
