#!/usr/bin/env bash
# Autonomous approval hook for storyhook (Claude Code and Codex).
#
# Decision D6 of docs/spec/full-auto-engine.md. Inside an engine lane there is
# nobody at the prompt, so two tool calls have to be answered by something other
# than a person: the plan exit, which is normally approved by hand, and the
# question, which normally waits for one. This hook allows the plan tool and
# refuses questions at provider events. Dispatch separately arms an exact-pane
# watcher for each provider before it submits the autonomous charter, because
# Claude Code 2.1.261 no longer emits the PermissionRequest event that used to
# trigger its watcher. A changed or missing UI receives no input and remains
# safely stopped.
#
#   PreToolUse: ExitPlanMode              -> allow the tool call
#   AskUserQuestion | request_user_input  -> deny, with feedback the model reads
#   anything else, or a payload it cannot read -> no decision
#
# Both providers share this vocabulary. SH-459 measured the Codex arm live
# against CLI 0.149.0: a PreToolUse matcher named `request_user_input` runs
# before the question UI, and `permissionDecisionReason` is returned to the
# model as the blocking reason.
#
# INERT UNLESS AN AUTONOMOUS DISPATCH SAYS OTHERWISE. `STORYHOOK_AUTO` marks an
# ordinary `dispatch --auto`; `STORYHOOK_FULL_AUTO` remains the engine lane
# marker. Auto-approving plans in a developer's own session would be a far
# worse defect than the stall this prevents, so inertness is a tested property --
# tests/test-full-auto-inert.sh, in the shape test-charter-inert.sh already
# tests the charter's. Unset AND set-but-empty are both inert: a launcher that
# computed the marker and got nothing must not activate a session that does not
# exist.
#
# The selected variable carries the session's STORY ID, which the feedback names. A
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
# NO `set -e`, DELIBERATELY. For hook events the exit status IS a decision
# channel -- the host acts on a nonzero exit, and 2 blocks the call outright. A
# stray failing command must never get to decide that; every path below ends in
# an explicit `exit 0` and the decision travels in the JSON. This is SH-355's
# rule ("a hook that annotates must never decide") one host over: there it was
# git obeying prepare-commit-msg's status, here it is the agent host obeying
# this one.
#
# THE KNOWN HOLE, STATED RATHER THAN PAPERED OVER. Provider hooks fail OPEN at
# their manifest timeout. If this hook times out, or a provider changes the exact
# review-pane text, the agent asks, nobody answers, and the session stalls. An
# engine lane's ceiling quarantines it with a reason naming exactly that; an
# ordinary Auto window remains available for operator capture. The same
# reasoning governs an unreadable payload below: a hook
# that cannot tell which tool it is must not decide. Detected and reported, not
# silent, which is the bar.
set -uo pipefail

# Dispatch-owned continuation for Claude's plan review. The original pane pid
# and tmux's dead flag prevent a retained or respawned pane from inheriting this
# watch; three exact visible strings scope what it may type into.
approve_claude_plan() {
  local pane="${1:-}" expected_pid="${2:-}" limit="${3:-0}"
  local attempt=0 screen="" identity=""

  [ -n "${STORYHOOK_AUTO:-}${STORYHOOK_FULL_AUTO:-}" ] || return 0
  [[ "$pane" =~ ^%[0-9]+$ ]] || return 0
  [[ "$expected_pid" =~ ^[1-9][0-9]*$ ]] || return 0
  [[ "$limit" =~ ^[0-9]+$ ]] || limit=0
  command -v tmux >/dev/null 2>&1 || return 0

  while :; do
    identity=$(tmux display-message -p -t "$pane" \
      '#{pane_pid}:#{pane_dead}' 2>/dev/null) || return 0
    [ "$identity" = "$expected_pid:0" ] || return 0
    screen=$(tmux capture-pane -p -t "$pane" 2>/dev/null) || return 0
    if [[ "$screen" == *"Ready to code?"* \
       && "$screen" == *"❯ 1. Yes, and use auto mode"* \
       && "$screen" == *"2. Yes, manually approve edits"* ]]; then
      identity=$(tmux display-message -p -t "$pane" \
        '#{pane_pid}:#{pane_dead}' 2>/dev/null) || return 0
      [ "$identity" = "$expected_pid:0" ] || return 0
      tmux send-keys -t "$pane" Enter >/dev/null 2>&1 || true
      return 0
    fi
    attempt=$((attempt + 1))
    [ "$limit" -eq 0 ] || [ "$attempt" -lt "$limit" ] || return 0
    sleep 1
  done
}

