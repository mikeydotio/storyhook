#!/usr/bin/env bash
# session.sh — shared tmux/worktree/pane-readiness mechanics.
#
# FORKED from mikeydotio/agentics' plugins/issue/lib/session.sh (as of
# 2026-07-24, agentics plugin `issue` v2.36.0), which is itself the
# provider-agnostic core extracted from plugins/issue/bin/issue.sh: no
# GitHub-issue concepts (issue numbers, labels, closingIssuesReferences).
# It is the window/worktree naming, git-safety (merged-branch) helpers,
# two-tier readiness gate, and confirmed-send handoff that this plugin's
# own bin/story.sh needs.
#
# This is a DELIBERATE FORK, not a shared/sourced file: storyhook issue #40
# needs this plugin to be self-contained (no cross-marketplace-plugin
# runtime dependency on agentics' `issue` plugin being installed alongside
# it). Every function below is otherwise byte-identical to its agentics
# original. One deliberate rename while forking: the ISSUE_PROTECTED_BRANCHES
# env var name (below) becomes STORY_PROTECTED_BRANCHES — the agentics
# original's header flagged that exact name as "left open for a future
# actuator (e.g. story.sh) to decide," and since this is no longer a file
# shared across two plugins, there's no reason to keep the foreign prefix.
#
# Since this is a fork, not a symlink, it can drift from the agentics
# original over time — a future maintainer syncing a fix from one side
# should consider porting it to the other.
#
# Sourced, not executed: this file sets no shell options of its own (no
# `set -euo pipefail`) — it inherits whatever the sourcing caller already
# set. bin/story.sh sources this file immediately after its own `set -euo
# pipefail`, before its config block runs.
#
# External variable contract — every name below is READ by a function in
# this file but DEFINED ONLY BY THE CALLER (bin/story.sh's own config
# block). A caller must set all of them (even to bin/story.sh's own
# defaults) before invoking the corresponding function, or `set -u` raises
# an unbound-variable error the first time that function runs:
#
#   WINDOW_NAME_TPL          resolve_wname
#   READY_PATTERN            wait_ready
#   READY_ATTEMPTS           wait_ready
#   READY_DELAY              wait_ready
#   READY_STABLE_POLLS       wait_ready
#   READY_FRAME_GLYPH        wait_ready
#   READY_PROMPT_GLYPH       wait_ready, input_box_text, prompt_accepted
#   READY_TAIL_LINES         pane_tail
#   READY_ACCEPT_PATTERN     prompt_accepted
#   CONFIRM_ATTEMPTS         poll_input
#   CONFIRM_DELAY            poll_input
#   SEND_RETRIES             send_prompt_confirmed
#   PASTE_SETTLE_DELAY       paste_text, paste_prompt
#   CAPTURE_LINES            capture_pane_transcript (default only — callers
#                            may pass an explicit override as its $2)
#   WORKTREE_IGNORE_PATH     worktree_ignore_status, append_worktree_ignore
#   WORKTREE_IGNORE_COMMENT  append_worktree_ignore
#   STORY_PROTECTED_BRANCHES is_protected_branch — read DIRECTLY from the
#                            environment (unlike every other name above,
#                            which is a script-scoped local the caller sets
#                            from an env var of its own choosing), so this
#                            literal env var name is inherited as-is by any
#                            caller. Renamed from the agentics original's
#                            ISSUE_PROTECTED_BRANCHES on this fork — see the
#                            header note above.
#
# None of the variables above are declared in this file — declaring them
# here would just shadow whatever the caller set, which is exactly the
# hidden coupling this extraction is trying to keep visible rather than bury.

# ---- JSON emitters ----------------------------------------------------------
# fail <message> — emit {ok:false, display} and exit non-zero. The skill halts
# and shows `display`.
fail() {
  jq -n --arg d "$1" '{ok:false, display:$d}'
  exit 1
}

# refuse <reason> <message> — a GUARD rejection: {ok:false, reason, display} +
# non-zero exit. Distinct from fail() so callers can tell a guard veto (e.g. a
# lost claim race) from an ordinary error.
refuse() {
  jq -n --arg r "$1" --arg d "$2" '{ok:false, reason:$r, display:$d}'
  exit 1
}

