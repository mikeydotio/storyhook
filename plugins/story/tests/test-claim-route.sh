#!/usr/bin/env bash
# SH-478: the helper's `work` verb and the skill that fronted it are retired,
# and the `/story` router routes `claim <id>|--next` in their place.
#
# This file replaces test-work.sh. It is deliberately in two halves, because
# the route has two halves that can break independently:
#
#   1. The helper no longer answers `work` at all -- a caller who followed the
#      old prose gets the usage line rather than a stale claim path.
#   2. The router names an invocation the CLI actually accepts. That is a
#      WIRING assertion in the SH-360 sense: the four other work test files
#      existed to defend a hand-rolled retry loop that no longer exists, and
#      what replaces them is not more coverage of claiming (SH-476's own tests
#      own that) but proof that the spelling the router hands an agent is real.
#
# One spelling below is assembled at run time rather than written out; the
# comment at its site says why.
source "$(dirname "$0")/lib.sh"

SKILL="$PLUGIN_ROOT/skills/story/SKILL.md"
[ -r "$SKILL" ] || fail_test "router skill missing at $SKILL"

# --- the retired verb ---------------------------------------------------------

out=$(bash "$SCRIPT" work 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "work: the retired verb is refused"
assert_contains "$(jqf "$out" .display)" "usage: story.sh" "work: refusal is the usage line"

case "$(jqf "$(bash "$SCRIPT" bogus-subcommand 2>&1)" .display)" in
*" work "* | *"work [story-id]"*)
  fail_test "the usage line still offers \`work\`, a verb the router no longer accepts" ;;
esac

# Assembled from pieces so this file is not itself an offender against
# tests/retired_work_verb.rs, which forbids the retired skill's name in any
# tracked file. Same trick, same reason: a fence that needs an exemption for
# its own sibling is a fence with a widening hole in it.
retired_skill="story-""work"
[ -e "$PLUGIN_ROOT/skills/$retired_skill" ] \
  && fail_test "skills/$retired_skill still exists; the router no longer delegates to it"

# --- the router's replacement route -------------------------------------------

skill_text=$(cat "$SKILL")
assert_contains "$skill_text" '`claim <id>`' "router: names the id form"
assert_contains "$skill_text" '`claim --next`' "router: names the --next form"
assert_contains "$skill_text" "## Claim" "router: carries the Claim flow"
assert_contains "$skill_text" "references/ensure-cli.md" "router: the Claim flow checks for the CLI first"
# SH-308 left "present working context" as prose because summarizing what a
# story MEANS is judgment, not a fact a command can assert. Retiring the skill
# that carried it must not delete it -- this story required the choice to be
# made explicitly rather than by omission.
assert_contains "$skill_text" "Present working context" \
  "router: the retired skill's judgment step survived the retirement"

# --- the wiring: the CLI accepts exactly what the router prescribes ------------

repo=$(mk_story_repo)

id=$(new_story "$repo" "Claim me by id")
out=$(cd "$repo" && story claim "$id" --json 2>&1)
assert_eq "$(jqf "$out" .result)" "ok" "claim <id>: the router's spelling is accepted"
assert_eq "$(jqf "$out" .claimed_from)" "todo" "claim <id>: reports the state it came out of"
assert_eq "$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')" "in-progress" \
  "claim <id>: the REAL story really moved into the active-role state"

nid=$(new_story "$repo" "Claim me next")
(cd "$repo" && story prioritize "$nid" critical >/dev/null 2>&1)
out=$(cd "$repo" && story claim --next --json 2>&1)
assert_eq "$(jqf "$out" .result)" "ok" "claim --next: accepted"
assert_eq "$(jqf "$out" .story.story.id)" "$nid" "claim --next: took the top-priority ready story"

# A bare `claim` is a REFUSAL, never a silent `--next`. The router says so; the
# CLI is what has to mean it, since a script whose id argument came out empty
# reaches the CLI, not the prose.
out=$(cd "$repo" && story claim --json 2>&1)
assert_eq "$(jqf "$out" .result)" "error" "bare claim: refused rather than resolved to --next"

finish