# Codex 0.149.0 exposes no hook event before its separate plan-review UI, so
# dispatch starts this watcher only after it has confirmed Codex Plan mode and
# only for an autonomous child. The pane lifetime is the bound: long plans are
# supported, a dead/replaced pane ends the watcher, and exact option text gates
# the single Return. The optional attempt limit exists for deterministic tests;
# production passes zero (no artificial wall-clock deadline).
approve_codex_plan() {
  local pane="${1:-}" limit="${2:-0}" attempt=0 screen=""

  [ -n "${STORYHOOK_AUTO:-}${STORYHOOK_FULL_AUTO:-}" ] || return 0
  [[ "$pane" =~ ^%[0-9]+$ ]] || return 0
  [[ "$limit" =~ ^[0-9]+$ ]] || limit=0
  command -v tmux >/dev/null 2>&1 || return 0

  while :; do
    screen=$(tmux capture-pane -p -t "$pane" 2>/dev/null) || return 0
    if [[ "$screen" == *"Implement this plan?"* \
       && "$screen" == *"› 1. Yes, implement this plan"* \
       && "$screen" == *"2. Yes, clear context and implement"* \
       && "$screen" == *"3. No, stay in Plan mode"* ]]; then
      tmux send-keys -t "$pane" Enter >/dev/null 2>&1 || true
      return 0
    fi
    attempt=$((attempt + 1))
    [ "$limit" -eq 0 ] || [ "$attempt" -lt "$limit" ] || return 0
    sleep 1
  done
}

if [ "${1:-}" = "--approve-claude-plan" ]; then
  approve_claude_plan "${2:-}" "${3:-}" "${4:-0}"
  exit 0
fi
if [ "${1:-}" = "--approve-codex-plan" ]; then
  approve_codex_plan "${2:-}" "${3:-0}"
  exit 0
fi

# Drained before anything else, as the sibling hooks do, so the provider is
# never writing into a pipe this script has already walked away from.
stdin_json=""
if ! [ -t 0 ]; then
  stdin_json=$(cat)
fi

# The inert path pays no interpreter start: it is the path every attended
# session takes, on every plan exit and every question a human can answer.
if [ -z "${STORYHOOK_AUTO:-}${STORYHOOK_FULL_AUTO:-}" ]; then
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
    marker = os.environ.get("STORYHOOK_AUTO", "").strip()
if not marker:
    emit("{}")

story_id = marker if STORY_ID.match(marker) else ""
label = " " + story_id if story_id else ""
target = story_id if story_id else "this session's story"

try:
    payload = json.loads(os.environ.get("FULL_AUTO_PAYLOAD", ""))
except (TypeError, ValueError):
    emit("{}")

if not isinstance(payload, dict):
    emit("{}")

# The matcher already selected this call, and the payload is checked anyway. That
# keeps the script correct under a wildcard matcher -- the difference between
# deciding on the payload and deciding on having been invoked (SH-355).
event = payload.get("hook_event_name")
tool = payload.get("tool_name")

if event != "PreToolUse":
    emit("{}")

if tool == PLAN_EXIT:
    emit(
        envelope(
            "allow",
            "Autonomous Storyhook session%s: nobody is at the prompt, so this plan is approved "
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
            "This is an unattended Storyhook session; nobody can answer. If the "
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