# ---- helpers ----------------------------------------------------------------
render_template() {  # render_template <template> <id> [<name>] [<dir>]
  # <n>    -> the story id (kept as "n" for parity with the agentics original)
  # <name> -> the resolved window/worktree name; empty when not passed.
  # <dir>  -> the main checkout's absolute path (SH-62's auto charter uses
  #           this to anchor `story` writes at the tracker that actually holds
  #           the story — see story.sh's AUTO_PROMPT_TPL); empty when not
  #           passed, and no other template references it.
  local tpl="$1" n="$2" name="${3:-}" dir="${4:-}"
  tpl="${tpl//<name>/$name}"
  tpl="${tpl//<dir>/$dir}"
  printf '%s' "${tpl//<n>/$n}"
}

# repo_prefix <name> — the first 3 alphanumerics of <name>, lowercased (e.g.
# "storyhook" -> "sto"). The window/worktree naming stem. Tolerates a bare
# repo name (no "owner/" prefix) — storyhook has no GitHub-owner concept.
repo_prefix() {
  printf '%s' "${1##*/}" | tr -cd '[:alnum:]' | cut -c1-3 | tr '[:upper:]' '[:lower:]'
}

# resolve_wname <id> <repo-name> — the window/worktree name for story <id>:
# the STORY_WINDOW_NAME override if set, else "<repo-prefix>-<id>" (e.g.
# sto-STO-7). Shared by dispatch (to name the window/worktree it creates)
# and any future teardown, so both agree on the name.
resolve_wname() {
  local n="$1" repo="$2"
  if [ -n "$WINDOW_NAME_TPL" ]; then
    render_template "$WINDOW_NAME_TPL" "$n"
  else
    printf '%s-%s' "$(repo_prefix "$repo")" "$n"
  fi
}

# ---- git-safety helpers ------------------------------------------------------
# default_branch — the repo's default branch NAME (no "origin/"), from
# origin/HEAD, falling back to "main". NEVER a valid delete target.
default_branch() {
  local ref
  ref=$(git symbolic-ref --quiet refs/remotes/origin/HEAD 2>/dev/null) || ref=""
  if [ -n "$ref" ]; then printf '%s' "${ref##*/}"; else printf 'main'; fi
}

# is_protected_branch <branch> — true for the default branch, main/master, or any
# glob in STORY_PROTECTED_BRANCHES (space-separated). These are never deleted.
is_protected_branch() {
  local b="$1" d extra g
  d=$(default_branch)
  case "$b" in "$d"|main|master) return 0 ;; esac
  for g in ${STORY_PROTECTED_BRANCHES:-}; do
    case "$b" in $g) return 0 ;; esac
  done
  return 1
}

# local_branch_exists <branch>
local_branch_exists() { git show-ref --verify --quiet "refs/heads/$1"; }

# remote_branch_exists <branch>
remote_branch_exists() { [ -n "$(git ls-remote --heads origin "$1" 2>/dev/null)" ]; }

# freshen_base_ref <base> — BEST-EFFORT, quiet network refresh of the base
# branch's remote-tracking ref (refs/remotes/origin/<base>) so branch_is_merged
# compares against an up-to-date origin/<base>. Explicit refspec (with a
# leading + to match the default clone behaviour) guarantees the
# remote-tracking ref updates regardless of the remote's configured fetch
# refspecs. Offline / no-remote / any failure is swallowed — branch_is_merged
# then falls back to whatever refs already exist. NEVER mutates local
# branches, the index, or the worktree.
freshen_base_ref() {
  local base="$1"
  git fetch --quiet origin "+refs/heads/$base:refs/remotes/origin/$base" >/dev/null 2>&1 || true
}

# branch_is_merged <branch> <base> — true iff <branch>'s tip is an ancestor of a
# usable <base> ref, tested as the UNION of origin/<base> and local <base>: merged
# if it is an ancestor of EITHER. origin/<base> is checked FIRST because in a
# daemon-managed repo the merge lands on the remote and local <base> can lag
# indefinitely — callers freshen origin/<base> once per run (freshen_base_ref)
# before relying on this. If NEITHER base ref exists, returns FALSE — the safe
# default: an un-comparable branch is NOT considered merged, so it is never
# auto-deleted.
branch_is_merged() {
  local branch="$1" base="$2" ref
  for ref in "refs/remotes/origin/$base" "refs/heads/$base"; do
    if git show-ref --verify --quiet "$ref" \
       && git merge-base --is-ancestor "refs/heads/$branch" "$ref" 2>/dev/null; then
      return 0
    fi
  done
  return 1
}

