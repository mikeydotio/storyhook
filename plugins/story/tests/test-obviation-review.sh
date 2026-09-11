#!/usr/bin/env bash
# SH-673: rendered dispatch and the helper's real CLI context door.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)
id=$(new_story "$repo" "Target")
candidate=$(new_story "$repo" "Candidate")
(cd "$repo" && story move "$candidate" in-progress >/dev/null)

out=$(cd "$repo" && bash "$SCRIPT" context --story "$id")
assert_eq "$(jqf "$out" .ok)" true "story context succeeds"
assert_contains "$(jqf "$out" .display)" "Obviation review" "review is rendered"
assert_contains "$(jqf "$out" .display)" "$candidate" "candidate survives helper"
out=$(cd "$repo" && bash "$SCRIPT" context --full --story "$id")
assert_eq "$(jqf "$out" .ok)" true "story and full compose"
assert_contains "$(jqf "$out" .display)" "Critical path" "full survives"
out=$(cd "$repo" && bash "$SCRIPT" context --story SH-999999)
assert_eq "$(jqf "$out" .ok)" false "missing target is a failure"
assert_contains "$(jqf "$out" .display)" "SH-999999" "failure retains context"
out=$(cd "$repo" && bash "$SCRIPT" context --story)
assert_eq "$(jqf "$out" .ok)" false "missing flag value is a failure"

for agent in claude codex; do
  for mode in attended council solo; do
    args=(dispatch "$id"); council=off
    if [ "$mode" != attended ]; then args+=(--auto); fi
    if [ "$mode" = council ]; then council=on; fi
    out=$(cd "$repo" && STORY_DRY_RUN=1 STORY_AGENT="$agent" STORY_COUNCIL="$council" \
      bash "$SCRIPT" "${args[@]}")
    prompt=$(jqf "$out" .prompt)
    assert_contains "$prompt" "story load-context --story $id" "$agent $mode names target"
    assert_contains "$prompt" "story help obviation-review" "$agent $mode names procedure"
    assert_contains "$prompt" "Before implementation" "$agent $mode orders review"
  done
done

# Explicit whole-prompt overrides remain whole-prompt overrides.
for agent in claude codex; do
  out=$(cd "$repo" && STORY_DRY_RUN=1 STORY_AGENT="$agent" STORY_PROMPT='Custom <n>' \
    bash "$SCRIPT" dispatch "$id")
  assert_eq "$(jqf "$out" .prompt)" "Custom $id" "attended override preserved"
  for council in on off; do
    out=$(cd "$repo" && STORY_DRY_RUN=1 STORY_AGENT="$agent" STORY_COUNCIL="$council" \
      STORY_AUTO_PROMPT='Custom auto <n>' bash "$SCRIPT" dispatch "$id" --auto)
    assert_eq "$(jqf "$out" .prompt)" "Custom auto $id" "autonomous override preserved"
  done
done
finish
