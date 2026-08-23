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
# SH-229 did exactly that once: SH-226 (the process-identity gate below —
# rendered characters alone can't prove a pane is running Claude, a bare
# shell prompt can satisfy both readiness tiers), SH-239 (pane_runs
# recognising the launch binary by identity, not just by name — a
# version-named install, e.g. Claude Code's native installer, defeats any
# fixed name pattern), and SH-263 (the paired fake tmux's shared default
# state directory, corruptible by two concurrent test files) all landed here
# FIRST and were ported to agentics' own plugins/issue/lib/session.sh as
# AGE-83 (mikeydotio/agentics#172). The two sides are aligned as of that PR;
# the deliberate divergences above (the STORY_PROTECTED_BRANCHES rename, the
# bare-id resolve_wname) were not touched by the port and remain intentional.
# Since it's still two forks and not one shared file, this can drift again —
# AGE-83's own header carries the same note pointing back here.
#
# A second deliberate divergence, added for SH-166: the agentics original's
# resolve_wname always prefixes with a repo-derived stem (agentics' `issue`
# plugin names windows from bare GitHub issue numbers like "118", which
# genuinely collide across repos sharing one tmux session). storyhook ids
# already carry their own project prefix (SH-, LIL-, TST-, ...) and don't
# collide that way, so this fork's resolve_wname returns the bare id
# instead. (SH-176 removed the pre-SH-166 legacy-name recognition this
# divergence originally shipped alongside, once no `<repo-prefix>-*`
# worktree remained anywhere using this plugin.)
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
#   READY_PROCESS_PATTERN    wait_ready, pane_runs
#   READY_LAUNCH_BIN         pane_runs — the launch command's FIRST WORD (not
#                            the whole command line). May be empty, which
#                            simply disables the identity half of pane_runs.
#   READY_TAIL_LINES         pane_tail
#   READY_ACCEPT_PATTERN     prompt_accepted
#   CONFIRM_ATTEMPTS         poll_input
#   CONFIRM_DELAY            poll_input
#   SEND_RETRIES             send_prompt_confirmed
#   SUBMIT_KEY               send_prompt_confirmed
#   EMPTY_INPUT_PATTERN      input_state (provider-rendered empty placeholder)
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

# refuse_with <reason> <message> <json-object> — refuse(), plus diagnostic fields
# merged in from <json-object>. A dispatch that gets far enough to open a window
# has evidence worth carrying (which pane, what was running in it, what the pane
# said), and refuse()'s fixed three-field shape has nowhere to put it. Separate
# from refuse() rather than a widened refuse() so no existing caller changes.
refuse_with() {
  jq -n --arg r "$1" --arg d "$2" --argjson extra "$3" \
    '{ok:false, reason:$r, display:$d} + $extra'
  exit 1
}

# ---- helpers ----------------------------------------------------------------
render_template() {  # render_template <template> <id> [<name>] [<dir>] [<reap>]
  # <n>    -> the story id (kept as "n" for parity with the agentics original)
  # <name> -> the resolved window/worktree name; empty when not passed.
  # <dir>  -> the main checkout's absolute path; empty when not passed.
  #           No shipped template uses it any more — the auto charter did,
  #           to anchor `story` writes at the tracker that actually held the
  #           story, and one store made that unnecessary. Kept because
  #           STORY_AUTO_PROMPT and STORY_PROMPT are user overrides and
  #           someone's may reference it.
  # <reap> -> the exact `story.sh reap <id>` command the caller resolved for
  #           this dispatch (SH-208); empty when not passed. Templated in
  #           rather than left for the child to reconstruct — an autonomous
  #           session knows neither this script's own path nor its project's
  #           slug reliably, and both are needed to self-reap correctly.
  local tpl="$1" n="$2" name="${3:-}" dir="${4:-}" reap="${5:-}"
  tpl="${tpl//<name>/$name}"
  tpl="${tpl//<dir>/$dir}"
  tpl="${tpl//<reap>/$reap}"
  printf '%s' "${tpl//<n>/$n}"
}

