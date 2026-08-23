# Resolve the Storyhook helper command

Every Storyhook skill needs one `<story-helper>` path, but Codex and Claude resolve it
differently because Codex command rules match exact argument prefixes.

- **Codex:** resolve `<story-helper>` as the active user's absolute
  `~/.codex/storyhook/story.sh` path. Expand the home directory before constructing the
  tool call; never pass a literal `~` or `$HOME`. This unversioned launcher is installed by
  `story plugin install codex`, and it resolves the currently enabled plugin version on
  every invocation. Never fall back to the versioned plugin-cache helper: that path changes
  on update and bypasses Storyhook's narrow Codex rule.
- **Other hosts:** resolve `<story-helper>` as `<plugin-root>/bin/story.sh`, where
  `<plugin-root>` is the absolute installed plugin root derived by the loading skill.

Invoke either path as `bash "<story-helper>" <subcommand> ...`.

If the Codex launcher is missing, run `command -v story`. If the CLI exists, stop and tell
the user to run `story plugin install codex`, then restart Codex. If the CLI is missing,
load `<plugin-root>/skills/story-install/SKILL.md` and follow it; after installation, the
Codex plugin installer still needs to be run before helper-backed skills can continue.
