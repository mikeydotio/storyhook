---
name: storyhook-plan
description: "Use when breaking down a feature, spec, or requirement into structured stories with dependencies and priorities. Accepts a file path (markdown or YAML spec) or inline description. Creates stories in storyhook with relationships."
user-invocable: true
allowed-tools: Bash(story *), Read, Write, AskUserQuestion
argument-hint: "[<spec-file> | \"description\"]"
---

# Storyhook Plan

Decompose a feature or spec into structured stories with dependencies.

## Steps

### 1. Identify input

- **If a file path is provided** (e.g., `/storyhook:plan ./spec.md`), read the file to verify it exists
- **If an inline description is provided** (e.g., `/storyhook:plan "Add user authentication with OAuth2"`), write it to a temporary markdown file at `/tmp/storyhook-plan-input.md`
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

- If stories are ready to work on, suggest `/storyhook:work` to start on the first story in the critical path
- If the plan needs further refinement, suggest `/storyhook:triage` to review and adjust priorities
