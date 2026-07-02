# Storyhook Workflow Patterns

Common workflow patterns for using storyhook with AI coding agents. Each pattern includes the purpose, when to use it, and concrete command sequences. All commands below use the real verb-first CLI — see `cli-reference.md` for the full syntax and JSON shapes of every command used here.

---

## 1. Session Lifecycle

The standard pattern for every coding session: understand context, pick work, execute, and hand off.

**When to use**: Every session. This is the default workflow.

```bash
# 1. Understand current state
story load-context

# 2. Find the best task to work on
story next --count 3

# 3. Start working on a story
story move SH-3 in-progress
story comment SH-3 "Starting work on this story"

# 4. Log progress as you go
story comment SH-3 "Implemented the database migration"
story comment SH-3 "Added unit tests for the new schema"

# 5. Complete the story
story move SH-3 done "Feature complete with tests"

# 6. Hand off at session end
story handoff --since 2h
```

**Key points**:
- Always run `story load-context` at session start -- it prevents duplicate work and surfaces blockers early
- Use `story next` instead of manually picking -- it respects priorities and `blocks`/`blocked-by` dependencies
- Add comments as you work so the next session has context
- `story move <id> <state> "<comment>"` can log the completion comment in the same call as the transition

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
story prioritize SH-10 critical
story prioritize SH-11 high
story prioritize SH-12 medium

# 4. Add labels for categorization
story label SH-10 backend,database
story label SH-11 backend,api
story label SH-12 frontend

# 5. View the dependency graph
story graph --critical-path

# 6. Triage any issues
story list --ready

# 7. Start working on the first story
story next
```

**Key points**:
- Always use `--dry-run` first to review before creating stories
- `story decompose` parses `### Wave N` headings into `blocked-by` edges automatically -- prefer structuring the spec that way over manually relating every story afterward
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
story move SH-1 in-progress
# ... do the work ...
story move SH-1 done "Database schema created"

story move SH-2 in-progress
# ... do the work ...
story move SH-2 done "Config module implemented"

# 5. Check what's unblocked now
story next --count 5

# 6. Continue with the next group
story move SH-3 in-progress
# ... do the work ...
story move SH-3 done "API endpoints implemented"

# 7. Verify progress
story summary
```

**Key points**:
- `--parallel-groups` shows which stories have no `blocks`/`blocked-by` dependency between them and can be done in any order
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
story move SH-5 done "No longer needed after architecture change"
story prioritize SH-6 low
story comment SH-7 "Still relevant, updating priority"
story prioritize SH-7 high

# 5. Check blocked stories
story list --blocked

# 6. Clear resolved blockers
story unblock SH-8
story move SH-8 in-progress

# 7. Re-prioritize
story prioritize SH-9 critical
story prioritize SH-10 medium

# 8. Verify the backlog is clean
story summary
story next --count 5
```

**Key points**:
- Run `story doctor` first -- it catches structural issues like invalid states or orphaned relationships
- Be aggressive about closing stale stories -- if nobody has touched it in a week, it may not be needed
- `story block`/`story unblock` are for external blockers; a story stuck behind an unfinished dependency is controlled by `blocked-by` relationships instead (see Pattern 6)
- After cleanup, verify `story next` gives sensible recommendations

---

## 5. Team Workflow

Manage work across team members with assignments and handoffs.

**When to use**: When multiple people (or agents) are working on the same project.

```bash
# 1. Register team members
story member add "Alice <alice@example.com>"
story member add -g bob-dev

# 2. Assign stories
story assign SH-1 alice
story assign SH-2 alice
story assign SH-3 bob-dev

# 3. View assignments
story list --assignee alice
story list --assignee bob-dev

# 4. Track progress
story move SH-1 in-progress
story comment SH-1 "Working on database schema"

# 5. Hand off between sessions
story handoff --since 4h

# 6. Next session picks up context
story load-context
story list --assignee alice --state in-progress
```

**Key points**:
- Use `story member add -g <handle>` for GitHub users (fetches name/email automatically)
- A registered member's assignable ID is derived from their name (e.g. `"Bob Dev"` -> `bob-dev`) -- use `story list --json` or `story show <id> --json` to confirm the exact slug if unsure
- Assign all in-progress work so it is clear who owns what
- Use `story handoff` at session boundaries for continuity

---

## 6. Blocker Management

Track and resolve dependencies and external blockers. Storyhook distinguishes two different things that both read as "blocked":

- **External blockers** (`story block`/`story unblock`) -- something outside the graph (a person, a decision, an API) that has nothing to do with another story
- **Internal dependencies** (`blocks`/`blocked-by` relationships) -- one story genuinely cannot start until another story closes

**When to use**: When work is stuck waiting on something.

```bash
# 1. Mark a story as externally blocked
story block SH-3 "Need API spec from design team"

# 2. Set up dependency relationships between stories
story relate SH-1 blocks SH-3
story relate SH-3 blocks SH-4

# 3. See what's blocked and why
story list --blocked
story graph --blocked-by SH-1

# 4. When the external blocker is resolved
story unblock SH-3
story move SH-3 in-progress

# 5. Check what else is now unblocked
story next --count 5
```

**Key points**:
- Use `story block`/`story unblock` for external blockers (waiting on a person, a decision, an API)
- Use `story relate <a> blocks <b>` (equivalently `story relate <b> blocked-by <a>`) for internal dependencies -- `story next`, `story list --ready`, and `story graph` all read `blocks`/`blocked-by` edges to compute readiness
- There is no `precedes`/`follows` in this CLI -- `blocks`/`blocked-by` are the only relations that gate execution order
- `story graph --blocked-by SH-1` shows the full transitive impact of a single blocking story

---

## 7. Git Integration

Keep stories synchronized with git activity.

**When to use**: After merges, pulls, or rebases, or when commit messages reference story IDs.

```bash
# 1. Sync recent git history
story commit-sync --since 7d

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
story show SH-3
```

**Key points**:
- Reference story IDs in commit messages (e.g., `SH-3`) to automatically link commits
- `story hooks install` sets up git hooks (`post-commit`, `post-merge`, `prepare-commit-msg`) for real-time syncing
- Use `story commit-sync` for bulk sync after pulling or rebasing (previously named `sync-git`; that alias still works)
- The plugin's post-git hook also runs sync automatically after detected git operations
