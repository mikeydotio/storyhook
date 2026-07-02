---
name: storyhook-setup
description: "Use when setting up storyhook in a new project, when the 'story' command is not found, or when asked to configure storyhook integration. Detects CLI availability, installs if missing, initializes project, and configures plugin behavior."
user-invocable: true
allowed-tools: Read, Write, Bash(command -v *), Bash(story *), Bash(cargo *), Bash(curl *), AskUserQuestion
---

# Storyhook Setup

Set up storyhook CLI and project configuration.

## Steps

### 1. Check CLI availability

Run `command -v story` to see if the CLI is installed.

- **If missing**, follow the `storyhook-install` skill to install and verify the `story` CLI. It asks the user's permission and offers the official installer or cargo. Do not continue until `story --help` works.
- **If present**, continue to the next step.

### 2. Initialize the project

Check if `.storyhook/` directory exists.

- **If missing**, run `story init` to create it
  - Ask the user if they want a custom prefix (e.g., `story init --prefix API`). The prefix is used for story IDs like `API-1`, `API-2`, etc.
- **If present**, confirm the project is already initialized and show current config with `story summary`

### 3. Configure plugin behavior

Create `.storyhook/plugin-config.toml` with these defaults:

```toml
# Storyhook Claude Code plugin configuration
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

- If yes, run `story scaffold claude-md` to generate the instructions block
- Wrap the output in sentinel markers when appending to CLAUDE.md:
  ```
  <!-- BEGIN STORYHOOK -->
  (scaffold output here)
  <!-- END STORYHOOK -->
  ```
- If CLAUDE.md already contains `<!-- BEGIN STORYHOOK -->`, replace the existing block

### 5. Git hooks (optional)

Ask if the user wants git hooks installed for automatic story syncing.

- If yes, run `story hooks install`
- This sets up post-commit hooks that automatically link commits to stories when commit messages mention story IDs

### 6. Next steps

Suggest running `/storyhook:storyhook-context` to see the current project state and start working.
