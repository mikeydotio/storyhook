---
name: story-sync
description: "Use after git operations (merge, rebase, pull) to synchronize git commit history with storyhook stories. Links commits referencing story IDs to their stories and auto-closes stories mentioned in merge commits."
user-invocable: true
allowed-tools: Bash(story *), Bash(command -v *), AskUserQuestion
argument-hint: "[--since <duration>]"
---

# Storyhook Sync

Synchronize git commit history with storyhook stories.

## Steps

### 0. Ensure the storyhook CLI is available

Before running any `story` command, confirm the CLI is installed by running `command -v story`. If it is missing, follow `references/ensure-cli.md`: tell the user, ask permission to install (via `AskUserQuestion`), and if approved use the `story-install` skill before continuing. Do not run `story` commands until this check passes.

### 1. Run git sync

Run `story sync-git --since <duration>` where duration defaults to `7d`. If the user provided a different duration (e.g., `/story-sync --since 30d`), use that value.

The sync command:
- Scans git commit messages for story ID references (e.g., `SH-3`, `API-12`)
- Links matching commits to their stories as comments
- Auto-transitions stories mentioned in merge commits (e.g., a commit message like `Merge: closes SH-3` will mark SH-3 as done)

### 2. Report results

Present what was synced:
- Which stories received new commit references
- Which stories were auto-closed from merge commits
- Any commit references that did not match existing stories (potential typos or deleted stories)

If nothing was synced, let the user know the stories are already up to date with the git history.