# delete_merged_local_branch <branch> <base> — delete a LOCAL branch already
# classed deletable. Tries the gentle `git branch -d` first (which keeps
# git's own merged-into-HEAD/upstream backstop for the common fresh-base case); if
# that refuses AND our authoritative union check still says the branch is merged
# into a base ref (fresh origin/<base> or local <base>), escalate to
# `git branch -D`. Returns 0 on delete, non-zero otherwise.
delete_merged_local_branch() {
  local branch="$1" base="$2"
  git branch -d "$branch" >/dev/null 2>&1 && return 0
  branch_is_merged "$branch" "$base" || return 1
  git branch -D "$branch" >/dev/null 2>&1
}

# ---- input-box / readiness helpers -------------------------------------------
# input_box_text <content> — echo the trailing text of the ACTIVE input row (the
# LAST line bearing READY_PROMPT_GLYPH), box padding stripped. The input row, NOT
# the pane's last non-blank line: the real TUI (and the test fixtures) render a
# FOOTER *below* the input box, so the last non-blank line is the footer and never
# the prompt.
input_box_text() {
  local content="$1" row tail
  row=$(printf '%s\n' "$content" | grep -F -- "$READY_PROMPT_GLYPH" | tail -1) || row=""
  [ -n "$row" ] || { printf ''; return 0; }
  tail=${row##*"$READY_PROMPT_GLYPH"}   # everything after the last glyph
  tail=${tail//│/}                       # strip the box border (literal, mb-safe)
  printf '%s' "$tail"
}

# input_state <pane> — "text" (box holds unsubmitted input) | "empty" (idle box) |
# "unknown" (capture failed). "unknown" is DISTINCT from "empty" so a transient
# capture failure can never be misread as a submission confirmation.
input_state() {
  local content
  content=$(tmux capture-pane -p -t "$1" 2>/dev/null) || { printf 'unknown'; return; }
  case "$(input_box_text "$content")" in
    *[![:space:]]*) printf 'text' ;;
    *)              printf 'empty' ;;
  esac
}

# poll_input <pane> <text|empty> — poll input_state up to CONFIRM_ATTEMPTS times,
# CONFIRM_DELAY apart, for the box to reach <want>. 0 on reaching it, else 1.
poll_input() {
  local pane="$1" want="$2" attempt=0
  while [ "$attempt" -lt "$CONFIRM_ATTEMPTS" ]; do
    [ "$(input_state "$pane")" = "$want" ] && return 0
    sleep "$CONFIRM_DELAY"
    attempt=$((attempt + 1))
  done
  return 1
}

# wait_ready <pane> <launch-cmd> — poll until Claude's TUI is ready, bounded by
# READY_ATTEMPTS. Two tiers:
#   FAST:       launch_gone AND content matches the READY_PATTERN footer marker.
#   STRUCTURAL: launch_gone AND content has BOTH the frame rule and the idle
#               prompt glyph AND has stabilised (byte-identical for
#               READY_STABLE_POLLS consecutive comparisons).
# Either tier satisfied → success. On success, WAIT_READY_TIER is set to the tier
# that matched ("marker" | "structural"); a timeout leaves it "none".
#
# launch_gone = "the pane's last non-blank line no longer ends with the launch
# command", i.e. the typed launch command has left the input line (claude started).
WAIT_READY_TIER="none"
wait_ready() {
  local pane="$1" launch="$2" attempt=0 content last_line
  local prev='' stable=0 launch_gone
  WAIT_READY_TIER="none"
  while [ "$attempt" -lt "$READY_ATTEMPTS" ]; do
    if content=$(tmux capture-pane -p -t "$pane" 2>/dev/null); then
      last_line=$(printf '%s\n' "$content" | grep -v '^[[:space:]]*$' | tail -1 || true)
      launch_gone=false
      if [[ "$last_line" != *"$launch" ]]; then launch_gone=true; fi

      # Tier 1 — broadened footer marker (returns immediately; no stabilise wait).
      if [ "$launch_gone" = true ] \
         && printf '%s' "$content" | grep -Eq -- "$READY_PATTERN"; then
        WAIT_READY_TIER="marker"
        return 0
      fi

      # Tier 2 — structural frame + idle glyph + stabilisation. Increment the
      # stable counter ONLY when all structural preconditions hold AND this
      # capture is byte-identical to the previous one; any change (or a missing
      # precondition) resets it. `prev` starts empty, so the first identical pair
      # is the first comparison that can count.
      if [ "$launch_gone" = true ] \
         && printf '%s' "$content" | grep -qF -- "$READY_FRAME_GLYPH" \
         && printf '%s' "$content" | grep -qF -- "$READY_PROMPT_GLYPH" \
         && [ -n "$content" ] && [ "$content" = "$prev" ]; then
        stable=$((stable + 1))
        if [ "$stable" -ge "$READY_STABLE_POLLS" ]; then
          WAIT_READY_TIER="structural"
          return 0
        fi
      else
        stable=0
      fi
      prev="$content"
    else
      # Couldn't observe the pane — don't let a stale `prev` fake a stable streak.
      stable=0
      prev=''
    fi
    sleep "$READY_DELAY"
    attempt=$((attempt + 1))
  done
  return 1
}

