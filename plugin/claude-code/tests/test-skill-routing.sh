#!/usr/bin/env bash
# The router skill is prose, so nothing else in this suite can catch it
# drifting from the actuator it routes to. These are cheap structural
# assertions against exactly the failure modes that drift produces: a verb
# the script accepts but the skill never mentions, and a file the skill tells
# the agent to read that isn't there.
source "$(dirname "$0")/lib.sh"

SKILL="$PLUGIN_ROOT/skills/story/SKILL.md"
[ -r "$SKILL" ] || fail_test "router skill missing at $SKILL"

# --- every subcommand the script accepts has a documented invocation ---
# Pulled straight out of bin/story.sh's `case`, so adding a subcommand without
# documenting how to call it fails here rather than shipping undiscoverable.
#
# Checked across the skill AND its reference files, and against the literal
# `story.sh <sub>` invocation rather than a bare word: the user-facing verb and
# the script subcommand deliberately differ in two places (`/story do` runs
# `dispatch`, `/story new` runs `create`), so matching prose would be both
# noisier and weaker than matching the call site.
docs=("$SKILL" "$PLUGIN_ROOT"/references/story-*.md)
verbs=$(awk '/^case "\$\{1:-\}" in$/,/^esac$/' "$SCRIPT" \
        | sed -n 's/^  \([a-z][a-z]*\)).*/\1/p')
[ -n "$verbs" ] || fail_test "could not extract any subcommands from $SCRIPT's router"
for v in $verbs; do
  grep -qF "story.sh $v" "${docs[@]}" \
    || fail_test "no documented \`story.sh $v\` invocation in the skill or its references"
done

# --- every skill the router delegates to exists ---
for target in $(grep -o 'skills/story-[a-z]*/SKILL\.md' "$SKILL" | sort -u); do
  [ -r "$PLUGIN_ROOT/$target" ] || fail_test "router delegates to a missing skill: $target"
done

# --- every reference file the router tells the agent to read exists ---
for ref in $(grep -o 'references/[a-z-]*\.md' "$SKILL" | sort -u); do
  [ -r "$PLUGIN_ROOT/$ref" ] || fail_test "router references a missing file: $ref"
done

# --- the flows the new verbs depend on are actually described ---
for section in "List → Pick" "View + Offer" "Dispatch"; do
  grep -qF "$section" "$SKILL" || fail_test "router skill is missing the '$section' flow"
done

# --- bare-id detection must not swallow verbs ---
grep -qF '^[A-Za-z0-9]+-[0-9]+$' "$SKILL" \
  || fail_test "router skill does not pin the bare-id pattern, so a verb could be read as a story id"

# --- the stale "complete is not part of this skill yet" note is gone ---
grep -qi "teardown verb.*not part of this skill" "$SKILL" \
  && fail_test "router skill still claims complete is unimplemented"

# --- SH-62: --auto is documented where a caller would actually look ---
grep -qF -- "--auto" "$SKILL" \
  || fail_test "router skill never mentions --auto (dispatch's autonomous flag)"
grep -qF "STORY_AUTO_PROMPT" "$SKILL" \
  || fail_test "router skill documents STORY_PROMPT but not its --auto counterpart, STORY_AUTO_PROMPT"

# --- reference files must not tell the agent to drive the CLI directly ---
# The whole contract is that side effects go through bin/story.sh; a reference
# that says otherwise reintroduces the duplication the router exists to avoid.
for ref in "$PLUGIN_ROOT/references/story-new.md" "$PLUGIN_ROOT/references/story-complete.md"; do
  [ -r "$ref" ] || { fail_test "missing reference file: $ref"; continue; }
  grep -q 'bin/story\.sh' "$ref" || fail_test "$(basename "$ref") never routes through bin/story.sh"
done

finish
