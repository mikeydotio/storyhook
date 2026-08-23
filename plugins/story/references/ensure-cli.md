# Ensure the storyhook CLI is available

Every storyhook skill that shells out to `story` depends on the CLI being installed.
When the plugin is installed from a marketplace, the CLI is *not* included,
so a skill may run in a project where `story` is missing.

The loading skill resolves `<plugin-root>` from its own installed `SKILL.md` path and
`<story-helper>` as the absolute `<plugin-root>/bin/story.sh` path. Substitute and quote
that absolute path below; never resolve it from the user's current working directory.

Before doing CLI-dependent work, run this guard:

1. Run `bash "<story-helper>" ensure-cli`. This is the one verb that
   answers even when `story` itself is missing — it always returns `ok:true`; `installed`
   is the fact you're checking.
2. **`installed:true`**, proceed with the skill normally.
3. **`installed:false`**, the storyhook CLI is required and must be installed first:
   - Tell the user the `story` CLI isn't installed and this skill needs it.
   - Ask one concise question for explicit permission, using the host's structured question
     mechanism when available.
   - **If approved**, load `<plugin-root>/skills/story-install/SKILL.md` and follow it to install and verify the CLI,
     then continue with the original skill.
   - **If declined**, stop and explain that the skill can't run without the CLI, and point
     the user to the `story-install` skill for when they're ready.

Do not attempt to run `story` commands until the guard passes — they will fail with
"command not found" and produce confusing errors.

> Note: this guard applies to interactive skills only. Provider session hooks
> (`session-start.sh`, `post-git.sh`, `stop-handoff.sh`) run headless and intentionally
> no-op when `story` is absent — they must never prompt.
