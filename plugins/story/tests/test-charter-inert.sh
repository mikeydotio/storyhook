#!/usr/bin/env bash
# SH-226 family F -- the CHARTER-INERT invariant.
#
# The handoff prompt is prose an LLM reads, delivered as keystrokes into a pane.
# When that pane turned out to be a shell, the shell executed it: the charter's
# backticked spans are command substitutions, and one of them was
# `story move <n> done`. Four stories closed with no work done.
#
# The gate in test-dispatch-occupant-gate.sh stops the charter reaching a shell
# through DISPATCH. This asserts the complementary property -- that the text is
# inert wherever it lands, including channels that have no gate at all: a doc
# quoting it, a transcript, or an agent reading the template out of the repo.
# That last channel is not hypothetical; it happened during the investigation.
#
# WHY AN ALLOWLIST. The charter was already "mostly" inert before the fix, by
# accident: an unquoted `(` made zsh abort command one AFTER its substitutions
# had run, and the bare word `done` -- a reserved word -- parse-errored command
# two after `story move <n> done` had already executed. Both accidents depend on
# where a paren and a reserved word happen to fall in English prose, and both
# differ by shell (bash rejects the whole string up front and runs nothing). An
# invariant phrased as "which characters can execute something" is really an
# invariant about where the parse dies, which nobody can maintain. So: allow
# only characters that are non-special in EVERY POSIX-family shell, and the
# worst case becomes one `command not found` on the first word.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)
id=$(new_story "$repo" "Charter inertness")

# STORY_DRY_RUN emits the fully rendered prompt, AFTER <n>/<reap> substitution and
# after PROMPT_EXTRA is appended -- the real render path, not a re-derivation of
# the template. That is what makes this a test of what actually gets typed.
#
# SH-219: --auto now has TWO charters -- COUNCIL (council_vote_available finds
# a real ‘/council-vote’ skill on disk) and SOLO (it doesn't) -- so both are
# rendered here, forced deterministically via STORY_COUNCIL, independent of
# whatever plugins happen to be installed wherever this suite runs.
attended=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$id" 2>&1 | jq -r '.prompt')
auto=$(cd "$repo" && STORY_DRY_RUN=1 STORY_COUNCIL=on bash "$SCRIPT" dispatch "$id" --auto 2>&1 | jq -r '.prompt')
solo=$(cd "$repo" && STORY_DRY_RUN=1 STORY_COUNCIL=off bash "$SCRIPT" dispatch "$id" --auto 2>&1 | jq -r '.prompt')
codex_attended=$(cd "$repo" && STORY_DRY_RUN=1 STORY_AGENT=codex bash "$SCRIPT" dispatch "$id" 2>&1 | jq -r '.prompt')
codex_auto=$(cd "$repo" && STORY_DRY_RUN=1 STORY_AGENT=codex STORY_COUNCIL=on bash "$SCRIPT" dispatch "$id" --auto 2>&1 | jq -r '.prompt')
codex_solo=$(cd "$repo" && STORY_DRY_RUN=1 STORY_AGENT=codex STORY_COUNCIL=off bash "$SCRIPT" dispatch "$id" --auto 2>&1 | jq -r '.prompt')

[ -n "$attended" ] && [ "$attended" != null ] \
  || fail_test "charter-inert: could not render the attended prompt"
[ -n "$auto" ] && [ "$auto" != null ] \
  || fail_test "charter-inert: could not render the auto (council) prompt"
[ -n "$solo" ] && [ "$solo" != null ] \
  || fail_test "charter-inert: could not render the auto (solo) prompt"
for pair in "attended:$codex_attended" "auto:$codex_auto" "solo:$codex_solo"; do
  label="${pair%%:*}"; text="${pair#*:}"
  [ -n "$text" ] && [ "$text" != null ] \
    || fail_test "charter-inert: could not render the Codex $label prompt"
done