# pane_tail <pane> — READ-ONLY. Echo the last READY_TAIL_LINES non-blank lines of
# the pane, for attaching to a warning result as diagnostic evidence. A failed
# capture echoes nothing (an empty tail is an acceptable degrade).
pane_tail() {
  local pane="$1" content
  content=$(tmux capture-pane -p -t "$pane" 2>/dev/null) || return 0
  printf '%s\n' "$content" | grep -v '^[[:space:]]*$' | tail -n "$READY_TAIL_LINES" || true
}

# prompt_accepted <pane> — READ-ONLY, NON-GATING. Best-effort check that a READY
# TUI consumed the just-submitted prompt: either a working/thinking indicator
# rendered (READY_ACCEPT_PATTERN) or the idle prompt glyph sits on an otherwise
# cleared input row. Returns 0 (accepted) / 1 (unconfirmed). Callers record the
# result as signal only — it must NEVER flip prompt_confirmed or trigger a resend.
prompt_accepted() {
  local pane="$1" content last_line
  content=$(tmux capture-pane -p -t "$pane" 2>/dev/null) || return 1
  if printf '%s' "$content" | grep -Eq -- "$READY_ACCEPT_PATTERN"; then
    return 0
  fi
  # Idle input row: the last non-blank line is just the prompt glyph (no trailing
  # user text), i.e. the input box cleared and re-rendered its empty prompt.
  last_line=$(printf '%s\n' "$content" | grep -v '^[[:space:]]*$' | tail -1 || true)
  case "$last_line" in
    *"$READY_PROMPT_GLYPH") return 0 ;;
    *) return 1 ;;
  esac
}

# paste_text <pane> <text> — literal-paste <text>, then SETTLE so a bracketed
# paste closes before any Enter. Used for typing a SINGLE-LINE command into a
# SHELL, where bracketed paste isn't guaranteed; the multi-line-safe prompt send
# uses paste_prompt below. Sends NO Enter. Returns non-zero if the paste send
# itself failed.
paste_text() {
  tmux send-keys -t "$1" -l "$2" 2>/dev/null || return 1
  sleep "$PASTE_SETTLE_DELAY"
}

# paste_prompt <pane> <text> <buffer> — deliver <text> into a Claude TUI as ONE
# bracketed paste, so an embedded newline stays TEXT instead of submitting the
# prompt at its first line (`send-keys -l` sends a newline as a literal Enter).
# Loads a private tmux buffer from stdin (no temp file), then pastes it with -p
# (bracketed-paste markers → the TUI buffers the whole paste and never submits
# mid-way) and -d (delete the private buffer after). No -r: tmux's default
# LF→CR matches what a real terminal sends on a human paste. Sends NO Enter; the
# settle preserves the settle-before-Enter invariant. Only the PROMPT uses this —
# the launch send types a single-line command into a shell and keeps paste_text.
# Returns non-zero if either tmux stage failed (caller then skips its receipt
# poll and retries).
paste_prompt() {
  local pane="$1" text="$2" buf="$3"
  printf '%s' "$text" | tmux load-buffer -b "$buf" - 2>/dev/null || return 1
  tmux paste-buffer -p -d -b "$buf" -t "$pane" 2>/dev/null || return 1
  sleep "$PASTE_SETTLE_DELAY"
}

# pane_for_window <window-name> — READ-ONLY. Echo the pane id of the (active) pane
# of the tmux window named <window-name>, searching every session on the server;
# empty if no such window. Prefers the active pane, falling back to the first.
pane_for_window() {
  local wname="$1"
  tmux list-panes -a -F '#{window_name}	#{pane_active}	#{pane_id}' 2>/dev/null \
    | awk -F'\t' -v w="$wname" '
        $1==w && $2==1 { print $3; found=1; exit }
        $1==w && !first { first=$3 }
        END { if (!found && first) print first }'
}

