#!/usr/bin/env bash
# `story.sh list` — the rows the skill's List->Pick flow offers.
#
# `story list --ready` used to not be the right answer on its own: is_ready()
# asked "is this workable", not "is this available to pick up" — it returned
# true for a story someone was already working (the gap /story do's own guard
# exists to close), and unlike `story next` it does not filter out parents.
# storyhook's SH-236 fixed the first gap (`story list --ready` now excludes
# an in-progress story itself), so cmd_list's own already-claimed filter is
# redundant with its input rather than load-bearing — kept anyway since a
# no-op filter costs nothing and a future storyhook regression would be
# masked, not caused, by removing it. The parent-exclusion narrowing is
# unaffected and still load-bearing. Both are asserted here against the real
# CLI.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)

# --- empty project ---
out=$(cd "$repo" && bash "$SCRIPT" list 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "empty: ok"
assert_eq "$(jqf "$out" .count)" "0" "empty: count 0"
assert_eq "$(jqf "$out" '.stories|length')" "0" "empty: no rows"
assert_contains "$(jqf "$out" .display)" "No ready stories" "empty: display says so"

ready=$(new_story "$repo" "Ready one")
(cd "$repo" && story prioritize "$ready" high >/dev/null 2>&1)

# --- row shape matches what AskUserQuestion consumes ---
out=$(cd "$repo" && bash "$SCRIPT" list 2>&1)
assert_eq "$(jqf "$out" .count)" "1" "one: count"
assert_eq "$(jqf "$out" '.stories[0].id')" "$ready" "one: id"
assert_eq "$(jqf "$out" '.stories[0].option.label')" "$ready" "one: option.label is the story id"
assert_eq "$(jqf "$out" '.stories[0].option.description')" "Ready one" "one: option.description is the title"
assert_eq "$(jqf "$out" '.stories[0].priority')" "high" "one: priority surfaced for ordering context"

# --- an already-claimed story is excluded (SH-236: storyhook itself now
# --- excludes it from `list --ready`; cmd_list's own filter is redundant
# --- defense-in-depth, not what makes this pass) ---
claimed=$(new_story "$repo" "Being worked")
(cd "$repo" && story move "$claimed" in-progress >/dev/null 2>&1)
# Fixture sanity: pins the new ground truth so a storyhook regression here is
# caught by *this* test too, not only by storyhook's own SH-236 tests.
in_ready=$(cd "$repo" && story list --ready --json | jq --arg id "$claimed" '[.stories[]?.story.id]|index($id) != null')
assert_eq "$in_ready" "false" "fixture sanity: \`story list --ready\` excludes an in-progress story (SH-236)"
out=$(cd "$repo" && bash "$SCRIPT" list 2>&1)
assert_eq "$(jqf "$out" '[.stories[].id]|index("'"$claimed"'")')" "null" "claimed: excluded from the pick list"

# --- a parent/epic is excluded, matching `story next` ---
child=$(new_story "$repo" "A child")
parent=$(new_story "$repo" "An epic")
(cd "$repo" && story relate "$parent" parent-of "$child" >/dev/null 2>&1)
out=$(cd "$repo" && bash "$SCRIPT" list 2>&1)
assert_eq "$(jqf "$out" '[.stories[].id]|index("'"$parent"'")')" "null" "parent: epic excluded"
[ "$(jqf "$out" '[.stories[].id]|index("'"$child"'")')" = "null" ] \
  && fail_test "parent: the child should still be offered"

# --- a blocked story is excluded (is_ready() handles this one) ---
blocked=$(new_story "$repo" "Blocked")
(cd "$repo" && story block "$blocked" "waiting" >/dev/null 2>&1)
out=$(cd "$repo" && bash "$SCRIPT" list 2>&1)
assert_eq "$(jqf "$out" '[.stories[].id]|index("'"$blocked"'")')" "null" "blocked: excluded"

# --- a closed story is excluded, and `story list` alone would NOT exclude it ---
closed=$(new_story "$repo" "Closed")
(cd "$repo" && story move "$closed" done >/dev/null 2>&1)
bare_has=$(cd "$repo" && story list --json | jq --arg id "$closed" '[.stories[]?.story.id]|index($id) != null')
assert_eq "$bare_has" "true" "fixture sanity: bare \`story list\` really does include closed stories"
out=$(cd "$repo" && bash "$SCRIPT" list 2>&1)
assert_eq "$(jqf "$out" '[.stories[].id]|index("'"$closed"'")')" "null" "closed: excluded"

# --- arg validation ---
out=$(cd "$repo" && bash "$SCRIPT" list extra 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "list: takes no arguments"

finish
