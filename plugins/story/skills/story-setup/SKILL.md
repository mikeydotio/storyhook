---
name: story-setup
description: "Use when setting up storyhook in a new project, when the 'story' command is not found, or when asked to configure storyhook integration. Detects CLI availability, installs if missing, initializes project, and configures plugin behavior."
---

# Storyhook Setup

## Resolve packaged files and helper

Take the absolute directory containing this loaded `SKILL.md` as `<skill-dir>` and resolve
`<plugin-root>` as the normalized absolute path `<skill-dir>/../..`. Load
`<plugin-root>/references/helper-command.md` and follow it to resolve `<story-helper>`.
Substitute absolute, shell-quoted paths for packaged-file placeholders. Never resolve
packaged files from the user's current working directory.

## Steps

### 1. Check CLI availability

Load `<plugin-root>/references/ensure-cli.md` and follow it. Do not continue until it passes.

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
```

`enabled` is the only key this table defines. Whether a claim posts a comment is a
per-invocation choice — `story claim --comment <text>` or `--no-comment` — not a standing
setting.

### 4. Project agent instructions

`story project new` creates the repository's `AGENTS.md` instructions by default. If the
project was initialized by an older Storyhook version or the user asks to refresh those
instructions, run `bash "<story-helper>" scaffold-agents-md`. The helper owns the canonical
`story scaffold agents-md` output, recognizes an exact scaffold already written by
`story project new`, and merges or replaces only its sentinel-delimited Storyhook block
without overwriting unrelated content. Do not invent or hand-copy a second instruction
template.

### 5. Git integration (optional)

Ask if the user wants git hooks installed for automatic story syncing. Use the host's
structured question mechanism when available.

- If yes, run `story hooks install`
- This sets up post-commit hooks that automatically link commits to stories when commit messages mention story IDs

### 6. Next steps

Suggest the `story-context` skill to see the current project state and start working.
