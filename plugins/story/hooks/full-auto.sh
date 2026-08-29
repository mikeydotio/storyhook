#!/usr/bin/env bash
# Full Auto PreToolUse hook for storyhook (Claude Code and Codex).
#
# Decision D6 of docs/spec/full-auto-engine.md. Inside an engine lane there is
# nobody at the prompt, so two tool calls have to be answered by something other
# than a person: the plan exit, which is normally approved by hand, and the
# question, which normally waits for one. This hook answers both natively, per
# tool call. The alternative -- watching the pane and typing the approval -- is
# screen-scraping a TUI, which is what SH-226 cost this project.
#
#   ExitPlanMode                          -> allow
#   AskUserQuestion | request_user_input  -> deny, with feedback the model reads
#   anything else, or a payload it cannot read -> no decision
#
# Both providers share this vocabulary. SH-459 measured the Codex arm live
# against CLI 0.149.0: a PreToolUse matcher named `request_user_input` runs
# before the question UI, and `permissionDecisionReason` is returned to the
# model as the blocking reason.
#
# INERT UNLESS A LANE SAYS OTHERWISE. `STORYHOOK_FULL_AUTO` is the whole of the
# activation, and only the engine sets it (SH-461). Auto-approving plans in a
# developer's own session would be a far worse defect than the stall this
# prevents, so inertness is a tested property rather than a claim --
# tests/test-full-auto-inert.sh, in the shape test-charter-inert.sh already
# tests the charter's. Unset AND set-but-empty are both inert: a launcher that
# computed the marker and got nothing must not activate a lane that does not
# exist.
#
# The variable carries the lane's STORY ID, which the feedback then names. A
# value that is not shaped like one still enforces -- it just falls back to
# generic wording rather than echoing a bogus id at the model.
#
# THE PLUGIN KILL SWITCH IS DELIBERATELY NOT CONSULTED. `[plugin] enabled =
# false` turns off the session hooks, which inject context and write handoffs;
# both are conveniences. Unattendedness is not, and a second switch that could
# silently turn a lane back into an attended one would re-open the exact failure
# this hook exists to close, in the one configuration nobody would think to
# check.
#
# NO `set -e`, DELIBERATELY. For PreToolUse the exit status IS a decision
# channel -- the host acts on a nonzero exit, and 2 blocks the call outright. A
# stray failing command must never get to decide that; every path below ends in
# an explicit `exit 0` and the decision travels in the JSON. This is SH-355's
# rule ("a hook that annotates must never decide") one host over: there it was
# git obeying prepare-commit-msg's status, here it is the agent host obeying
# this one.
#
# THE KNOWN HOLE, STATED RATHER THAN PAPERED OVER. PreToolUse fails OPEN at its
# manifest timeout on both hosts -- measured for Claude in SH-306 and for Codex
# in SH-459. If this hook times out, the agent asks, nobody answers, and the
# lane stalls; the engine's stall ceiling quarantines it with a reason naming
# exactly that. The same reasoning governs an unreadable payload below: a hook
# that cannot tell which tool it is must not decide. Detected and reported, not
# silent, which is the bar.
set -uo pipefail

# Drained before anything else, as the sibling hooks do, so the provider is
# never writing into a pipe this script has already walked away from.
stdin_json=""
if ! [ -t 0 ]; then
  stdin_json=$(cat)
fi

# The inert path pays no interpreter start: it is the path every session but a
# lane takes, on every plan exit and every question a human is there to answer.
if [ -z "${STORYHOOK_FULL_AUTO:-}" ]; then
  printf '{}'
  exit 0
fi

# The payload travels in the environment rather than on stdin because stdin is
# how the here-document below reaches python3.
read -r -d '' FULL_AUTO_PY <<'PY'
import json
import os
import re
import sys

PLAN_EXIT = "ExitPlanMode"
QUESTION_TOOLS = ("AskUserQuestion", "request_user_input")
# The reserved shape of a storyhook id: a project prefix and a number.
STORY_ID = re.compile(r"^[A-Za-z][A-Za-z0-9]*-[0-9]+$")


def emit(text):
    sys.stdout.write(text)
    raise SystemExit(0)


def envelope(decision, reason):
    return json.dumps(
        {
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": decision,
                "permissionDecisionReason": reason,
            }
        }
    )


marker = os.environ.get("STORYHOOK_FULL_AUTO", "").strip()
if not marker:
    emit("{}")

lane = marker if STORY_ID.match(marker) else ""
label = " " + lane if lane else ""
target = lane if lane else "this lane's story"

try:
    payload = json.loads(os.environ.get("FULL_AUTO_PAYLOAD", ""))
except (TypeError, ValueError):
    emit("{}")

if not isinstance(payload, dict):
    emit("{}")

# The matcher already selected this call, and the payload is asked anyway. That
# keeps the script correct under a wildcard matcher -- the difference between
# deciding on the payload and deciding on having been invoked (SH-355).
if payload.get("hook_event_name") != "PreToolUse":
    emit("{}")

tool = payload.get("tool_name")

if tool == PLAN_EXIT:
    emit(
        envelope(
            "allow",
            "Full Auto lane%s: nobody is at the prompt, so this plan is approved "
            "automatically. Post it as a comment on %s before you start "
            "implementing." % (label, target),
        )
    )

if tool in QUESTION_TOOLS:
    # Mirrors AUTO_COUNCIL_CLAUSE in bin/story.sh, including its self-healing
    # tail: SH-219 already established that naming the fallback in the sentence
    # is what makes the instruction safe where /council-vote is unavailable, so
    # this hook needs no on-disk probe of its own on a per-tool-call budget.
    emit(
        envelope(
            "deny",
            "This is an unattended Full Auto lane; nobody can answer. If the "
            "question has one clear best answer, research current best practice "
            "and decide it yourself. If two or more are genuinely defensible, "
            "convene /council-vote instead of asking. If /council-vote is "
            "unavailable to you, or the council aborts without a decision, do "
            "not stall: choose the one you can best defend. Either way, record "
            "the decision as a comment on %s the moment you make it, before you "
            "resume the work." % target,
        )
    )

emit("{}")
PY

decision=$(FULL_AUTO_PAYLOAD="$stdin_json" python3 -c "$FULL_AUTO_PY" 2>/dev/null) || decision=""

# Anything that is not an object is not a decision. A python3 that is missing,
# that failed, or that printed a diagnostic collapses to the same silence as a
# tool this hook has no opinion about.
case "$decision" in
  "{"*) printf '%s' "$decision" ;;
  *) printf '{}' ;;
esac
exit 0
