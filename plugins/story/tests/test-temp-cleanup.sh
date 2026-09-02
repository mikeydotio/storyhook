#!/usr/bin/env bash
# A fixture helper commonly runs inside command substitution so it can return
# its path. Cleanup registration must survive that subshell and reach the EXIT
# trap in the caller that sourced lib.sh.
source "$(dirname "$0")/lib.sh"

paths=$(mktemp /tmp/story-test-cleanup-paths.XXXXXX)
_TMP_REPOS+=("$paths")

bash -c '
  source "$1"
  repo=$(mk_story_repo CLN)
  install=$(mk_versioned_claude 1.2.3)
  printf "%s\n%s\n" "$repo" "$install"
' _ "$TESTS_DIR/lib.sh" >"$paths"

while IFS= read -r path; do
  [ -n "$path" ] || fail_test "temp cleanup: helper returned an empty path"
  [ ! -e "$path" ] \
    || fail_test "temp cleanup: command-substitution fixture survived its EXIT trap at $path"
done <"$paths"

assert_eq "$(wc -l <"$paths" | tr -d " ")" "2" \
  "temp cleanup: both command-substitution helpers were exercised"

finish
