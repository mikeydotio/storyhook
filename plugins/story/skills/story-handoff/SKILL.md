---
name: story-handoff
description: "Use when ending a work session or switching contexts. Generates a comprehensive handoff document showing what was accomplished, what changed, and what remains."
---

# Storyhook Handoff

You are a **thin router**. Deterministic work lives in
`bash "<story-helper>" <subcommand> …`. **Route and render — never call
`story` yourself for the steps below.**

## Resolve packaged files and helper

Take the absolute directory containing this loaded `SKILL.md` as `<skill-dir>` and resolve
`<plugin-root>` as the normalized absolute path `<skill-dir>/../..`. Load
`<plugin-root>/references/helper-command.md` and follow it to resolve `<story-helper>`.
Substitute absolute paths for every placeholder below and shell-quote them. Never resolve
packaged files from the user's current working directory.

## Steps

### 0. Ensure the storyhook CLI is available

Load `<plugin-root>/references/ensure-cli.md` and follow it. Do not continue until it passes.

### 1. Generate the handoff

Run `bash "<story-helper>" handoff`, adding ` --since <duration>` only if
the user gave one — pass nothing and the CLI's own
default window applies; never restate a number here. A hand-copied default is exactly what
went stale before: this file once quoted a default that had gone wrong by 12x, because the
commit that fixed the real value elsewhere never touched this file (`git show 04ac259` in
this repo has the full story). `ok:false` → show `display`, stop.

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

Provider integrations may also generate handoffs at session end. When this skill is invoked
directly, the user can customize the time window via step 1's `--since`.
