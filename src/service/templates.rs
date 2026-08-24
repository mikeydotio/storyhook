//! The repository content storyhook scaffolds on request.
//!
//! Three templates, all of them files a user asks for explicitly — by running
//! `story project new` or `story scaffold` — and none of them written by any other
//! command. That distinction is the whole reason they survive a design whose
//! headline rule is that storyhook writes nothing into a repository: a
//! scaffolded instruction file is the output of the command, in the same sense
//! that a generated `Makefile` is the output of a generator.
//!
//! They are held here rather than beside the dispatcher because two callers
//! need them — [`ProjectService::init`](super::project::ProjectService::init)
//! generates `AGENTS.md`, and `story scaffold` prints whichever one it is
//! asked for — and because their bytes are a user-visible contract that the
//! differential harness compares against the legacy path verbatim.

/// `AGENTS.md`: how an agent is expected to drive this project's tracker.
///
/// `prefix` names the project's story-id prefix and `done_state` its first
/// CLOSED state, so the examples are runnable in the project they describe
/// rather than in a hypothetical one.
#[must_use]
pub fn agents_md(prefix: &str, done_state: &str) -> String {
    format!(
        r#"# AGENTS.md — Project Task Management

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
3. **Claim it**: `story move {prefix}-<n> in-progress`
4. **Record progress as you go**: `story comment {prefix}-<n> "what changed and why"`
5. **Finish**: `story move {prefix}-<n> {done_state} "summary of what was delivered"`
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
story prioritize {prefix}-1 critical
story prioritize {prefix}-4 high
story prioritize {prefix}-6 medium
```

**Read `story help priority-rubric` before choosing a level.** Priority is
`story next`'s sort key, not a label — it decides what the next session picks
up, ties break toward the older story, and every inflated level costs the
resolution of the level it joins. A story created with no `--priority` defaults
to `low`; one with no `--type` uses the project's first configured type.

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
story relate {prefix}-1 parent-of {prefix}-2
story relate {prefix}-2 blocks {prefix}-3
story relate {prefix}-5 relates-to {prefix}-2
story relate {prefix}-6 obviates {prefix}-7
```

### Dependency graph

Visualize relationships and find bottlenecks:

```
story graph                           # full dependency overview
story graph --blocked-by {prefix}-1   # trace why a story is blocked
```

## During execution

- Before starting: `story move {prefix}-<n> in-progress`
- When blocked by another story: `story block {prefix}-<n> --on {prefix}-<blocker> "reason"`
  — records a real `blocked-by` edge, which clears itself when the blocker
  closes. A reason alone (no `--on`) is free text that never clears itself;
  use it only when the blocker genuinely isn't a story.
- When unblocked: `story unblock {prefix}-<n>` (or `--on {prefix}-<blocker>`
  to clear just that edge)
- When done: `story move {prefix}-<n> {done_state} "what was delivered"`
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
| Show a story | `story show {prefix}-<n>` |
| Create a story | `story new "<title>"` |
| Move to a state | `story move {prefix}-<n> <state>` |
| Add a comment | `story comment {prefix}-<n> "comment text"` |
| Set priority | `story prioritize {prefix}-<n> high` |
| What a level means | `story help priority-rubric` |
| Adopt or file a mid-work find | `story help scope-rubric` |
| Assign a story | `story assign {prefix}-<n> <member>` |
| Add a label | `story label {prefix}-<n> <label>` |
| Block on another story | `story block {prefix}-<n> --on {prefix}-<blocker> "reason"` |
| Unblock a story | `story unblock {prefix}-<n>` |
| Add a relationship | `story relate {prefix}-1 blocks {prefix}-2` |
| Set several fields | `story set {prefix}-<n> --priority high --state in-progress` |
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
"#,
        done_state = done_state,
        prefix = prefix,
    )
}

/// The `## Storyhook` section a project's own `CLAUDE.md` gets.
#[must_use]
pub fn claude_md() -> String {
    r#"## Storyhook

This project uses **storyhook** for task tracking. Full usage instructions are in `AGENTS.md` — read that file before starting work.

Quick start: run `story load-context` at session start, `story next` to pick a task.

Run `story help <command>` for detailed usage on any command, or `story help --compact` for the full reference.
"#
    .to_string()
}

/// `.cursorrules`: the same instructions in Cursor's format.
#[must_use]
pub fn cursor_rules() -> String {
    r#"# Cursor Rules — storyhook Integration

This project uses **storyhook** as its issue tracker. Use the storyhook CLI
to manage tasks.

## Task Management

- Run `story load-context` at the start of each session to understand project state.
- Run `story next` to find the highest-priority ready task.
- After completing work, mark the story done: `story move <id> done`.
- Use `story handoff --since 2h` to summarize work at session end.
- Found a second problem while working? Prefer adopting it into the story you
  are on over filing a new one — run `story help scope-rubric` before you file.

## Commands

- `story list` — list open stories
- `story new "<title>"` — create a new story
- `story show <id>` — show story details
- `story comment <id> "text"` — add a comment
- `story move <id> <state>` — change story state
- `story prioritize <id> <level>` — set priority (critical, high, medium, low);
  run `story help priority-rubric` for what each level means before choosing one
- `story assign <id> <member>` — assign a story
- `story label <id> <label>` — add a label
- `story block <id> "reason"` — mark story as blocked
- `story unblock <id>` — clear blocked status
- `story relate <a> <rel> <b>` — add a relationship
- `story set <id> --field value` — update multiple fields at once
- `story search "<query>"` — search stories
- `story summary` — project overview
- `story load-context` — full project context for LLM consumption
- `story phase list` — phase progress overview
- `story handoff --since <duration>` — recent changes summary

Run `story help <command>` for detailed usage on any command.
"#
    .to_string()
}