# resolve_wname <id> — the window/worktree/branch name for story <id>: the
# STORY_WINDOW_NAME override if set, else the bare id (e.g. SH-7). Shared by
# dispatch (to name the window/worktree it creates) and any future teardown,
# so both agree on the name. Since SH-166 this is bare — see this file's
# header for why storyhook ids don't need the repo-prefix disambiguation
# agentics' issue plugin does.
resolve_wname() {
  local n="$1"
  if [ -n "$WINDOW_NAME_TPL" ]; then
    render_template "$WINDOW_NAME_TPL" "$n"
  else
    printf '%s' "$n"
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
  local content box_text
  content=$(tmux capture-pane -p -t "$1" 2>/dev/null) || { printf 'unknown'; return; }
  box_text="$(input_box_text "$content")"
  case "$box_text" in
    *[![:space:]]*)
      if [ -n "${EMPTY_INPUT_PATTERN:-}" ] \
         && printf '%s' "$box_text" | grep -Eq -- "$EMPTY_INPUT_PATTERN"; then
        printf 'empty'
      else
        printf 'text'
      fi
      ;;
    *) printf 'empty' ;;
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

# pane_command <pane> — READ-ONLY. Echo the pane's FOREGROUND command as tmux
# reports it (`#{pane_current_command}`), or empty when it cannot be observed.
# This is the only fact on this path that comes from the process table rather
# than from rendered characters.
pane_command() {
  tmux display-message -p -t "$1" '#{pane_current_command}' 2>/dev/null || printf ''
}

# resolve_exe <command-word> — READ-ONLY. Echo the real path <command-word>
# runs: PATH lookup, then every symlink followed to the file itself. Empty (and
# non-zero) when it does not resolve to one.
#
# `readlink -f` is deliberately not used. It is a GNU extension that BSD only
# grew recently, and this file targets the bash 3.2 / BSD userland macOS ships;
# a hand-rolled chase costs six lines and works everywhere. The hop bound is
# what makes a symlink CYCLE terminate — a readiness gate that hangs is a
# readiness gate that has failed.
resolve_exe() {
  local target="$1" link dir hops=0
  [ -n "$target" ] || return 1
  target=$(command -v "$target" 2>/dev/null) || return 1
  # A builtin, function or alias answers `command -v` with something that is
  # not a path. Only a real file has an identity to compare against.
  case "$target" in /*) ;; *) return 1 ;; esac
  while [ -L "$target" ] && [ "$hops" -lt 32 ]; do
    link=$(readlink "$target" 2>/dev/null) || break
    [ -n "$link" ] || break
    case "$link" in
      /*) target="$link" ;;
      *)  dir="${target%/*}"; target="$dir/$link" ;;
    esac
    hops=$((hops + 1))
  done
  # A dangling link, or a chase that hit the hop bound mid-cycle, resolves to
  # nothing real. Fail rather than hand back a path to a file that isn't there:
  # every caller here is asking "is the occupant THIS binary", and a binary that
  # does not exist is not one anything can be running.
  [ -e "$target" ] || return 1
  printf '%s' "$target"
}

# launch_binary_path — resolve_exe over READY_LAUNCH_BIN, memoised for the poll
# loop (pane_runs is called once per READY_ATTEMPTS). Keyed on the input so a
# caller that changes READY_LAUNCH_BIN mid-process — cmd_doctor does, to probe
# its own launch template — is never answered from the previous one's cache.
_LAUNCH_BIN_KEY=""
_LAUNCH_BIN_PATH=""
launch_binary_path() {
  [ -n "$READY_LAUNCH_BIN" ] || return 1
  if [ "$_LAUNCH_BIN_KEY" != "$READY_LAUNCH_BIN" ]; then
    _LAUNCH_BIN_KEY="$READY_LAUNCH_BIN"
    _LAUNCH_BIN_PATH="$(resolve_exe "$READY_LAUNCH_BIN" || printf '')"
  fi
  [ -n "$_LAUNCH_BIN_PATH" ] || return 1
  printf '%s' "$_LAUNCH_BIN_PATH"
}

# pane_runs <pane> — 0 iff the pane's occupant is the launch binary.
# FAILS CLOSED: an occupant that cannot be observed is not a match, following
# branch_is_merged's precedent in this file — an un-establishable fact is never
# read as the permissive answer.
#
# Three independent ways to say yes, in cost order. PANE_RUNS_RULE records which
# one answered, so `story doctor` can report that the NAME check alone would
# have refused (SH-239) rather than leaving an operator to discover it at
# dispatch time:
#
#   pattern         the occupant NAME matches READY_PROCESS_PATTERN. The
#                   original check, unchanged, and still the common case.
#   launch-binary   the name IS the basename of the launch command's own
#                   resolved binary. Claude Code's native installer points
#                   ~/.local/bin/claude at ~/.local/share/claude/versions/
#                   <version>, and tmux's `#{pane_current_command}` reports the
#                   basename of the RESOLVED executable — so the occupant is
#                   called "2.1.228" and no fixed pattern can ever anticipate
#                   it. Asking whether it is the binary we launched is a
#                   question about identity, which survives every version bump;
#                   asking what it is called is a question about spelling,
#                   which does not.
#   sibling-version the name is version-shaped AND names a real executable in
#                   that same resolved directory. Covers update skew: the pane
#                   still executes the version it was launched from after the
#                   symlink has moved on, which is not hypothetical — it
#                   happened to a live session while SH-239 was being written.
#
# What none of them will do is admit a shell, which is the whole point of
# SH-226. `zsh` matches no pattern, is not the resolved binary, and is not
# version-shaped; the sibling rule additionally requires an existing executable
# in the install directory, so "looks like a version" cannot become a hole of
# its own.
PANE_RUNS_RULE="none"
pane_runs() {
  local cmd resolved base dir
  cmd="$(pane_command "$1")"
  cmd="${cmd##*/}"
  WAIT_READY_COMMAND="$cmd"
  PANE_RUNS_RULE="none"
  [ -n "$cmd" ] || return 1

  if printf '%s' "$cmd" | grep -Eq -- "$READY_PROCESS_PATTERN"; then
    PANE_RUNS_RULE="pattern"
    return 0
  fi

  resolved="$(launch_binary_path)" || return 1
  base="${resolved##*/}"
  if [ "$cmd" = "$base" ]; then
    PANE_RUNS_RULE="launch-binary"
    return 0
  fi

  # The skew rule applies ONLY to an install that names its binaries by version
  # in the first place. If the launcher resolved to a plainly-named file, a
  # version-shaped occupant sitting beside it says nothing about this install,
  # and admitting it would widen the gate for no reason.
  printf '%s' "$base" | grep -Eq '^[0-9]+(\.[0-9]+)*$' || return 1
  printf '%s' "$cmd" | grep -Eq '^[0-9]+(\.[0-9]+)*$' || return 1
  dir="${resolved%/*}"
  if [ -f "$dir/$cmd" ] && [ -x "$dir/$cmd" ]; then
    PANE_RUNS_RULE="sibling-version"
    return 0
  fi
  return 1
}

