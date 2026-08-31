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
# Checked across EVERY skill file and reference file, not just the router's
# own two — SH-308 gave `context`/`sync`/`handoff` their own dedicated
# skills, called from the router's delegation table rather than from its own
# prose, so a verb's documented call site is no longer guaranteed to live in
# $SKILL itself. Match either the installed-path placeholder (`<story-helper>`)
# or a literal `story.sh <sub>` mention. The user-facing verb and script
# subcommand deliberately differ in places such as `do`/`dispatch`, so
# matching bare prose would be noisier and weaker than matching the call site.
docs=("$PLUGIN_ROOT"/skills/*/SKILL.md "$PLUGIN_ROOT"/references/*.md "$PLUGIN_ROOT"/adapters/*.md)
undocumented_router_verbs() {
  local script="$1" v
  for v in $(router_verbs "$script"); do
    grep -Eq "(story\\.sh|<story-helper>\\\") $v" "${docs[@]}" || printf '%s\n' "$v"
  done
}

verbs=$(router_verbs "$SCRIPT")
verb_count=$(printf '%s\n' "$verbs" | awk 'NF { count++ } END { print count + 0 }')
[ "$verb_count" -ge 18 ] \
  || fail_test "router verb extraction is implausibly small: expected at least 18, found $verb_count"

# The helper's command table is generated conceptually from the router above;
# a prose count drifted to "seventeen" while the router already exposed twenty.
# Keep the README count-free so adding a verb has one source of truth.
grep -qF '`bin/story.sh` accepts' "$PLUGIN_ROOT/README.md" \
  && fail_test "plugin README hard-codes a subcommand count instead of listing the router"
for v in $(undocumented_router_verbs "$SCRIPT"); do
  fail_test "no documented helper invocation for subcommand \`$v\`"
done

# Mutation controls: an undocumented arm of either accepted spelling shape
# must be extracted and named. A parser that silently drops one shape cannot
# make the real-tree assertion above look clean by accident.
mutant=$(mktemp /tmp/story-router-mutation.XXXXXX)
_TMP_REPOS+=("$mutant")
awk '
  $0 == "case \"${1:-}\" in" { in_router = 1 }
  in_router && /^  \*\)/ && !inserted {
    print "  freshverb) : ;;"
    print "  fresh-verb) : ;;"
    inserted = 1
  }
  { print }
  END { if (!inserted) exit 1 }
' "$SCRIPT" >"$mutant" || fail_test "could not insert router mutation controls"
missing=$(undocumented_router_verbs "$mutant")
assert_contains "$missing" "freshverb" "router scan names a fresh undocumented plain verb"
assert_contains "$missing" "fresh-verb" "router scan names a fresh undocumented hyphenated verb"

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
grep -qF -- "--agent=claude|codex" "$SKILL" \
  || fail_test "router skill never names dispatch's canonical agent choices"
grep -qF -- '--agent=claude' "$PLUGIN_ROOT/adapters/claude-code.md" \
  || fail_test "Claude adapter does not pass the canonical Claude agent flag"
grep -qF -- '--agent=codex' "$PLUGIN_ROOT/adapters/codex.md" \
  || fail_test "Codex adapter does not pass the canonical Codex agent flag"
grep -qF -- 'STORY_AGENT=claude-code' "$PLUGIN_ROOT/adapters/claude-code.md" \
  && fail_test "Claude adapter still emits the legacy provider token"
grep -qF -- "--force" "$SKILL" \
  || fail_test "router skill never mentions --force (dispatch's forced-redispatch flag)"
grep -qF -- "--resume" "$SKILL" \
  || fail_test "router skill never mentions --resume (dispatch recovery permission)"
grep -qF -- 'resume-available' "$SKILL" \
  || fail_test "router skill never routes the interactive resume confirmation"
grep -qF "STORY_AUTO_PROMPT" "${docs[@]}" \
  || fail_test "router skill documents STORY_PROMPT but not its --auto counterpart, STORY_AUTO_PROMPT"

# --- SH-469: epic Auto is an engine start in the router and both adapters ---
for doc in "$SKILL" "$PLUGIN_ROOT/adapters/claude-code.md" "$PLUGIN_ROOT/adapters/codex.md"; do
  grep -qF 'kind:"engine-run"' "$doc" \
    || fail_test "$(basename "$doc"): epic Auto result kind is undocumented"
  grep -qF -- '--full-auto' "$doc" \
    || fail_test "$(basename "$doc"): engine-private lane marker boundary is undocumented"
done
grep -qF 'typed epic' "$SKILL" \
  || fail_test "router skill does not distinguish typed epics from actionable stories"
grep -qF 'without `--auto`' "$SKILL" \
  || fail_test "router skill does not document bare-epic refusal"

# --- reference files must not tell the agent to drive the CLI directly ---
# The whole contract is that side effects go through bin/story.sh; a reference
# that says otherwise reintroduces the duplication the router exists to avoid.
for ref in "$PLUGIN_ROOT/references/story-new.md" "$PLUGIN_ROOT/references/story-complete.md"; do
  [ -r "$ref" ] || { fail_test "missing reference file: $ref"; continue; }
  grep -q '<story-helper>' "$ref" || fail_test "$(basename "$ref") never routes through the installed helper"
done

finish
