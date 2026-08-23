#!/usr/bin/env bash
# A skill runs in the user's repository, not the plugin directory. Reproduce a
# versioned/cachebuster install path, resolve the helper from the loaded
# SKILL.md location, then execute it from an unrelated cwd.
source "$(dirname "$0")/lib.sh"

cache=$(mktemp -d /tmp/story-plugin-cache.XXXXXX)
unrelated=$(mktemp -d /tmp/story-plugin-cwd.XXXXXX)
_TMP_REPOS+=("$cache" "$unrelated")
installed="$cache/story/0.6.0+sh432"
mkdir -p "$installed"
cp -R "$PLUGIN_ROOT/." "$installed/"

skill_file="$installed/skills/story-context/SKILL.md"
[ -r "$skill_file" ] || fail_test "copied skill missing from versioned install path"
skill_dir=$(cd "$(dirname "$skill_file")" && pwd)
resolved_helper=$(cd "$skill_dir/../.." && pwd)/bin/story.sh

assert_eq "$resolved_helper" "$installed/bin/story.sh" \
  "SKILL.md-relative resolver must select the installed helper"

rc=0
out=$(cd "$unrelated" && bash "$resolved_helper" 2>&1) || rc=$?
[ "$rc" -ne 0 ] || fail_test "helper without a verb should return a usage refusal"
assert_contains "$out" '"ok": false' "installed helper returned its JSON envelope"
assert_contains "$out" 'usage: story.sh' "installed helper executed from unrelated cwd"

finish
