---
name: story-sync
description: "Use after git operations (merge, rebase, pull) to synchronize git commit history with storyhook stories. Links commits referencing story IDs to their stories and auto-closes stories mentioned in merge commits."
---

# Storyhook Sync

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

### 1. Run git sync

Run `bash "<story-helper>" sync`, adding ` --since <duration>` only if
the user gave one — pass nothing and the CLI's own
default window applies; never restate a number here. `ok:false` → show `display`, stop.

The underlying command:
- Scans git commit messages for story ID references (e.g., `SH-3`, `API-12`)
- Links matching commits to their stories as comments
- Auto-transitions stories mentioned in merge commits (e.g., a commit message like `Merge: closes SH-3` will mark SH-3 as done)

### 2. Report results

Show `display` — it already reports what was scanned and linked. If nothing was synced,
it says so; the stories are already up to date with the git history.
