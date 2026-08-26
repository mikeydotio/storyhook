#!/usr/bin/env bash
# SH-437: Codex skills must cross the sandbox through one unversioned launcher,
# never through the plugin cache path that changes on every update.
source "$(dirname "$0")/lib.sh"

resolver="$PLUGIN_ROOT/references/helper-command.md"
[ -r "$resolver" ] || fail_test "missing helper-command resolver"
assert_contains "$(cat "$resolver")" '~/.codex/storyhook/story.sh' \
  "Codex resolver names the stable launcher"
assert_contains "$(cat "$resolver")" 'Never fall back to the versioned plugin-cache helper' \
  "Codex resolver forbids the stale cache-path fallback"
assert_contains "$(cat "$resolver")" 'never pass a literal `~` or `$HOME`' \
  "Codex resolver requires an absolute path that can match an exact rule"

for skill in \
  "$PLUGIN_ROOT/skills/story/SKILL.md" \
  "$PLUGIN_ROOT/skills/story-context/SKILL.md" \
  "$PLUGIN_ROOT/skills/story-handoff/SKILL.md" \
  "$PLUGIN_ROOT/skills/story-plan/SKILL.md" \
  "$PLUGIN_ROOT/skills/story-setup/SKILL.md" \
  "$PLUGIN_ROOT/skills/story-sync/SKILL.md" \
  "$PLUGIN_ROOT/skills/story-triage/SKILL.md"
do
  assert_contains "$(cat "$skill")" 'references/helper-command.md' \
    "$(basename "$(dirname "$skill")") loads the provider-aware resolver"
done

assert_contains "$(cat "$PLUGIN_ROOT/skills/story-install/SKILL.md")" \
  'story plugin install codex' \
  "Codex's CLI bootstrap also installs the stable launcher and rule"

finish
