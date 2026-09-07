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

> Project standards, environment, and git policy live in [CLAUDE.md](./CLAUDE.md) — read it alongside this file.

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
5. **Submit**: run new and directly impacted tests, push one PR, then link it:
   ```
   story link-pr {prefix}-<n> <pr-url>
   ```
6. **Hand off to verification**: record all final context, then make
   `story move {prefix}-<n> verifying` your last action and stop. The daemon
   runs the full suite, merges a green PR, moves the story to `{done_state}`,
   and reaps the lane.

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

### Typed epics are folders

A story is an epic only when its type is `epic`; adding children to an ordinary
story does not change its role. Typed epics organize child stories and carry no
executable steps of their own. Put every implementation step in a child story,
then work and claim those children. The epic's state is computed from them.

### Reserved labels

Two label names mean something to the tooling. Both are ordinary labels
otherwise — add and remove them with `story label` / `story unlabel`.

| Label | Effect |
|---|---|
| `no-auto` | Needs a person in the loop — questions may be asked and a plan approved. `story next` still offers it and it is still claimable by hand; automation skips it. |
| `human-only` | Only a person may do it. `story next` and `story claim --next` never return it. |

`human-only` is **not** a block. The story stays ready everywhere a person
looks: `story list --ready` carries it, every ready count counts it, and an
epic whose only incomplete child is `human-only` does not become blocked.
Anyone can pick it up at any time — it is simply never handed out as an
agent's next assignment.

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
- When submitted: link exactly one open close-on-merge PR, then move the story
  to `verifying` as your final action. Do not run the full suite, merge, close,
  or reap from an agent worktree.
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
| Link the submitted PR | `story link-pr {prefix}-<n> <pr-url>` |
| Set priority | `story prioritize {prefix}-<n> high` |
| What a level means | `story help priority-rubric` |
| Adopt or file a mid-work find | `story help scope-rubric` |
| Assign a story | `story assign {prefix}-<n> <member>` |
| Add a label | `story label {prefix}-<n> <label>` |
| Reserved label names | `story help label` |
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

## Mini-roadmap

- Complete: SH-584 dispatch/Astra and verifier-remediation fixes merged (#668).
- Complete: SH-585 installed-launcher reader exception merged (#669).
- Complete: SH-581 extracted-source Lima guest build fix merged (#670).
- Complete: SH-579 release tag/preflight observer and advisory merged (#671).
- Complete: SH-342 silent post-git synchronization merged (#674).
- Complete: SH-577 coordinate-driven browser regression fixes merged (#672).
- Current: verify SH-586's open pull-request chips on Web UI story cards through
  the centralized verifier.
- Next: audit verifier behavior end to end (SH-560).
- Later: separate the project-owned roadmap from generated agent instructions
  (SH-557).
- Later: resume the attachment epic's authenticated serving and upload work
  (SH-315).
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
- After targeted tests, push one PR, link it with `story link-pr`, then make
  `story move <id> verifying` your last action. The verifier owns the full
  suite, merge, completion, and cleanup.
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
- `story label <id> <label>` — add a label. Two names are reserved:
  `no-auto` (needs a person in the loop; still offered by `story next`) and
  `human-only` (only a person may do it; `story next` never returns it, though
  the story stays ready and is not blocked). Run `story help label` for detail
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
