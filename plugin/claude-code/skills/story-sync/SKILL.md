---
name: story-sync
description: "Use after git operations (merge, rebase, pull) to synchronize git commit history with storyhook stories. Links commits referencing story IDs to their stories and auto-closes stories mentioned in merge commits."
user-invocable: true
allowed-tools: Bash(story *), Bash(command -v *), Bash(bash ${CLAUDE_PLUGIN_ROOT}/bin/story.sh *), AskUserQuestion
argument-hint: "[--since <duration>]"
---

# Storyhook Sync

You are a **thin router**. Deterministic work lives in
`bash ${CLAUDE_PLUGIN_ROOT}/bin/story.sh <subcommand> …`. **Route and render — never call
`story` yourself for the steps below.**

## Steps

### 0. Ensure the storyhook CLI is available

Follow `${CLAUDE_PLUGIN_ROOT}/references/ensure-cli.md`. Do not continue until it passes.

### 1. Run git sync

Run `bash ${CLAUDE_PLUGIN_ROOT}/bin/story.sh sync`, adding ` --since <duration>` only if
the user gave one (e.g., `/story-sync --since 30d`) — pass nothing and the CLI's own
default window applies; never restate a number here. `ok:false` → show `display`, stop.

The underlying command:
- Scans git commit messages for story ID references (e.g., `SH-3`, `API-12`)
- Links matching commits to their stories as comments
- Auto-transitions stories mentioned in merge commits (e.g., a commit message like `Merge: closes SH-3` will mark SH-3 as done)

### 2. Report results

Show `display` — it already reports what was scanned and linked. If nothing was synced,
it says so; the stories are already up to date with the git history.