# wait_ready <pane> <launch-cmd> — poll until Claude is ready IN THE PANE,
# bounded by READY_ATTEMPTS. Every success requires BOTH:
#
#   1. The pane's foreground command matches READY_PROCESS_PATTERN — a fact from
#      the process table, which a shell prompt cannot fake; AND
#   2. one of two rendering tiers:
#      FAST:       launch_gone AND content matches the READY_PATTERN footer marker.
#      STRUCTURAL: launch_gone AND content has BOTH the frame rule and the idle
#                  prompt glyph AND has stabilised (byte-identical for
#                  READY_STABLE_POLLS consecutive comparisons).
#
# Condition 1 is SH-226. Both tiers used to rest on rendered characters alone,
# and a Powerlevel10k shell prompt supplies a frame rule and an idle glyph for
# free — so when a launch failed, this function affirmed in under a second and
# the caller typed an autonomous agent charter into zsh, which executed it. The
# check belongs HERE, in the predicate whose own contract claims to establish
# that Claude is ready, rather than in a caller: a caller-side check would leave
# this function still asserting something it does not test.
#
# The process check is queried only once a tier's other conditions already hold,
# so the common case costs one extra tmux round trip rather than READY_ATTEMPTS.
#
# On return: WAIT_READY_TIER is the tier that matched ("marker" | "structural",
# else "none"); WAIT_READY_COMMAND is the last occupant observed; and
# WAIT_READY_REASON is "ok", "wrong-process" (a tier matched but the occupant did
# not) or "timeout". Errors travel with context: "not ready" and "a shell is
# sitting in that pane" are different sentences and lead to different actions.
#
# launch_gone = "the pane's last non-blank line no longer ends with the launch
# command". Note this says nothing about WHY it left: a shell that answered
# `command not found` satisfies it exactly as a started Claude does, which is why
# it is not, and never was, evidence that claude started.
WAIT_READY_TIER="none"
WAIT_READY_COMMAND=""
WAIT_READY_REASON="timeout"
wait_ready() {
  local pane="$1" launch="$2" attempt=0 content last_line
  local prev='' stable=0 launch_gone
  WAIT_READY_TIER="none"
  WAIT_READY_COMMAND=""
  WAIT_READY_REASON="timeout"
  while [ "$attempt" -lt "$READY_ATTEMPTS" ]; do
    if content=$(tmux capture-pane -p -t "$pane" 2>/dev/null); then
      last_line=$(printf '%s\n' "$content" | grep -v '^[[:space:]]*$' | tail -1 || true)
      launch_gone=false
      if [[ "$last_line" != *"$launch" ]]; then launch_gone=true; fi

      # Tier 1 — broadened footer marker (returns immediately; no stabilise wait).
      if [ "$launch_gone" = true ] \
         && printf '%s' "$content" | grep -Eq -- "$READY_PATTERN"; then
        if pane_runs "$pane"; then
          WAIT_READY_TIER="marker"
          WAIT_READY_REASON="ok"
          return 0
        fi
        WAIT_READY_REASON="wrong-process"
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
          if pane_runs "$pane"; then
            WAIT_READY_TIER="structural"
            WAIT_READY_REASON="ok"
            return 0
          fi
          # The glyphs are there and stable, but a shell is what is rendering
          # them. Keep polling: claude may still be starting behind this pane.
          WAIT_READY_REASON="wrong-process"
          stable=0
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

# wait_ready_sentinel <pane> <captured-pid> <worktree-path> — poll until a
# Claude Code SessionStart hook has published a dispatch sentinel INSIDE
# <worktree-path> (SH-231, replacing the screen-scrape above for
# `cmd_dispatch`'s launch, which — since SH-230 — execs straight into the
# launch binary rather than typing it, so <captured-pid> is that binary's own
# pid, captured via `#{pane_pid}` right after `tmux new-window` returned).
# `cmd_doctor`'s own scratch-window self-test is NOT ported to this: it types
# into an interactive pane a human is watching, with no fresh dispatch
# worktree to scope a sentinel to, so `wait_ready` (above) stays exactly
# right for it — see this function's own commit message for why porting it
# anyway would be scope, not safety.
#
# EVERY SUCCESS REQUIRES ALL THREE, checked in this order:
#
#   1. Re-querying `#{pane_pid}` for <pane> still answers <captured-pid> —
#      the pane story.sh opened has not been repurposed (a respawn, a second
#      dispatch racing the same window name). remain-on-exit means tmux never
#      does this on its own, so a mismatch here is a real anomaly, not a
#      timing fluke, and is never silently accepted.
#   2. `kill -0 <captured-pid>` still succeeds — proven empirically (tmux
#      3.7b) to be load-bearing rather than redundant with (1): tmux freezes
#      `#{pane_pid}`/`#{pane_current_command}` at their LAST LIVE values once
#      the pane's process exits under remain-on-exit, so re-querying them
#      alone cannot detect death. `pid_is_live`'s own doc
#      (src/daemon/lifecycle.rs:1806) makes the same point about a bare pid
#      check versus a held lock; there is no lock here, so this AND with (3)
#      is what stands in for one.
#   3. The dispatch sentinel exists at
#      <worktree-path>/.claude/dispatch-sentinel.json, AND `pane_runs` — the
#      SAME process-table fact `wait_ready` gates on above — still matches
#      READY_PROCESS_PATTERN. A sentinel with the right pid dead or a live pid
#      that is not actually running the launch binary are both refused.
#
# Existence alone is NEVER sufficient (council verdict on SH-231): a sentinel
# is not a secret, and nothing stops a second,
# unrelated Claude Code session from being pointed at the same worktree path
# during the polling window and publishing an equally well-formed one. (1)
# and (2) are what make this dispatch's OWN pane the one thing being trusted,
# not the sentinel's content.
#
# WAIT_READY_REASON distinguishes which half failed, so a caller can name the
# actual remedy rather than a generic timeout:
#   ok           — all three held; ready.
#   pid-mismatch — the pane no longer shows <captured-pid> (or vanished
#                  entirely) — FAILS FAST, no reason to keep polling a pane
#                  that already isn't the one this dispatch opened.
#   pid-exited   — <captured-pid> is confirmed dead — FAILS FAST, same
#                  reasoning: the sentinel could still appear, but nothing
#                  would be alive to receive the prompt.
#   wrong-process — the sentinel exists but the pane's occupant does not
#                  match READY_PROCESS_PATTERN; keeps polling (claude may
#                  still be starting).
#   no-sentinel  — timed out with the pane alive and correct throughout, but
#                  no sentinel ever appeared (SessionStart hook missing,
#                  disabled, or never fired).
#
# WAIT_READY_COMMAND is the last observed occupant, refreshed every
# iteration regardless of which branch it ends up in — a launch that never
# became claude/node still leaves something diagnosable to report even on a
# no-sentinel timeout.
WAIT_READY_TIER="none"
WAIT_READY_COMMAND=""
WAIT_READY_REASON="timeout"
wait_ready_sentinel() {
  local pane="$1" captured_pid="$2" worktree="$3" attempt=0
  local sentinel_path="$worktree/.claude/dispatch-sentinel.json"
  WAIT_READY_TIER="none"
  WAIT_READY_COMMAND=""
  WAIT_READY_REASON="timeout"

  [ -n "$captured_pid" ] || { WAIT_READY_REASON="pid-mismatch"; return 1; }

  while [ "$attempt" -lt "$READY_ATTEMPTS" ]; do
    local current_pid
    current_pid=$(tmux display-message -p -t "$pane" '#{pane_pid}' 2>/dev/null || printf '')
    # Diagnostic only, and unconditional: whatever is actually running in the
    # pane right now, an operator investigating any failure below benefits
    # from being told what it is — the same "last observed occupant" contract
    # WAIT_READY_COMMAND has always carried for wait_ready above.
    WAIT_READY_COMMAND="$(pane_command "$pane")"
    WAIT_READY_COMMAND="${WAIT_READY_COMMAND##*/}"
    if [ -z "$current_pid" ] || [ "$current_pid" != "$captured_pid" ]; then
      WAIT_READY_REASON="pid-mismatch"
      return 1
    fi
    if ! kill -0 "$captured_pid" 2>/dev/null; then
      WAIT_READY_REASON="pid-exited"
      return 1
    fi
    if [ -f "$sentinel_path" ]; then
      if pane_runs "$pane"; then
        WAIT_READY_TIER="sentinel"
        WAIT_READY_REASON="ok"
        return 0
      fi
      WAIT_READY_REASON="wrong-process"
    else
      WAIT_READY_REASON="no-sentinel"
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
#   Phase B: send the provider's submit key and confirm SUBMISSION (the box
#            cleared); on a swallowed key re-send THAT KEY ALONE (bounded) —
#            never re-paste, which would
#            duplicate the prompt.
# A positive result REQUIRES having first observed the box hold the prompt, so an
# empty box from a never-arrived paste can't masquerade as submitted. Claude
# submits with Enter; current Codex submits with Tab. Returns 0 only once
# submission is confirmed.
#
# SH-226 reversed a rule that used to live here: "if receipt is never confirmed,
# Enter is still pressed once best-effort (never regress below the old 'always
# Enter')". Enter was pressed BEFORE `received` was consulted, so a return of 1
# could not distinguish "never sent" from "sent blind" — and against a pane that
# is not Claude, that stray Enter IS the submission. The old rule was written
# when the pane was assumed to be a ready TUI and a spare Enter was harmless.
# It is not harmless, so Phase B is now wholly conditional on receipt.
#
# On return, SEND_PROMPT_PHASE names WHICH phase was reached, so a caller can
# tell an undelivered handoff (nothing was typed — safe to roll back a claim)
# from an unconfirmed one (it may already be in front of a live agent — rolling
# back would hand the same story to a second session):
#   submitted            receipt AND submission confirmed (the 0 return)
#   received-unsubmitted the box held the text; submission never confirmed
#   undelivered          the text never reached the box; NO Enter was sent
SEND_PROMPT_PHASE="undelivered"
send_prompt_confirmed() {
  local pane="$1" text="$2" buf="$3" received=false try=0
  SEND_PROMPT_PHASE="undelivered"
  # Phase A — deliver + confirm receipt.
  while [ "$try" -le "$SEND_RETRIES" ]; do
    if paste_prompt "$pane" "$text" "$buf" && poll_input "$pane" text; then
      received=true
      break
    fi
    try=$((try + 1))
  done
  # Nothing landed in the box, so nothing is submitted: no submit key is sent.
  [ "$received" = true ] || return 1
  SEND_PROMPT_PHASE="received-unsubmitted"
  # Phase B — submit + confirm. Re-send the submit key alone (never re-paste).
  try=0
  while [ "$try" -le "$SEND_RETRIES" ]; do
    if tmux send-keys -t "$pane" "$SUBMIT_KEY" 2>/dev/null; then
      if poll_input "$pane" empty; then
        SEND_PROMPT_PHASE="submitted"
        return 0
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
