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

for skill in "$PLUGIN_ROOT"/skills/*/SKILL.md; do
  name=$(basename "$(dirname "$skill")")
  case "$name" in
  story-install)
    # This skill bootstraps the CLI and stable launcher, so the resolver's
    # requirement that both already exist cannot apply to it.
    continue
    ;;
  story-update)
    # The CLI owns its atomic self-update directly; no helper orchestration is
    # involved, so resolving the helper would add a dependency with no use.
    continue
    ;;
  esac
  assert_contains "$(cat "$skill")" 'references/helper-command.md' \
    "$name loads the provider-aware resolver"
done

assert_contains "$(cat "$PLUGIN_ROOT/skills/story-install/SKILL.md")" \
  'story plugin install codex' \
  "Codex's CLI bootstrap also installs the stable launcher and rule"

finish
