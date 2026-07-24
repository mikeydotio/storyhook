# Ensure the storyhook CLI is available

Every storyhook skill that shells out to `story` depends on the CLI being installed.
When the plugin is installed via the Claude Code marketplace, the CLI is *not* included,
so a skill may run in a project where `story` is missing.

Before doing CLI-dependent work, run this guard:

1. Run `command -v story`.
2. **If found**, proceed with the skill normally.
3. **If not found**, the storyhook CLI is required and must be installed first:
   - Tell the user the `story` CLI isn't installed and this skill needs it.
   - Ask for explicit permission to install it (use `AskUserQuestion`).
   - **If approved**, follow the `story-install` skill to install and verify the CLI,
     then continue with the original skill.
   - **If declined**, stop and explain that the skill can't run without the CLI, and point
     the user to `/story-install` for when they're ready.

Do not attempt to run `story` commands until the guard passes — they will fail with
"command not found" and produce confusing errors.

> Note: this guard applies to interactive, user-invocable skills only. The session hooks
> (`session-start.sh`, `post-git.sh`, `stop-handoff.sh`) run headless and intentionally
> no-op when `story` is absent — they must never prompt.
