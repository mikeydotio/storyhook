#!/usr/bin/env bash
# `story.sh create` — the filing half of `/story new`.
#
# The description path is the interesting one. `story new` has no
# --description-file (unlike `gh issue create --body-file`), so the drafted
# markdown is written to a file and read back into a single argv element.
# These assertions pin that the content survives byte-for-byte: markdown that
# reached the CLI through a shell command string would be mangled by
# backticks (command substitution) and $dollars (parameter expansion).
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)

# --- minimal ---
out=$(cd "$repo" && bash "$SCRIPT" create --title "A filed story" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "create: ok"
id=$(jqf "$out" .id)
assert_contains "$id" "TST-" "create: returns the assigned id"
assert_contains "$(jqf "$out" .display)" "$id" "create: display names the id"
assert_eq "$(cd "$repo" && story show "$id" --json | jq -r '.story.story.title')" "A filed story" \
  "create: the real CLI agrees on the title"

# --- every field reaches the CLI ---
out=$(cd "$repo" && bash "$SCRIPT" create --title "Fully specified" \
        --type bug --priority high --label "alpha,beta" 2>&1)
full=$(jqf "$out" .id)
meta=$(cd "$repo" && story show "$full" --json)
assert_eq "$(printf '%s' "$meta" | jq -r '.story.story.story_type')" "bug" "create: --type"
assert_eq "$(printf '%s' "$meta" | jq -r '.story.story.priority')" "high" "create: --priority"
assert_eq "$(printf '%s' "$meta" | jq -r '.story.story.labels|join(",")')" "alpha,beta" "create: --label"

# --- a hostile multi-line description survives intact ---
desc="$repo/desc.md"
cat >"$desc" <<'MARKDOWN'
## Context

Line with `backticks` and $dollars and "quotes" and 'singles'.

    $(rm -rf /nope)
    `touch /nope`

- item one
- item two
MARKDOWN
out=$(cd "$repo" && bash "$SCRIPT" create --title "Rich description" --description-file "$desc" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "create: --description-file ok"
rich=$(jqf "$out" .id)
stored=$(cd "$repo" && story show "$rich" --json | jq -r '.story.story.description')
expected=$(cat "$desc")
assert_eq "$stored" "$expected" "create: description survives byte-for-byte"
assert_contains "$stored" 'touch /nope' "create: backticked text stored literally, not executed"
assert_contains "$stored" '$dollars' "create: dollars stored literally, not expanded"
[ -e /nope ] && fail_test "create: a substitution in the description actually executed"

# --- inline --description still works ---
out=$(cd "$repo" && bash "$SCRIPT" create --title "Inline desc" --description "just a line" 2>&1)
inline=$(jqf "$out" .id)
assert_eq "$(cd "$repo" && story show "$inline" --json | jq -r '.story.story.description')" "just a line" \
  "create: inline --description"

# --- SH-779: --blocked-by files the story already blocked, in one write ---
blocker=$(jqf "$(cd "$repo" && bash "$SCRIPT" create --title "The blocker" 2>&1)" .id)
other=$(jqf "$(cd "$repo" && bash "$SCRIPT" create --title "Another blocker" 2>&1)" .id)
out=$(cd "$repo" && bash "$SCRIPT" create --title "Waits on both" \
        --blocked-by "$blocker" --blocked-by "$other" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "create: --blocked-by ok"
waits=$(jqf "$out" .id)
edges=$(cd "$repo" && story show "$waits" --json \
          | jq -r '[.story.story.relationships[] | select(.relation == "blocked-by") | .other_id] | sort | join(",")')
assert_eq "$edges" "$(printf '%s\n%s\n' "$blocker" "$other" | sort | paste -sd, -)" \
  "create: every --blocked-by reaches the CLI as an edge"
ready=$(cd "$repo" && story list --ready --json | jq -r '[.stories[].story.id] | join(",")')
case ",$ready," in
  *",$waits,"*) fail_test "create: a story filed with --blocked-by must not be ready" ;;
esac
out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" create --title "Dry blocked" --blocked-by "$blocker" 2>&1)
assert_contains "$(jqf "$out" '.commands[0]')" "--blocked-by $blocker" "dry: names each --blocked-by"
out=$(cd "$repo" && bash "$SCRIPT" create --title "x" --blocked-by 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "create: --blocked-by needs a value"
assert_contains "$(jqf "$out" .display)" "--blocked-by" "create: says which flag"
before=$(cd "$repo" && story list --json | jq '.stories|length')
out=$(cd "$repo" && bash "$SCRIPT" create --title "Never filed" --blocked-by TST-999 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "create: an unknown blocker is ok:false"
after=$(cd "$repo" && story list --json | jq '.stories|length')
assert_eq "$after" "$before" "create: an unknown blocker files nothing"

# --- dry run files nothing ---
before=$(cd "$repo" && story list --json | jq '.stories|length')
out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" create --title "Never filed" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "dry: ok"
assert_eq "$(jqf "$out" .dry_run)" "true" "dry: flagged"
after=$(cd "$repo" && story list --json | jq '.stories|length')
assert_eq "$after" "$before" "dry: no story was actually created"

# --- errors ---
out=$(cd "$repo" && bash "$SCRIPT" create 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "create: --title is required"
assert_contains "$(jqf "$out" .display)" "--title" "create: usage names --title"

out=$(cd "$repo" && bash "$SCRIPT" create --title "x" --description-file /nonexistent/nope.md 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "create: unreadable description file is ok:false"
assert_contains "$(jqf "$out" .display)" "not readable" "create: says why"

out=$(cd "$repo" && bash "$SCRIPT" create --title "x" --bogus y 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "create: unknown flag is rejected"

out=$(cd "$repo" && bash "$SCRIPT" create --title "x" --type no-such-type 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "create: a CLI validation failure surfaces as ok:false"

# Complexity crosses the helper boundary as one explicit choice.
out=$(cd "$repo" && bash "$SCRIPT" create --title "Assessed work" --complexity high 2>&1)
assert_eq "$(jqf "$out" .ok)" true "create accepts complexity"
assessed=$(jqf "$out" .id)
stored=$(cd "$repo" && story show "$assessed" --json)
assert_eq "$(jqf "$stored" .story.story.complexity)" high "helper stores the complexity"
assert_eq "$(jqf "$stored" .story.story.complexity_assessed)" true "helper marks explicit choice assessed"
finish
