#!/usr/bin/env bash
# `story.sh view <id>` — read-only. Backs both `/story view <id>` and the
# bare-id View + Offer flow.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)
id=$(new_story "$repo" "A viewable story")
(cd "$repo" && story comment "$id" "a discussion comment" >/dev/null 2>&1)

out=$(cd "$repo" && bash "$SCRIPT" view "$id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "view: ok"
assert_eq "$(jqf "$out" .id)" "$id" "view: id"
assert_eq "$(jqf "$out" .title)" "A viewable story" "view: title"
assert_eq "$(jqf "$out" .state)" "todo" "view: state"
assert_eq "$(jqf "$out" .superstate)" "OPEN" "view: superstate"
assert_contains "$(jqf "$out" .display)" "A viewable story" "view: display renders the story"
assert_contains "$(jqf "$out" .display)" "a discussion comment" \
  "view: display carries the comments (the discussion history the offer flow needs)"

# --- read-only ---
assert_eq "$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')" "todo" \
  "view: does not mutate state"

# --- a closed story is still viewable (archived stories resolve too) ---
closed=$(new_story "$repo" "Closed story")
(cd "$repo" && story move "$closed" done >/dev/null 2>&1)
out=$(cd "$repo" && bash "$SCRIPT" view "$closed" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "view: an archived story still resolves"
assert_eq "$(jqf "$out" .superstate)" "CLOSED" "view: reports it closed"

# --- works from a subdirectory (SH-46's anchoring applies to every verb) ---
mkdir -p "$repo/src/nested"
out=$(cd "$repo/src/nested" && bash "$SCRIPT" view "$id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "view: works from a subdirectory"

# --- errors ---
out=$(cd "$repo" && bash "$SCRIPT" view "TST-9999" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "view: unknown story is ok:false"
assert_contains "$(jqf "$out" .display)" "TST-9999" "view: names the missing id"

out=$(cd "$repo" && bash "$SCRIPT" view 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "view: missing id is ok:false"
assert_contains "$(jqf "$out" .display)" "usage" "view: missing id shows usage"

out=$(cd "$repo" && bash "$SCRIPT" view "bad id!" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "view: invalid id is ok:false"
assert_contains "$(jqf "$out" .display)" "alphanumeric" "view: names the constraint"

out=$(cd "$repo" && bash "$SCRIPT" view "$id" extra 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "view: rejects extra arguments"

finish
