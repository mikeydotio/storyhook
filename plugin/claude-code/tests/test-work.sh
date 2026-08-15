#!/usr/bin/env bash
# `story.sh work [id]` (SH-308) — story selection, [plugin].tracking
# resolution, active-role state transition, and the start comment, all in one
# script call replacing skills/story-work/SKILL.md's five hand-run steps.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)

# --- explicit id, default (normal) tracking: moves AND comments ---
id=$(new_story "$repo" "Work me")
out=$(cd "$repo" && bash "$SCRIPT" work "$id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "explicit id: ok"
assert_eq "$(jqf "$out" .id)" "$id" "explicit id: id"
assert_eq "$(jqf "$out" .picked)" "true" "explicit id: picked"
assert_eq "$(jqf "$out" .state)" "in-progress" "explicit id: active-role state"
assert_eq "$(jqf "$out" .tracking)" "normal" "explicit id: tracking defaults to normal"
assert_eq "$(jqf "$out" .moved)" "true" "explicit id: moved"
assert_eq "$(jqf "$out" .commented)" "true" "explicit id: commented under normal tracking"
assert_contains "$(jqf "$out" .display)" "Work me" "explicit id: display renders the real story"
assert_eq "$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')" "in-progress" \
  "explicit id: the REAL story really moved"
assert_eq "$(cd "$repo" && story show "$id" --json | jq -r '.story.story.comments|length')" "1" \
  "explicit id: the REAL story really got a comment"

# --- quiet tracking: moves but does NOT comment ---
qid=$(new_story "$repo" "Work me quietly")
printf '\n[plugin]\ntracking = "quiet"\n' >> "$repo/.storyhook.toml"
out=$(cd "$repo" && bash "$SCRIPT" work "$qid" 2>&1)
assert_eq "$(jqf "$out" .tracking)" "quiet" "quiet: reads the real [plugin] table"
assert_eq "$(jqf "$out" .moved)" "true" "quiet: still moves"
assert_eq "$(jqf "$out" .commented)" "false" "quiet: does not comment"
assert_eq "$(cd "$repo" && story show "$qid" --json | jq -r '.story.story.comments|length')" "0" \
  "quiet: the REAL story really has no comment"

# --- verbose tracking (still comments, same as normal at this verb's level) ---
vid=$(new_story "$repo" "Work me verbosely")
sed -i '' 's/tracking = "quiet"/tracking = "verbose"/' "$repo/.storyhook.toml"
out=$(cd "$repo" && bash "$SCRIPT" work "$vid" 2>&1)
assert_eq "$(jqf "$out" .tracking)" "verbose" "verbose: reads the real value"
assert_eq "$(jqf "$out" .commented)" "true" "verbose: comments"

# --- no id: picks the top-priority ready story ---
nid=$(new_story "$repo" "Next up")
(cd "$repo" && story prioritize "$nid" critical >/dev/null 2>&1)
out=$(cd "$repo" && bash "$SCRIPT" work 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "no-id: ok"
assert_eq "$(jqf "$out" .picked)" "true" "no-id: picked something"
assert_eq "$(jqf "$out" .id)" "$nid" "no-id: picked the highest-priority ready story"

# --- no id, nothing ready: ok:true, picked:false, never an AskUserQuestion trigger ---
(cd "$repo" && story move "$id" done >/dev/null 2>&1)
(cd "$repo" && story move "$qid" done >/dev/null 2>&1)
(cd "$repo" && story move "$vid" done >/dev/null 2>&1)
(cd "$repo" && story move "$nid" done >/dev/null 2>&1)
out=$(cd "$repo" && bash "$SCRIPT" work 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "nothing-ready: still ok:true"
assert_eq "$(jqf "$out" .picked)" "false" "nothing-ready: picked:false"
assert_contains "$(jqf "$out" .display)" "story-triage" "nothing-ready: display suggests triage"

# --- dry run previews without touching anything ---
did=$(new_story "$repo" "Dry run me")
out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" work "$did" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "dry: ok"
assert_eq "$(jqf "$out" .dry_run)" "true" "dry: flagged"
assert_contains "$(jqf "$out" '.commands|join(" ")')" "story move $did" "dry: previews the move"
assert_eq "$(cd "$repo" && story show "$did" --json | jq -r '.story.story.state')" "todo" \
  "dry: does not actually move"

# --- errors ---
out=$(cd "$repo" && bash "$SCRIPT" work "TST-9999" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "unknown id: ok:false"
out=$(cd "$repo" && bash "$SCRIPT" work "bad id!" 2>&1)
assert_contains "$(jqf "$out" .display)" "alphanumeric" "invalid id: names the constraint"
out=$(cd "$repo" && bash "$SCRIPT" work "$did" extra 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "extra args: rejected"

finish