# Every character not on the allowlist. Kept as an explicit banned set so a
# failure names the offending character rather than just failing a regex.
check_inert() {
  local label="$1" text="$2" bad
  bad=$(printf '%s' "$text" | python3 -c '
import sys
BANNED = set("`$;&|<>!()[]{}*?~#\\\\\x27") | {"\n"}
text = sys.stdin.read()
print("".join(sorted({c for c in text if c in BANNED})))')
  if [ -n "$bad" ]; then
    fail_test "charter-inert: $label carries shell metacharacter(s) [$bad] -- a shell would not treat this as inert text"
  fi
}

check_inert "the attended prompt" "$attended"
check_inert "the autonomous (council) charter" "$auto"
check_inert "the autonomous (solo) charter" "$solo"
check_inert "the Codex attended prompt" "$codex_attended"
check_inert "the Codex autonomous (council) charter" "$codex_auto"
check_inert "the Codex autonomous (solo) charter" "$codex_solo"

# An even quote count: an unbalanced double quote wedges a shell at a
# continuation prompt rather than executing anything, but it is still a wedge.
for pair in "attended:$attended" "auto:$auto" "solo:$solo" \
            "Codex attended:$codex_attended" "Codex auto:$codex_auto" \
            "Codex solo:$codex_solo"; do
  label="${pair%%:*}"; text="${pair#*:}"
  n=$(printf '%s' "$text" | tr -cd '"' | wc -c | tr -d ' ')
  if [ $((n % 2)) -ne 0 ]; then
    fail_test "charter-inert: the $label prompt has an odd number of double quotes ($n)"
  fi
done

# The invariant must never be satisfiable by deleting the instructions. These
# are the load-bearing spans -- the things the agent is actually told to run.
# Shared between both --auto charters (the head/tail SH-219 split in two):
for needle in "story show $id --json" "story move $id verifying" \
              "story link-pr $id PR-URL" "new and directly impacted tests" \
              "story block $id" "prefer adopting it into" \
              "context window is still unused" "before you resume the work"; do
  for variant in "auto:$auto" "solo:$solo"; do
    label="${variant%%:*}"; text="${variant#*:}"
    case "$text" in
      *"$needle"*) ;;
      *) fail_test "charter-inert: the $label charter no longer instructs '$needle' -- inertness must not be bought by removing the instructions" ;;
    esac
  done
done
# Gate and merge authority must stay with the centralized verifier.
for variant in "auto:$auto" "solo:$solo"; do
  label="${variant%%:*}"; text="${variant#*:}"
  case "$text" in
    *"Do not run make test, land-pr.sh, story move $id done, reap"*) ;;
    *) fail_test "charter-inert: the $label charter no longer forbids child-owned verification" ;;
  esac
  case "$text" in
    *"gh pr merge"*) fail_test "charter-inert: the $label charter still instructs the bare gh merge path" ;;
  esac
done
# COUNCIL-only obligation:
case "$auto" in
  *"council-vote"*) ;;
  *) fail_test "charter-inert: the council charter no longer instructs 'council-vote' -- inertness must not be bought by removing the instructions" ;;
esac
# SOLO must never name council-vote -- there is nothing there to convene.
case "$solo" in
  *"council-vote"*) fail_test "charter-inert: the solo charter names 'council-vote', which SH-219 exists to keep it from doing" ;;
esac
# SOLO's own load-bearing obligation the council charter doesn't carry:
case "$solo" in
  *"do not stall"*) ;;
  *) fail_test "charter-inert: the solo charter no longer instructs 'do not stall' -- inertness must not be bought by removing the instructions" ;;
esac
case "$attended" in
  *"story show $id --json"*) ;;
  *) fail_test "charter-inert: the attended prompt no longer instructs 'story show'" ;;
esac

# Codex's provider-only addition is itself load-bearing: it carries the
# persistence obligation across the UI mode switch that has no ExitPlanMode
# tool boundary. All three built-in variants must name the resolved story,
# exact-plan semantics, and the first implementation step.
for variant in "attended:$codex_attended" "auto:$codex_auto" "solo:$codex_solo"; do
  label="${variant%%:*}"; text="${variant#*:}"
  for needle in "story comment $id your-exact-approved-plan" \
                "the first implementation step" \
                "post the plan verbatim rather than summarizing it"; do
    case "$text" in
      *"$needle"*) ;;
      *) fail_test "charter-inert: the Codex $label prompt no longer instructs '$needle'" ;;
    esac
  done
done

finish
