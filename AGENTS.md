# AGENTS.md — Project Task Management

This project uses **storyhook** (the `story` CLI) for work tracking. All agents must
follow the workflow below.

## Session lifecycle

1. **Start of session** — load project context:
   ```
   story load-context
   ```
2. **Pick the next task** — the highest-priority ready story:
   ```
   story next
   ```
3. **Claim it**: `story move SH-<n> in-progress`
4. **Record progress as you go**: `story comment SH-<n> "what changed and why"`
5. **Finish**: `story move SH-<n> done "summary of what was delivered"`
6. **End of session** — generate a handoff summary:
   ```
   story handoff --since 2h
   ```

## Planning

When creating an implementation plan, create a story for each discrete work item,
phase, or issue:

```
story new "Phase 1: Set up database schema"
story new "Phase 2: Implement API endpoints"
story new "Phase 3: Add authentication middleware"
```

Set a priority on each one so `story next` surfaces the right work:

```
story prioritize SH-1 critical
story prioritize SH-4 high
story prioritize SH-6 medium
```

**Read `story help priority-rubric` before choosing a level.** Priority is
`story next`'s sort key, not a label — it decides what the next session picks
up, ties break toward the older story, and every inflated level costs the
resolution of the level it joins. A story created with no `--priority` is
recorded as *nobody has assessed this*, and sorts last — the same place as a
story deliberately parked at `none`, which is a decision somebody made. The two
are stored apart: `story list --unassessed` finds the first kind.

### Decompose a spec

For larger specs, `story decompose` parses a markdown or YAML file into stories
with relationships, priorities and labels:

```
story decompose spec.md --dry-run    # preview without creating anything
story decompose spec.md              # create the stories
cat spec.md | story decompose --stdin
```

### Relationship types

Relationships express dependencies and structure. Both directions are always
recorded, so either end can be asked:

| Relation | Inverse | Purpose |
|---|---|---|
| `blocks` | `blocked-by` | Task dependencies — `story next` respects these |
| `parent-of` | `child-of` | Hierarchy — group subtasks under a parent |
| `relates-to` | `relates-to` | General link between related stories |
| `duplicate-of` | `duplicate-of` | Mark a story as a duplicate |
| `obviates` | `obviated-by` | One story makes another unnecessary |

```
story relate SH-1 parent-of SH-2
story relate SH-2 blocks SH-3
story relate SH-5 relates-to SH-2
story relate SH-6 obviates SH-7
```

### Dependency graph

Visualize relationships and find bottlenecks:

```
story graph                           # full dependency overview
story graph --blocked-by SH-1   # trace why a story is blocked
```

## During execution

- Before starting: `story move SH-<n> in-progress`
- When blocked by another story: `story block SH-<n> --on SH-<blocker> "reason"`
  — records a real `blocked-by` edge, which clears itself when the blocker
  closes. A reason alone (no `--on`) is free text that never clears itself;
  use it only when the blocker genuinely isn't a story.
- When unblocked: `story unblock SH-<n>` (or `--on SH-<blocker>`
  to clear just that edge)
- When done: `story move SH-<n> done "what was delivered"`
- What is ready: `story next --count 5`
- What is blocked: `story list --blocked`

### When you find a second problem

Filing a story for something you already understand, while already holding
the context, throws that context away and pays somebody else to rebuild it.
The default is to adopt it into the story you are already on instead: its
own commit, its own regression test, a comment on the story naming what was
adopted and why. This does not weaken two hats — two hats governs commits,
not stories.

**Read `story help scope-rubric` before filing a story for something you
found mid-work.** It has the test for whether a discovery belongs to your
story, when to fix it now versus adopt it and leave the story open, and
what still gets filed.

## Quick reference

| Action | Command |
|---|---|
| Project overview | `story load-context` |
| Next ready task | `story next` |
| List open stories | `story list` |
| Show a story | `story show SH-<n>` |
| Create a story | `story new "<title>"` |
| Move to a state | `story move SH-<n> <state>` |
| Add a comment | `story comment SH-<n> "comment text"` |
| Set priority | `story prioritize SH-<n> high` |
| What a level means | `story help priority-rubric` |
| Adopt or file a mid-work find | `story help scope-rubric` |
| Assign a story | `story assign SH-<n> <member>` |
| Add a label | `story label SH-<n> <label>` |
| Block on another story | `story block SH-<n> --on SH-<blocker> "reason"` |
| Unblock a story | `story unblock SH-<n>` |
| Add a relationship | `story relate SH-1 blocks SH-2` |
| Set several fields | `story set SH-<n> --priority high --state in-progress` |
| Decompose a spec | `story decompose spec.md` |
| Search stories | `story search "<query>"` |
| Project summary | `story summary` |
| Dependency graph | `story graph` |
| Phase progress | `story phase list` |
| Interactive TUI | `story tui` |
| Session handoff | `story handoff --since 2h` |

Run `story help <command>` for detailed usage on any command, or
`story help --compact` for the full reference.

## Where the data lives

Stories are kept in storyhook's own store, outside this repository — so every
branch, worktree and clone of this project sees one truth, and no ordinary
command writes to the working tree.

The one file that does belong to the repository is `.storyhook.toml`: it names
which project this checkout is, and it is where this repository's own storyhook
configuration lives. **Commit it.** A clone without it does not know which
project it is looking at.