# capture_pane_transcript <target> [lines] — READ-ONLY. Echo the rendered
# scrollback of a tmux pane as plain text, from <lines> rows back to the bottom
# (default CAPTURE_LINES). `-p` prints without escape sequences. Returns non-zero
# if the capture failed (e.g. the target no longer exists).
capture_pane_transcript() {
  local target="$1" lines="${2:-$CAPTURE_LINES}"
  tmux capture-pane -p -t "$target" -S "-$lines" 2>/dev/null || return 1
}

# send_prompt_confirmed <pane> <text> <buffer> — two-phase confirmed handoff.
#   Phase A: paste the prompt (as ONE bracketed paste via <buffer>) and confirm
#            it was RECEIVED (the input box holds text); re-paste (bounded by
#            SEND_RETRIES) ONLY if nothing landed — never blind-repaste.
#   Phase B: send Enter and confirm SUBMISSION (the box cleared); on a swallowed
#            Enter re-send ENTER ALONE (bounded) — never re-paste, which would
#            duplicate the prompt.
# A positive result REQUIRES having first observed the box hold the prompt, so an
# empty box from a never-arrived paste can't masquerade as submitted. If receipt
# is never confirmed, Enter is still pressed once best-effort (never regress
# below the old "always Enter"), but the result is reported unconfirmed. Returns
# 0 only once submission is confirmed.
send_prompt_confirmed() {
  local pane="$1" text="$2" buf="$3" received=false try=0
  # Phase A — deliver + confirm receipt.
  while [ "$try" -le "$SEND_RETRIES" ]; do
    if paste_prompt "$pane" "$text" "$buf" && poll_input "$pane" text; then
      received=true
      break
    fi
    try=$((try + 1))
  done
  # Phase B — submit + confirm. Re-send Enter alone (never re-paste).
  try=0
  while [ "$try" -le "$SEND_RETRIES" ]; do
    if tmux send-keys -t "$pane" Enter 2>/dev/null; then
      if [ "$received" = true ]; then
        poll_input "$pane" empty && return 0
      else
        break
      fi
    fi
    try=$((try + 1))
  done
  return 1
}

# ---- worktree-ignore hygiene --------------------------------------------------
# worktree_ignore_status <dir> — READ-ONLY. Echo "already-ignored" when a git
# ignore rule already covers the worktree dir — the exact rule OR a broader one
# such as `.claude/` — else "not-ignored". `git check-ignore -q` returns 0 when a
# path is ignored and 1 when it is not; 1 is a legitimate answer here, NOT an
# error, so the call MUST be wrapped in `if` (a bare invocation would trip the
# `set -e` at the top of the sourcing caller). check-ignore reads ignore files
# only (independent of HEAD/index), so it works in a fresh repo with no commits
# and no .gitignore. Probe a child path so a directory rule (trailing `/`) matches.
worktree_ignore_status() {
  local dir="$1" base
  base="${WORKTREE_IGNORE_PATH%/}"
  if git -C "$dir" check-ignore -q "$base/probe" 2>/dev/null; then
    printf 'already-ignored'
  else
    printf 'not-ignored'
  fi
}

# append_worktree_ignore <dir> — idempotent, BEST-EFFORT write of the worktree
# ignore rule to <dir>/.gitignore. Uses the canonical gitignore-append pattern
# (trailing-newline fix, blank separator, comment, rule). Echoes "added" on
# success, "already-ignored" if the exact rule is already present, or
# "add-failed" on any write error — and NEVER aborts the caller (a failure
# degrades dispatch to an untracked worktree dir, not a hard failure).
append_worktree_ignore() {
  local dir="$1" gi base rule lead=""
  gi="$dir/.gitignore"
  base="${WORKTREE_IGNORE_PATH%/}"
  rule="$base/"
  # Self-idempotent exact-line guard. The caller already gates on check-ignore
  # (which also honors a broader rule); this keeps the writer safe on its own.
  if [ -f "$gi" ] && grep -qxF "$rule" "$gi" 2>/dev/null; then
    printf 'already-ignored'
    return 0
  fi
  if [ -s "$gi" ]; then
    # Terminate an unterminated final line first ($(...) strips the trailing
    # newline, so a non-empty last byte means the file does NOT end in one)...
    [ -n "$(tail -c1 "$gi" 2>/dev/null)" ] && lead=$'\n'
    # ...then add a blank separator only when the last line is non-blank.
    [ -n "$(tail -n1 "$gi" 2>/dev/null)" ] && lead="${lead}"$'\n'
  fi
  { printf '%s%s\n%s\n' "$lead" "$WORKTREE_IGNORE_COMMENT" "$rule" >>"$gi"; } 2>/dev/null \
    && printf 'added' || printf 'add-failed'
}
