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
attended=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$id" 2>&1 | jq -r '.prompt')
auto=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$id" --auto 2>&1 | jq -r '.prompt')

[ -n "$attended" ] && [ "$attended" != null ] \
  || fail_test "charter-inert: could not render the attended prompt"
[ -n "$auto" ] && [ "$auto" != null ] \
  || fail_test "charter-inert: could not render the auto prompt"

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
check_inert "the autonomous charter" "$auto"

# An even quote count: an unbalanced double quote wedges a shell at a
# continuation prompt rather than executing anything, but it is still a wedge.
for pair in "attended:$attended" "auto:$auto"; do
  label="${pair%%:*}"; text="${pair#*:}"
  n=$(printf '%s' "$text" | tr -cd '"' | wc -c | tr -d ' ')
  if [ $((n % 2)) -ne 0 ]; then
    fail_test "charter-inert: the $label prompt has an odd number of double quotes ($n)"
  fi
done

# The invariant must never be satisfiable by deleting the instructions. These
# are the load-bearing spans -- the things the agent is actually told to run.
for needle in "story show $id --json" "story move $id done" "gh pr merge --merge" \
              "make test" "council-vote" "story block $id"; do
  case "$auto" in
    *"$needle"*) ;;
    *) fail_test "charter-inert: the auto charter no longer instructs '$needle' -- inertness must not be bought by removing the instructions" ;;
  esac
done
case "$attended" in
  *"story show $id --json"*) ;;
  *) fail_test "charter-inert: the attended prompt no longer instructs 'story show'" ;;
esac

finish
