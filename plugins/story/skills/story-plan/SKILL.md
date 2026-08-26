---
name: story-plan
description: "Use when breaking down a feature, spec, or requirement into structured stories with dependencies and priorities. Accepts a file path (markdown or YAML spec) or inline description. Creates stories in storyhook with relationships."
---

# Storyhook Plan

Decompose a feature or spec into structured stories with dependencies.

## Resolve packaged files and helper

Take the absolute directory containing this loaded `SKILL.md` as `<skill-dir>`. Resolve
`<plugin-root>` as the normalized absolute path `<skill-dir>/../..`. Load
`<plugin-root>/references/helper-command.md` and follow it to resolve `<story-helper>` for
the CLI guard below. Substitute absolute, shell-quoted paths for packaged-file placeholders.
Never resolve packaged files from the user's current working directory.

## Steps

### 0. Ensure the storyhook CLI is available

Load `<plugin-root>/references/ensure-cli.md` and follow it. Do not continue until it passes.

### 1. Identify input

- **If a file path is provided**, read the file to verify it exists
- **If an inline description is provided**, write it to a markdown file — **use the session scratchpad directory if one exists, otherwise `mktemp`** — never a fixed shared path: two concurrent planning sessions could silently overwrite each other's input
- **If no argument**, ask the user to describe what they want to plan or provide a spec file

### 2. Preview the decomposition

Run `story decompose <file> --dry-run` to preview what stories will be created without actually creating them.

This shows:
- Proposed story titles
- Suggested priorities and labels
- Dependency relationships between stories

Present the preview to the user for review.

### 3. Get approval

Ask the user if the proposed stories look correct. They may want to:
- Adjust titles or priorities
- Add or remove stories
- Change dependency relationships

If adjustments are needed, edit the input file and re-run `--dry-run` until the user is satisfied.

### 4. Create the stories

On approval, run `story decompose <file>` (without `--dry-run`) to create all stories with their relationships and priorities.

### 5. Show the dependency graph

Run `story graph --critical-path` to display the dependency structure and identify the critical path through the planned work.

### 6. Suggest next steps

- If stories are ready to work on, suggest the router's `claim <id>` route to start on the first story in the critical path
- If the plan needs further refinement, suggest the `story-triage` skill to review and adjust priorities
