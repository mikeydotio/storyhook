# Storyhook Workflow Patterns

Common workflow patterns for using storyhook with AI coding agents. Each pattern includes the purpose, when to use it, and concrete command sequences.

---

## 1. Session Lifecycle

The standard pattern for every coding session: understand context, pick work, execute, and hand off.

**When to use**: Every session. This is the default workflow.

```bash
# 1. Understand current state
story context

# 2. Find the best task to work on
story next --count 3

# 3. Start working on a story
story SH-3 is in-progress
story SH-3 "Starting work on this story"

# 4. Log progress as you go
story SH-3 "Implemented the database migration"
story SH-3 "Added unit tests for the new schema"

# 5. Complete the story
story SH-3 is done "Feature complete with tests"

# 6. Hand off at session end
story handoff --since 2h
```

**Key points**:
- Always run `story context` at session start -- it prevents duplicate work and surfaces blockers early
- Use `story next` instead of manually picking -- it respects priorities and dependencies
- Add comments as you work so the next session has context
- The stop hook automatically generates a handoff, but you can also do it manually

---

## 2. Planning Workflow

Break down a feature or spec into structured stories before starting implementation.

**When to use**: When you receive a new feature request, spec document, or project requirement.

```bash
# 1. Decompose a spec into stories (preview first)
story decompose feature-spec.md --dry-run

# 2. Create the stories
story decompose feature-spec.md

# 3. Review and adjust priorities
story SH-10 priority critical
story SH-11 priority high
story SH-12 priority medium

# 4. Add labels for categorization
story SH-10 label backend,database
story SH-11 label backend,api
story SH-12 label frontend

# 5. View the dependency graph
story graph --critical-path

# 6. Triage any issues
story list --ready

# 7. Start working on the first story
story next
```

**Key points**:
- Always use `--dry-run` first to review before creating stories
- Set priorities on all stories so `story next` gives good recommendations
- Use `story graph --critical-path` to identify which stories unblock the most work
- Label stories to make filtering easy later

---

## 3. Multi-Story Workflow

Work through multiple related stories in dependency order, maximizing parallel work where possible.

**When to use**: When a feature has been decomposed into several stories with dependencies.

```bash
# 1. View the dependency structure
story graph

# 2. Find the critical path (longest chain)
story graph --critical-path

# 3. Find stories that can be done in parallel
story graph --parallel-groups

# 4. Work on first parallel group
story SH-1 is in-progress
# ... do the work ...
story SH-1 is done "Database schema created"

story SH-2 is in-progress
# ... do the work ...
story SH-2 is done "Config module implemented"

# 5. Check what's unblocked now
story next --count 5

# 6. Continue with the next group
story SH-3 is in-progress
# ... do the work ...
story SH-3 is done "API endpoints implemented"

# 7. Verify progress
story summary
```

**Key points**:
- `--parallel-groups` shows which stories have no dependencies between them and can be done in any order
- After completing a story, check `story next` -- previously blocked stories may now be ready
- Use `story summary` periodically to see overall progress

---

## 4. Recovery Patterns

Fix project health issues: stale stories, broken dependencies, lost context.

**When to use**: When the project backlog is messy, stories are stale, or something seems wrong.

```bash
# 1. Diagnose project health
story doctor

# 2. Auto-fix what can be fixed
story doctor --fix

# 3. Find stale stories (no activity in 7 days)
story list --stale 7d

# 4. Decide on each stale story
story SH-5 is done "No longer needed after architecture change"
story SH-6 priority low
story SH-7 "Still relevant, updating priority"
story SH-7 priority high

# 5. Check blocked stories
story list --blocked

# 6. Clear resolved blockers
story SH-8 awaits --clear
story SH-8 is in-progress

# 7. Re-prioritize
story SH-9 priority critical
story SH-10 priority medium

# 8. Verify the backlog is clean
story summary
story next --count 5
```

**Key points**:
- Run `story doctor` first -- it catches structural issues like orphaned files or invalid states
- Be aggressive about closing stale stories -- if nobody has touched it in a week, it may not be needed
- After cleanup, verify `story next` gives sensible recommendations

---

## 5. Team Workflow

Manage work across team members with assignments and handoffs.

**When to use**: When multiple people (or agents) are working on the same project.

```bash
# 1. Register team members
story member add "Alice Smith <alice@example.com>"
story member add -g bob-dev

# 2. Assign stories
story SH-1 assign alice
story SH-2 assign alice
story SH-3 assign bob-dev

# 3. View assignments
story list --assignee alice
story list --assignee bob-dev

# 4. Track progress
story SH-1 is in-progress
story SH-1 "Working on database schema"

# 5. Hand off between sessions
story handoff --since 4h

# 6. Next session picks up context
story context
story list --assignee alice --state in-progress
```

**Key points**:
- Use `story member add -g <handle>` for GitHub users (fetches name/email automatically)
- Assign all in-progress work so it is clear who owns what
- Use `story handoff` at session boundaries for continuity

---

## 6. Blocker Management

Track and resolve dependencies and external blockers.

**When to use**: When work is stuck waiting on something.

```bash
# 1. Mark a story as blocked
story SH-3 awaits "Need API spec from design team"

# 2. Set up dependency relationships
story SH-3 follows SH-1
story SH-4 follows SH-3

# 3. See what's blocked and why
story list --blocked
story graph --blocked-by SH-1

# 4. When the blocker is resolved
story SH-3 awaits --clear
story SH-3 is in-progress

# 5. Check what else is now unblocked
story next --count 5
```

**Key points**:
- Use `awaits` for external blockers (waiting on a person, a decision, an API)
- Use `follows`/`precedes` for internal dependencies (story A must finish before story B)
- `story graph --blocked-by SH-1` shows the full transitive impact of a single blocker

---

## 7. Git Integration

Keep stories synchronized with git activity.

**When to use**: After merges, pulls, or rebases, or when commit messages reference story IDs.

```bash
# 1. Sync recent git history
story sync-git --since 7d

# 2. Install git hooks for automatic syncing
story hooks install

# 3. Verify hooks are active
story hooks list

# 4. Commit messages that reference stories will auto-link
# Example commit message: "Add login endpoint for SH-3"
git commit -m "Add login endpoint for SH-3"

# 5. Merge commits can auto-close stories
# Example: "Merge: closes SH-3"
git merge feature-branch

# 6. Check sync results
story SH-3
```

**Key points**:
- Reference story IDs in commit messages (e.g., `SH-3`) to automatically link commits
- `story hooks install` sets up post-commit hooks for real-time syncing
- Use `story sync-git` for bulk sync after pulling or rebasing
- The plugin's post-git hook also runs sync automatically after detected git operations
