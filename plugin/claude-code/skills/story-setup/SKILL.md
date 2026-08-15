---
name: story-setup
description: "Use when setting up storyhook in a new project, when the 'story' command is not found, or when asked to configure storyhook integration. Detects CLI availability, installs if missing, initializes project, and configures plugin behavior."
user-invocable: true
allowed-tools: Read, Write, Bash(command -v *), Bash(story *), Bash(cargo *), Bash(curl *), Bash(bash ${CLAUDE_PLUGIN_ROOT}/bin/story.sh *), AskUserQuestion
---

# Storyhook Setup

## Steps

### 1. Check CLI availability

Follow `${CLAUDE_PLUGIN_ROOT}/references/ensure-cli.md`. Do not continue until it passes.

### 2. Initialize the project

Check whether `.storyhook.toml` exists at the repo root. That committed pointer file is what
says which project this checkout belongs to; story data itself lives in storyhook's store,
outside the repository.

- **If missing**, run `story project new --prefix <PREFIX>` to create the project and write the pointer
  - `--prefix` is **required**: you have no terminal, so the command will not ask and will not default it. Ask the user which prefix they want (e.g. `story project new --prefix API`, giving story IDs `API-1`, `API-2`, …) before running it.
  - If `story project new` reports that this repository still keeps its stories in a `.storyhook/`
    directory, it has not been migrated. Run `story migrate --dry-run` to show what would be
    imported, then `story migrate`. It never writes to the directory it reads.
- **If present**, confirm the project is already initialized and show current config with `story summary`

### 3. Configure plugin behavior

Add a `[plugin]` table to `.storyhook.toml`, alongside the identity keys `story project new` wrote.
storyhook reads this table and never rewrites it, so it is safe to edit by hand:

```toml
[plugin]
# Set to false to turn the session hooks off entirely.
enabled = true

# Tracking verbosity: "quiet", "normal", or "verbose"
# quiet   = minimal output, no auto-comments
# normal  = status updates and session handoffs
# verbose = detailed logging with progress comments
tracking = "normal"
```

Ask the user if they want to adjust the tracking level.

### 4. Agent instructions (optional)

Ask if the user wants storyhook instructions added to their CLAUDE.md file.

If yes, run `bash ${CLAUDE_PLUGIN_ROOT}/bin/story.sh scaffold-claude-md`. `ok:false` → show
`display`, stop. `ok:true` → show `display`; `action` names what happened (`created`,
`appended`, or `replaced` — the helper already finds any existing `<!-- BEGIN STORYHOOK
-->` block and replaces it in place, byte-preserving everything else in the file). Do not
edit CLAUDE.md yourself for this step.

### 5. Git hooks (optional)

Ask if the user wants git hooks installed for automatic story syncing.

- If yes, run `story hooks install`
- This sets up post-commit hooks that automatically link commits to stories when commit messages mention story IDs

### 6. Next steps

Suggest running `/story-context` to see the current project state and start working.
