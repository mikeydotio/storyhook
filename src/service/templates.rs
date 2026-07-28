//! The repository content storyhook scaffolds on request.
//!
//! Three templates, all of them files a user asks for explicitly — by running
//! `story init` or `story scaffold` — and none of them written by any other
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

This project uses **storyhook** for task tracking. All agents must follow the workflow below.

## Workflow

1. **Start of session**: Load project context
   ```
   story load-context
   ```

2. **Pick next task**: Get the highest-priority ready story
   ```
   story next
   ```

3. **Work on the task**: Implement the changes for the assigned story

4. **Complete the task**: Mark the story as done
   ```
   story move <id> {done_state}
   ```

5. **End of session**: Generate a handoff summary
   ```
   story handoff --since 2h
   ```

## Quick Reference

| Action | Command |
|---|---|
| List open stories | `story list` |
| Show a story | `story show {prefix}-<n>` |
| Create a story | `story new "<title>"` |
| Move to state | `story move {prefix}-<n> <state>` |
| Add a comment | `story comment {prefix}-<n> "comment text"` |
| Set priority | `story prioritize {prefix}-<n> high` |
| Assign a story | `story assign {prefix}-<n> <member>` |
| Add a label | `story label {prefix}-<n> <label>` |
| Block a story | `story block {prefix}-<n> "reason"` |
| Unblock a story | `story unblock {prefix}-<n>` |
| Add relationship | `story relate {prefix}-1 blocks {prefix}-2` |
| Set multiple fields | `story set {prefix}-<n> --priority high --state in-progress` |
| Search stories | `story search "<query>"` |
| Project summary | `story summary` |
| Context (for LLM) | `story load-context` |
| Phase progress | `story phase list` |
| Session handoff | `story handoff --since 2h` |

Run `story help --compact` for the full command reference.

## Important

The `.storyhook/` directory is version-controlled project data. Do NOT add it to
`.gitignore`. It must be committed to git so that project state travels with the repository.
"#,
        done_state = done_state,
        prefix = prefix,
    )
}

/// The `## Storyhook` section a project's own `CLAUDE.md` gets.
#[must_use]
pub fn claude_md() -> String {
    r#"## Storyhook

This project uses **storyhook** for task tracking. Full usage instructions are in `.storyhook/CLAUDE.md` — read that file before starting work.

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

## Commands

- `story list` — list open stories
- `story new "<title>"` — create a new story
- `story show <id>` — show story details
- `story comment <id> "text"` — add a comment
- `story move <id> <state>` — change story state
- `story prioritize <id> <level>` — set priority (critical, high, medium, low, none)
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
