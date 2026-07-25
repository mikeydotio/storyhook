#!/usr/bin/env bash
# story.sh — deterministic helper for the /story skill's `do` verb (GitHub
# issue #40).
#
# FORKED from mikeydotio/agentics' plugins/storywork/bin/story.sh (as of
# 2026-07-24, agentics plugin `storywork` v2.36.0) — itself already
# storyhook-flavored (claims via storyhook's `--if-state` CAS primitive,
# names worktrees/windows from a story id instead of a GitHub issue number)
# but built for `conductor`'s own daemon, which pre-filters for readiness
# via `story next`/`list --ready` before ever calling `dispatch`. issue #40
# needs `/story do` to enforce that same readiness check ITSELF, since a
# human can invoke it directly against any story id, ready or not. Sources
# this plugin's own vendored `lib/session.sh` (forked alongside this file —
# see that file's header) rather than agentics' `issue` plugin, so this
# plugin has no cross-marketplace-plugin runtime dependency.
#
# One subcommand, emitting exactly ONE JSON object on stdout with an `ok`
# boolean and a human-readable `display`, mirroring issue.sh/storywork's own
# contract:
#
#   dispatch <id>   Refuse unless <id> is READY (issue #40's core ask — see
#                    the READY-GATE step below) and not already claimed, then
#                    claim it via storyhook's CAS primitive, create a NEW git
#                    worktree at .claude/worktrees/<name> on branch
#                    worktree-<name>, open a new tmux window rooted IN that
#                    worktree, gate on claude becoming ready, then type +
#                    submit the handoff prompt.
#
# DELIBERATE DEVIATIONS from the agentics storywork.sh this was forked from:
#
#   1. ALREADY-IN-PROGRESS GUARD (new). storyhook's own `is_ready()` returns
#      true even for a story already `in-progress` — it only checks
#      open/unblocked, not "hasn't been claimed yet" (confirmed by direct
#      testing against the CLI: moving a story to in-progress does not drop
#      it from `story list --ready`). A readiness gate alone would therefore
#      NOT stop a redispatch of a story someone is already working. This
#      adds an explicit precondition: state == "in-progress" refuses before
#      any side effect, run BEFORE the ready-gate below.
#
#   2. READY-STATE GATE (new — the literal ask of issue #40). Queries
#      `story list --ready --json` (the same ground truth `story-work` and
#      every other interactive skill already rely on — reusing it here
#      avoids re-implementing `is_ready()`'s open/awaiting/obviated-by/
#      blocked-by logic in bash and risking drift from the Rust
#      implementation) and refuses if <id> isn't a member, building a
#      specific reason (closed / awaiting / obviated-by / blocked-by) from
#      the `show` data already in hand.
#
#   3. NO STORY_ALLOW_CLOSED escape hatch. storywork's own dispatch let a
#      caller force-dispatch onto a closed story via STORY_ALLOW_CLOSED,
#      folded into the same closed-superstate check the ready-gate above now
#      also performs — keeping both would mean ALLOW_CLOSED bypasses the
#      explicit check but the ready-gate then fails again for the same
#      reason regardless, an escape hatch that doesn't actually escape.
#      issue #40 asks for a hard refusal on a non-ready story, not a
#      bypassable one, so this fork drops the flag entirely: the ready-gate
#      is the SOLE, non-overridable readiness check. (STORY_REQUIRE_FRESH_BASE
#      is unrelated — a worktree-base-freshness concern, not a story-
#      readiness one — and is kept unchanged.)
#
#   4. SIMPLIFIED CLAIM LOGIC. storywork's `claim_needed` conditional exists
#      because ITS caller (conductor) may have already pre-claimed the story
#      via its own `--if-state` move before calling `story.sh dispatch`, so
#      the actuator has to tolerate "already in-progress, skip the move."
#      This script's only caller is a human via `/story do <id>` (or the
#      /story router), with no pre-claim step — deviation #1 above already
#      guarantees `state != "in-progress"` by the time the claim attempt
#      runs, so the CAS move is now unconditional (still guarded by
#      --if-state, still rolled back on a later hard failure).
#
# Everything else — worktree/tmux mechanics, the two/three-tier base-
# freshness handling, claim-rollback-on-later-failure, the CWD-vs-worktree-
# safe repo_root() — is unchanged from the agentics original.
set -euo pipefail

# Shared tmux/worktree/pane-readiness mechanics (window/worktree naming,
# git-safety helpers, the readiness gate, confirmed-send) live in
# ../lib/session.sh — this plugin's own vendored fork, not agentics'.
# shellcheck source=../lib/session.sh
source "$(dirname "${BASH_SOURCE[0]}")/../lib/session.sh"

# ---- config (all env-overridable) -------------------------------------------
STORY="${STORY_BIN:-story}"
# Launch command, run INSIDE the worktree dispatch already created — must NOT
# include `-w`/`--worktree` (would try to create a second worktree at the
# same path). <name> renders to the resolved window/worktree name, <n> to the
# story id, for a custom override that wants either.
LAUNCH_TPL="${STORY_LAUNCH_CMD:-claude --permission-mode plan --model opusplan}"
# The handoff prompt — the only lever the dispatcher has over the child
# session. Single-line + ASCII (no backticks) by default; delivery is via a
# bracketed paste (paste_prompt), so a multi-line STORY_PROMPT override is
# safe. Storyhook stories aren't GitHub issues: no label to apply, no
# `Closes #N` convention — the child instead posts its plan back via `story
# comment` and references the story id in every PR body.
PROMPT_TPL="${STORY_PROMPT:-Investigate and plan a fix for story <n> in this repo. Begin by reading it with \`story show <n> --json\` (its comments carry the discussion history). When your plan is finalized and approved, post it as a comment on <n> via \`story comment <n> \"<plan>\"\` before you start implementing. Ensure every pull request you open references story <n> in its body, and comment a link to each PR on <n> after you push it. Do not bump the version or deploy from this worktree: do not run semver bump, deployit deploy, or any release/version step, and do not plan for them -- versioning and deployment happen later from the main branch, not here.}"
# Extra clause a caller appends to the handoff prompt (daemon-caller seam).
# Appended VERBATIM with a single space separator, AFTER <n>/<name> templating.
PROMPT_EXTRA="${STORY_PROMPT_EXTRA:-}"
# New-window (and worktree) name override; supports the <n> placeholder.
WINDOW_NAME_TPL="${STORY_WINDOW_NAME:-}"
# Focus policy: the new window is created DETACHED (-d) by default.
FOREGROUND="${STORY_FOREGROUND:-}"
# Target tmux session for the dispatch window (non-interactive-caller seam).
TARGET_SESSION="${STORY_TARGET_SESSION:-}"
# Per-story git-worktree hygiene AND the worktree container itself.
WORKTREE_IGNORE_PATH="${STORY_WORKTREE_IGNORE_PATH:-.claude/worktrees/}"
WORKTREE_IGNORE_COMMENT="# story per-story git worktrees (ephemeral — never commit)"
# Readiness gate before typing the prompt — see lib/session.sh's wait_ready
# for the full two-tier rationale (marker footer match, or structural
# frame+glyph+stabilise fallback).
READY_PATTERN="${STORY_READY_PATTERN:-for shortcuts|for agents|mode on|to cycle}"
READY_ATTEMPTS="${STORY_READY_ATTEMPTS:-60}"
READY_DELAY="${STORY_READY_DELAY:-0.25}"
READY_FALLBACK_DELAY="${STORY_READY_FALLBACK_DELAY:-3}"
READY_STABLE_POLLS="${STORY_READY_STABLE_POLLS:-3}"
READY_FRAME_GLYPH="${STORY_READY_FRAME_GLYPH:-─}"
READY_PROMPT_GLYPH="${STORY_READY_PROMPT_GLYPH:-❯}"
READY_TAIL_LINES="${STORY_READY_TAIL_LINES:-8}"
CONFIRM_ATTEMPTS="${STORY_CONFIRM_ATTEMPTS:-8}"
CONFIRM_DELAY="${STORY_CONFIRM_DELAY:-0.3}"
SEND_RETRIES="${STORY_SEND_RETRIES:-2}"
PASTE_SETTLE_DELAY="${STORY_PASTE_SETTLE_DELAY:-0.2}"
READY_ACCEPT_PATTERN="${STORY_READY_ACCEPT_PATTERN:-esc to interrupt|Thinking|Crunching|tokens|to interrupt}"
CAPTURE_LINES="${STORY_CAPTURE_LINES:-200}"
DRY_RUN="${STORY_DRY_RUN:-}"
# State `complete` closes a story into. Empty means "resolve it from
# .storyhook/states.toml" (the first CLOSED-superstate entry) — see
# story_closed_state.
DONE_STATE="${STORY_DONE_STATE:-}"
# Escalate a non-fresh base (CACHED/HEAD-FALLBACK — see Step 6 below) from a
# warning to a hard ok:false. Unrelated to story readiness (see deviation #3
# above) — this is purely about the worktree's base commit.
REQUIRE_FRESH_BASE="${STORY_REQUIRE_FRESH_BASE:-}"

# Project root every `story` call is anchored to. Set once, from repo_root(),
# before the first story_cli use. Declared here so `set -u` is satisfied even
# on paths that fail before it is assigned.
PROJECT_DIR=""

require_story() {
  command -v "$STORY" >/dev/null 2>&1 \
    || fail "story CLI not found — build/install it from mikeydotio/storyhook (see story --help)."
}

# story_cli <args...> — run the story CLI anchored at $PROJECT_DIR (SH-46).
#
# The CLI has no --repo/-C/--cwd flag and ensure_project() does NOT walk up
# ancestors: the project root is always exactly env::current_dir(). Calling
# `story` in the caller's CWD therefore breaks two ways.
#
#   1. From a plain subdirectory it just fails (exit 3, "not initialized").
#   2. From inside a dispatched worktree it does something worse than fail --
#      it silently succeeds against the WRONG tracker. `.storyhook/` is
#      version-controlled, so every worktree carries its own independent copy.
#      repo_root() already anchors worktree CREATION to the main repo; without
#      this wrapper the ready-gate and CAS claim would be evaluated against a
#      different checkout than the one the worktree is made in.
#
# Runs in a subshell so the caller's CWD is never mutated (several later steps
# -- gitignore hygiene, `git worktree add` -- use paths relative to it).
story_cli() {
  ( CDPATH= cd -- "$PROJECT_DIR" && "$STORY" "$@" )
}

# valid_story_id <id> — a story id is interpolated verbatim into worktree
# paths and branch names (via resolve_wname), so it is validated at this
# boundary: non-empty, alphanumeric plus hyphen/underscore only (storyhook's
# own ids look like "STO-7"; this also rejects path-traversal/whitespace).
valid_story_id() {
  [[ "$1" =~ ^[A-Za-z0-9][A-Za-z0-9_-]*$ ]]
}

# repo_root — echo the MAIN worktree's absolute root directory, regardless of
# which worktree (or subdirectory of one) CWD is currently inside. `git
# rev-parse --git-common-dir` resolves to <main>/.git from ANY worktree of the
# repo (relative, ".git", only when CWD already IS the main worktree's own
# root), so its dirname — resolved to an absolute path via a real `cd` in a
# subshell, never the caller's shell — is stable no matter where CWD is.
#
# `dir` MUST use this instead of `git rev-parse --show-toplevel`:
# --show-toplevel is CWD-relative, so from inside one of THIS script's own
# worktrees it would return that worktree's root, not the main repo's — a
# `/story do` invoked from inside an existing dispatched worktree would then
# nest its new worktree inside it, matching nothing in `git worktree list`.
repo_root() {
  local common
  common=$(git rev-parse --git-common-dir 2>/dev/null) || return 1
  ( CDPATH= cd -- "$(dirname -- "$common")" && pwd -P )
}

# claim_rollback_note <id> <pre-claim-state> — cmd_dispatch's claim is a HARD
# PRECONDITION performed BEFORE any worktree/window side effect. If it
# succeeds and a LATER step still hard-fails (worktree/branch collision, `git
# worktree add`, `tmux new-window`), the story would otherwise be left
# permanently stuck at `in-progress` with no worktree, no window, and no
# session — silent. This attempts a best-effort CAS move BACK to
# <pre-claim-state>, guarded by --if-state in-progress so a genuine
# concurrent transition away from in-progress is never clobbered, and
# echoes a clause for the failure message naming the outcome either way.
claim_rollback_note() {
  local id="$1" pre_state="$2"
  local rb_json rb_result
  rb_json=$(story_cli move "$id" "$pre_state" --if-state in-progress --json 2>/dev/null) || true
  rb_result=$(printf '%s' "$rb_json" | jq -r '.result // ""' 2>/dev/null || printf '')
  if [ "$rb_result" = "ok" ]; then
    printf ' Rolled the claim back to `%s`.' "$pre_state"
  else
    printf ' WARNING: story %s is now stranded at `in-progress` with no worktree/window — run `story move %s %s --if-state in-progress` to un-stick it.' \
      "$id" "$id" "$pre_state"
  fi
}

# ready_gate_reason <show-json> — build a specific, best-effort explanation
# for why <id> isn't in `story list --ready`, from the `show` data already in
# hand. Not itself the gate (that's membership in `list --ready`, the CLI's
# own ground truth) — just a friendlier message than "not ready" alone.
ready_gate_reason() {
  printf '%s' "$1" | jq -r --arg id "$2" '
    .story.story as $s
    | if $s.superstate == "CLOSED" then
        "closed (superstate CLOSED)"
      elif ($s.awaiting // "") != "" then
        "awaiting: \"" + $s.awaiting + "\""
      elif ([$s.relationships[]? | select(.relation == "obviated-by")] | length) > 0 then
        "obviated by " + ([$s.relationships[] | select(.relation == "obviated-by") | .other_id] | join(", "))
      elif ([$s.relationships[]? | select(.relation == "blocked-by")] | length) > 0 then
        "has an unresolved blocked-by relationship to: "
          + ([$s.relationships[] | select(.relation == "blocked-by") | .other_id] | join(", "))
          + " (see `story show " + $id + "` for whether each is still open)"
      else
        "not in `story list --ready`"
      end'
}

# ---- subcommand: dispatch ---------------------------------------------------
cmd_dispatch() {
  local id="${1:-}"
  [ -n "$id" ] || fail "usage: story.sh dispatch <story-id>"
  valid_story_id "$id" || fail "story id must be alphanumeric (hyphens/underscores allowed) (got: $id)."

  # Step 1: tmux precondition (relaxed under dry-run and under
  # STORY_TARGET_SESSION — a non-interactive caller outside tmux dispatches
  # into a NAMED session, so its own tmux context is irrelevant).
  if [ -z "$DRY_RUN" ] && [ -z "$TARGET_SESSION" ]; then
    [ -n "${TMUX:-}" ] || fail "story requires tmux — run Claude inside a tmux session."
    [ -n "${TMUX_PANE:-}" ] || fail "story requires \$TMUX_PANE — run Claude inside a tmux pane."
  fi

  # Step 2: repo dir, anchored to the MAIN worktree via repo_root() — never
  # CWD-relative (see that function's header).
  local dir
  dir=$(repo_root) || fail "not inside a git repository."
  # Anchor every subsequent `story` call to the project root (SH-46) — see
  # story_cli's header for why the caller's CWD is not safe to inherit.
  PROJECT_DIR="$dir"

  # Step 3: story CLI present.
  require_story

  # Step 4: story exists. Every read here is REAL, even under dry-run
  # (issue.sh's own asymmetry: reads always run for real, only writes are
  # symbolic under dry-run).
  local show_json result title state
  show_json=$(story_cli show "$id" --json 2>/dev/null) || true
  result=$(printf '%s' "$show_json" | jq -r '.result // ""' 2>/dev/null || printf '')
  if [ "$result" != "ok" ]; then
    # `story show`'s own .error already reads "story `<id>` not found" on a
    # missing id, so use it directly rather than re-wrapping (which would
    # double the "not found" text); fall back to a generic message only if
    # .error itself is absent (e.g. the CLI emitted no JSON at all).
    fail "$(printf '%s' "$show_json" | jq -r --arg id "$id" '.error // ("story `" + $id + "` not found")' 2>/dev/null)"
  fi
  title=$(printf '%s' "$show_json" | jq -r '.story.story.title // ""')
  state=$(printf '%s' "$show_json" | jq -r '.story.story.state // ""')

  # Step 5 (deviation #1 — see header): ALREADY-IN-PROGRESS GUARD. Checked
  # before the ready-gate since `is_ready()` alone would not catch this —
  # an in-progress-but-unblocked story still reads as "ready".
  if [ "$state" = "in-progress" ]; then
    fail "story $id is already in-progress — not redispatching (a previous \`/story do\` may still be running against it, or move it back to a ready state first if this is stale)."
  fi

  # Step 6 (deviation #2 — see header): READY-STATE GATE, issue #40's core
  # ask. Ground-truth membership check against the CLI's own `is_ready()`,
  # via the exact command every interactive skill already relies on.
  local ready_json is_ready_flag
  ready_json=$(story_cli list --ready --json 2>/dev/null) || ready_json='{"stories":[]}'
  is_ready_flag=$(printf '%s' "$ready_json" | jq --arg id "$id" '([.stories[]?.story.id // empty] | index($id)) != null' 2>/dev/null || printf 'false')
  if [ "$is_ready_flag" != "true" ]; then
    local reason
    reason=$(ready_gate_reason "$show_json" "$id")
    fail "story $id is not ready to work on ($reason) — run \`story show $id\` for details."
  fi

  # claim target is unconditionally "in-progress" — deviation #4 (see
  # header): step 5 above already guarantees $state != "in-progress" here.
  local pre_claim_state="$state"

  if [ -z "$DRY_RUN" ]; then
    local move_json move_result
    move_json=$(story_cli move "$id" in-progress --if-state "$state" --json 2>/dev/null) || true
    move_result=$(printf '%s' "$move_json" | jq -r '.result // ""' 2>/dev/null || printf '')
    case "$move_result" in
      ok)
        state="in-progress" ;;
      conflict)
        refuse "claim-conflict" "story $id changed state before it could be claimed (expected \`$state\`, now \`$(printf '%s' "$move_json" | jq -r '.actual // "?"' 2>/dev/null)\`) — another dispatch likely won the race." ;;
      *)
        fail "story move $id in-progress failed: $(printf '%s' "$move_json" | jq -r '.error // "story move emitted no result"' 2>/dev/null)." ;;
    esac
  fi

  # Compute the name used for the tmux window, the worktree dir leaf, AND the
  # worktree branch: "<repo-prefix>-<id>" (e.g. "sto-STO-7"), or the
  # STORY_WINDOW_NAME override. repo_prefix's input need not be an
  # "owner/repo" string (storyhook has no GitHub-owner concept) — the git
  # toplevel directory's own basename is enough.
  local repo_name wname wt_container worktree_path worktree_branch
  repo_name="$(basename "$dir")"
  wname=$(resolve_wname "$id" "$repo_name")
  wt_container="${WORKTREE_IGNORE_PATH%/}"
  worktree_path="$dir/$wt_container/$wname"
  worktree_branch="worktree-$wname"

  local launch_cmd prompt
  launch_cmd=$(render_template "$LAUNCH_TPL" "$id" "$wname")
  prompt=$(render_template "$PROMPT_TPL" "$id" "$wname")
  [ -n "$PROMPT_EXTRA" ] && prompt="$prompt $PROMPT_EXTRA"

  local ignore_status
  ignore_status=$(worktree_ignore_status "$dir")

  local detach="-d "
  [ -n "$FOREGROUND" ] && detach=""
  local target=""
  [ -n "$TARGET_SESSION" ] && target="-t $TARGET_SESSION: "

  # Dry-run: all read-only checks above (including the claim precondition
  # checks) ran for real; the claim WRITE above was skipped ($DRY_RUN guards
  # it), so emit the planned commands SYMBOLICALLY and stop before any side
  # effect.
  if [ -n "$DRY_RUN" ]; then
    jq -n \
      --arg id "$id" --arg title "$title" --arg dir "$dir" \
      --arg wname "$wname" --arg launch "$launch_cmd" --arg prompt "$prompt" \
      --arg state "$state" \
      --arg ignore_status "$ignore_status" \
      --arg detach "$detach" --arg target "$target" \
      --arg wtpath "$worktree_path" --arg wtbranch "$worktree_branch" '
      {
        ok: true, dry_run: true,
        id: $id, title: $title, dir: $dir,
        window_name: $wname, prompt: $prompt, state: $state,
        worktree_branch: $wtbranch, worktree_path: $wtpath,
        gitignore: (if $ignore_status == "already-ignored" then "already-ignored" else "would-add" end),
        commands: [
          ("story move " + $id + " in-progress --if-state " + $state),
          ("git worktree add --no-track -b " + $wtbranch + " " + $wtpath + " <base-oid>"),
          ("tmux new-window " + $target + $detach + "-c " + $wtpath + " -n " + $wname + " -P -F #{pane_id}"),
          ("tmux send-keys -t <pane> -l " + $launch),
          "tmux send-keys -t <pane> Enter",
          ("printf %s " + $prompt + " | tmux load-buffer -b story-" + $id + " -"),
          ("tmux paste-buffer -p -d -b story-" + $id + " -t <pane>"),
          "tmux send-keys -t <pane> Enter"
        ],
        display: ("[story] DRY RUN for " + $id + " (" + $title
                  + "): would claim it via `story move " + $id + " in-progress --if-state " + $state
                  + "`, create worktree " + $wtpath + " (branch " + $wtbranch
                  + "), open a new tmux window named " + $wname
                  + ", and run the listed commands.")
      }'
    return 0
  fi

  # Step 7: idempotently gitignore the per-story worktree CONTAINER dir,
  # BEFORE the worktree materializes. Best-effort — never flips ok to false.
  local gitignore_result="already-ignored"
  if [ "$ignore_status" = "not-ignored" ]; then
    gitignore_result=$(append_worktree_ignore "$dir")
  fi

  # Step 8: fetch origin/<default> (best-effort, quiet) and resolve the
  # commit the new worktree will be based on. THREE tiers: FRESH (fetch ok +
  # ref resolves), CACHED (fetch failed but a prior origin/<default> ref
  # exists), HEAD-FALLBACK (origin/<default> has never resolved at all —
  # offline and never fetched). Deliberately NOT routed through
  # freshen_base_ref: that helper's documented contract is "any fetch
  # failure is swallowed" (it exists for branch_is_merged, which only needs
  # SOME usable ref, never freshness metadata) — inlining the fetch here and
  # capturing its OWN exit code is what lets base_fresh distinguish
  # "resolves" from "was just refreshed". Either way dispatch never blocks
  # on network. On any hard failure from here on, the claim above is rolled
  # back via claim_rollback_note so a failed dispatch never strands the
  # story at in-progress.
  local default fetch_rc=0
  default=$(default_branch)
  git fetch --quiet origin "+refs/heads/$default:refs/remotes/origin/$default" \
    >/dev/null 2>&1 || fetch_rc=$?

  local base_oid="" base_fresh=false base_note=""
  if base_oid=$(git rev-parse --verify --quiet "refs/remotes/origin/$default^{commit}" 2>/dev/null) \
     && [ -n "$base_oid" ]; then
    if [ "$fetch_rc" -eq 0 ]; then
      base_fresh=true
    else
      base_note="couldn't refresh origin/$default (offline?); based on last-known origin/$default @ ${base_oid:0:8}"
    fi
  elif base_oid=$(git rev-parse --verify --quiet 'HEAD^{commit}' 2>/dev/null) && [ -n "$base_oid" ]; then
    base_note="could not determine origin/$default; new work is based on the local checkout, NOT the latest origin tip"
    if [ -n "$REQUIRE_FRESH_BASE" ]; then
      fail "could not determine a fresh origin/$default and STORY_REQUIRE_FRESH_BASE is set — refusing to dispatch on a possibly-stale base.$(claim_rollback_note "$id" "$pre_claim_state")"
    fi
  else
    fail "cannot resolve a base commit for the new worktree (no origin/$default and HEAD has no commits).$(claim_rollback_note "$id" "$pre_claim_state")"
  fi

  # Step 9: create the worktree off the resolved base commit.
  if git show-ref --verify --quiet "refs/heads/$worktree_branch" || [ -e "$worktree_path" ]; then
    fail "a worktree or branch for \`$wname\` already exists — already dispatched?$(claim_rollback_note "$id" "$pre_claim_state")"
  fi
  local wt_err
  if ! wt_err=$(git worktree add --no-track -b "$worktree_branch" "$worktree_path" "$base_oid" 2>&1); then
    fail "failed to create worktree at $worktree_path: $(printf '%s' "$wt_err" | tail -n 2)$(claim_rollback_note "$id" "$pre_claim_state")"
  fi

  # Step 10: open the window (rooted IN the new worktree). A failure here
  # rolls back the just-created worktree/branch and the claim so a failed
  # dispatch leaves no litter.
  local new_window_args pane window
  new_window_args=(-c "$worktree_path" -n "$wname" -P -F '#{pane_id}')
  [ -z "$FOREGROUND" ] && new_window_args=(-d "${new_window_args[@]}")
  [ -n "$TARGET_SESSION" ] && new_window_args=(-t "$TARGET_SESSION:" "${new_window_args[@]}")
  if ! pane=$(tmux new-window "${new_window_args[@]}" 2>/dev/null) || [ -z "$pane" ]; then
    git worktree remove --force "$worktree_path" >/dev/null 2>&1 || true
    git worktree prune >/dev/null 2>&1 || true
    git branch -D "$worktree_branch" >/dev/null 2>&1 || true
    fail "failed to open a new tmux window.$(claim_rollback_note "$id" "$pre_claim_state")"
  fi
  window=$(tmux display-message -p -t "$pane" '#{window_id}' 2>/dev/null || printf '')

  # Pin the name before launching claude, so an early title escape can't win
  # the race.
  if [ -n "$window" ]; then
    tmux set-window-option -t "$window" automatic-rename off 2>/dev/null || true
    tmux set-window-option -t "$window" allow-rename off 2>/dev/null || true
  fi

  # Step 11: launch claude (literal mode). The worktree already exists (Step
  # 9), so the default launch omits `-w` entirely.
  paste_text "$pane" "$launch_cmd" || true
  tmux send-keys -t "$pane" Enter 2>/dev/null || true

  # Step 12: readiness gate before typing the prompt.
  local readiness_confirmed=false
  if wait_ready "$pane" "$launch_cmd"; then
    readiness_confirmed=true
  else
    sleep "$READY_FALLBACK_DELAY"
  fi

  # Step 13: type + submit the prompt, confirmed.
  local prompt_confirmed=false prompt_accepted_flag=false
  if send_prompt_confirmed "$pane" "$prompt" "story-$id"; then
    prompt_confirmed=true
    if prompt_accepted "$pane"; then
      prompt_accepted_flag=true
    fi
  fi

  # Result. ok:true from here on (the claim already succeeded above) — warn
  # on any unconfirmed step or a non-fresh base.
  local warning="" display base
  if [ "$readiness_confirmed" = true ] && [ "$prompt_confirmed" = true ]; then
    base="[story] $id ($title) → opened tmux window \`$wname\` on a worktree based on \`origin/$default\` @ \`${base_oid:0:8}\`, launched \`$launch_cmd\` (plan mode), submitted the prompt, and claimed it (now \`in-progress\`)."
  else
    if [ "$readiness_confirmed" = false ] && [ "$prompt_confirmed" = false ]; then
      warning="Couldn't confirm claude finished starting, nor that the prompt submitted — check window \`$wname\`."
    elif [ "$readiness_confirmed" = false ]; then
      warning="Couldn't confirm claude finished starting before the prompt was sent, but the prompt did submit — glance at window \`$wname\`."
    else
      warning="claude started, but couldn't confirm the prompt submitted — check window \`$wname\`."
    fi
    base="[story] $id ($title) → window \`$wname\` opened on a worktree based on \`origin/$default\` @ \`${base_oid:0:8}\`, but I couldn't fully confirm the handoff."
  fi
  if [ -n "$base_note" ]; then
    warning="${warning:+$warning }${base_note}."
  fi

  local tail_evidence=""
  if [ -n "$warning" ]; then
    display="$base $warning"
    tail_evidence=$(pane_tail "$pane")
  else
    display="$base"
  fi

  jq -n \
    --arg id "$id" --arg title "$title" --arg window "$window" --arg wname "$wname" \
    --arg pane "$pane" --arg state "$state" \
    --arg gitignore "$gitignore_result" \
    --argjson ready "$readiness_confirmed" --argjson pconf "$prompt_confirmed" \
    --argjson paccept "$prompt_accepted_flag" \
    --arg warning "$warning" --arg tail "$tail_evidence" --arg display "$display" \
    --arg default "$default" --arg base_oid "$base_oid" --argjson base_fresh "$base_fresh" \
    --arg wtbranch "$worktree_branch" --arg wtpath "$worktree_path" '
    {
      ok: true,
      id: $id, title: $title,
      window: $window, window_name: $wname, pane: $pane,
      state: $state,
      readiness_confirmed: $ready, prompt_confirmed: $pconf, prompt_accepted: $paccept,
      claimed: true, gitignore: $gitignore,
      base_branch: $default, base_ref: ("origin/" + $default),
      base_oid: $base_oid, base_fresh: $base_fresh,
      worktree_branch: $wtbranch, worktree_path: $wtpath
    }
    + (if $warning == "" then {} else {warning: $warning} end)
    + (if $tail == "" then {} else {pane_tail: $tail} end)
    + {display: $display}'
}

# ---- subcommand: complete ---------------------------------------------------
# FORKED from mikeydotio/agentics' plugins/storywork/bin/story.sh
# (cmd_complete / _story_worktree_status, as of 2026-07-25, agentics plugin
# `storywork` v2.36.0), with two deliberate deviations:
#
#   1. SPLIT INTO plan/execute. storywork exposes a single `complete <id>`
#      because its caller (conductor's daemon) needs no confirmation step.
#      A human-facing verb does: `plan` is a read-only preview the skill
#      shows before asking, `execute` acts. This mirrors issue.sh's own
#      `complete <plan|execute> <n>` split.
#   2. IT CLOSES THE STORY. storywork deliberately never touches story state
#      (conductor performs its own --if-state-guarded transition). But
#      `/issue complete` closes the issue, and parity is the ask here, so
#      `execute` moves the story into a CLOSED-superstate state. --no-close
#      and --no-clean give back each half independently, exactly as
#      issue.sh's do.
#
# The scan stays storywork's purpose-built SINGLE-TARGET lookup rather than a
# port of issue.sh's collect_targets: collect_targets also discovers the head
# branch of every merged PR that closed the issue, and storyhook has no such
# linkage — the worktree directory name is the sole story<->branch tie.

# story_closed_state — the slug `complete` moves a story into: the first
# CLOSED-superstate entry in .storyhook/states.toml, or $STORY_DONE_STATE.
# Not hard-coded to "done": states.toml is user-editable (this very repo
# defines five states, not the three `story init` seeds), and the CLI has no
# machine-readable "list states" verb to ask instead.
story_closed_state() {
  if [ -n "$DONE_STATE" ]; then printf '%s' "$DONE_STATE"; return 0; fi
  local file="$PROJECT_DIR/.storyhook/states.toml"
  [ -r "$file" ] || return 0
  awk '
    function val(s) { if (match(s, /"[^"]*"/)) return substr(s, RSTART + 1, RLENGTH - 2); return "" }
    /^[[:space:]]*\[\[states\]\]/ {
      if (slug != "" && sup == "CLOSED") { print slug; found = 1; exit }
      slug = ""; sup = ""; next
    }
    /^[[:space:]]*slug[[:space:]]*=/  { slug = val($0); next }
    /^[[:space:]]*super[[:space:]]*=/ { sup  = val($0); next }
    END { if (!found && slug != "" && sup == "CLOSED") print slug }
  ' "$file"
}

# _story_worktree_status <path> — removable|current|locked|dirty|missing.
#
# `current` uses --show-toplevel ON PURPOSE (unlike repo_root elsewhere): the
# question here is literally "is the caller standing inside the worktree it
# is asking us to delete", which is a CWD-relative question. A locked
# worktree is never removed and never unlocked — Claude Code's own worktree
# feature locks the ones it creates, and reclaiming those is not this verb's
# business.
_story_worktree_status() {
  local target="$1" cur locked
  cur=$(git rev-parse --show-toplevel 2>/dev/null || printf '')
  locked=$(git worktree list --porcelain 2>/dev/null | awk -v want="$target" '
    function flush() { if (p == want) print (l ? "1" : "0") }
    /^worktree / { flush(); p = substr($0, 10); l = 0 }
    /^locked/    { l = 1 }
    END          { flush() }')
  [ -n "$locked" ] || { printf 'missing'; return 0; }
  if [ "$target" = "$cur" ]; then printf 'current'; return 0; fi
  if [ "$locked" = "1" ]; then printf 'locked'; return 0; fi
  if [ -n "$(git -C "$target" status --porcelain 2>/dev/null)" ]; then printf 'dirty'; return 0; fi
  printf 'removable'
}

# _complete_prepare <id> — shared by plan and execute. Resolves the story,
# the target paths, and the classification of each, into caller-visible
# globals. Freshens origin/<default> exactly ONCE here so both verbs judge
# merged-ness against ground truth rather than a local <default> that a
# worktree-driven repo may never pull (a read, not a mutation — it runs even
# under dry-run, as issue.sh's cmd_complete_execute does).
_complete_prepare() {
  local id="$1"
  valid_story_id "$id" || fail "story id must be alphanumeric (hyphens/underscores allowed) (got: $id)."

  CMP_DIR=$(repo_root) || fail "not inside a git repository."
  PROJECT_DIR="$CMP_DIR"
  require_story

  local show_json result
  show_json=$(story_cli show "$id" --json 2>/dev/null) || true
  result=$(printf '%s' "$show_json" | jq -r '.result // ""' 2>/dev/null || printf '')
  if [ "$result" != "ok" ]; then
    fail "$(printf '%s' "$show_json" | jq -r --arg id "$id" '.error // ("story `" + $id + "` not found")' 2>/dev/null)"
  fi
  CMP_TITLE=$(printf '%s' "$show_json" | jq -r '.story.story.title // ""')
  CMP_STATE=$(printf '%s' "$show_json" | jq -r '.story.story.state // ""')
  CMP_SUPER=$(printf '%s' "$show_json" | jq -r '.story.story.superstate // ""')
  CMP_DONE_STATE=$(story_closed_state)

  local repo_name wt_container
  repo_name="$(basename "$CMP_DIR")"
  CMP_WNAME=$(resolve_wname "$id" "$repo_name")
  CMP_DEFAULT=$(default_branch)
  freshen_base_ref "$CMP_DEFAULT"

  wt_container="${WORKTREE_IGNORE_PATH%/}"
  CMP_WT_PATH="$CMP_DIR/$wt_container/$CMP_WNAME"
  CMP_WT_BRANCH="worktree-$CMP_WNAME"
  CMP_WT_STATUS=$(_story_worktree_status "$CMP_WT_PATH")

  # Branch classification. An un-comparable branch is NEVER treated as merged
  # (branch_is_merged returns false when neither origin/<default> nor local
  # <default> exists), so it is preserved rather than deleted.
  if ! local_branch_exists "$CMP_WT_BRANCH"; then
    CMP_BR_STATUS="missing"
  elif is_protected_branch "$CMP_WT_BRANCH"; then
    CMP_BR_STATUS="protected"
  elif branch_is_merged "$CMP_WT_BRANCH" "$CMP_DEFAULT"; then
    CMP_BR_STATUS="deletable"
  else
    CMP_BR_STATUS="unmerged"
  fi

  # Closing is an action only when the story is still open AND we resolved a
  # state to close it into.
  CMP_NEEDS_CLOSE=false
  if [ "$CMP_SUPER" != "CLOSED" ] && [ -n "$CMP_DONE_STATE" ]; then
    CMP_NEEDS_CLOSE=true
  fi

  CMP_ACTIONS=0
  [ "$CMP_WT_STATUS" = "removable" ] && CMP_ACTIONS=$((CMP_ACTIONS + 1))
  [ "$CMP_BR_STATUS" = "deletable" ] && CMP_ACTIONS=$((CMP_ACTIONS + 1))
  [ "$CMP_NEEDS_CLOSE" = true ] && CMP_ACTIONS=$((CMP_ACTIONS + 1))
  return 0
}

cmd_complete_plan() {
  local id="${1:-}"
  [ -n "$id" ] || fail "usage: story.sh complete plan <story-id>"
  shift
  [ "$#" -eq 0 ] || fail "usage: story.sh complete plan <story-id>"
  _complete_prepare "$id"

  jq -n \
    --arg id "$id" --arg title "$CMP_TITLE" --arg state "$CMP_STATE" \
    --arg super "$CMP_SUPER" --arg done_state "$CMP_DONE_STATE" \
    --arg default "$CMP_DEFAULT" --arg wtpath "$CMP_WT_PATH" \
    --arg wtstatus "$CMP_WT_STATUS" --arg branch "$CMP_WT_BRANCH" \
    --arg brstatus "$CMP_BR_STATUS" --argjson close "$CMP_NEEDS_CLOSE" \
    --argjson actions "$CMP_ACTIONS" '
    {
      ok: true, id: $id, title: $title, state: $state, superstate: $super,
      default_branch: $default,
      plan: {
        close: (if $close then { to: $done_state } else null end),
        worktree: { path: $wtpath, status: $wtstatus },
        branch:   { name: $branch, status: $brstatus }
      },
      actions_count: $actions,
      display: (
        "[story] complete " + $id + " — plan (" + ($actions|tostring) + " action(s)):\n"
        + "  story:    " + $state + " (" + $super + ")"
          + (if $close then " -> would close as `" + $done_state + "`" else " -> already closed, nothing to do" end) + "\n"
        + "  worktree: " + $wtpath + " [" + $wtstatus + "]"
          + (if $wtstatus == "removable" then " -> would remove"
             elif $wtstatus == "missing" then " -> nothing to remove"
             else " -> PRESERVED" end) + "\n"
        + "  branch:   " + $branch + " [" + $brstatus + "]"
          + (if $brstatus == "deletable" then " -> would delete (merged into " + $default + ")"
             elif $brstatus == "missing" then " -> nothing to delete"
             elif $brstatus == "unmerged" then " -> PRESERVED (not merged into " + $default + ")"
             else " -> PRESERVED (" + $brstatus + ")" end)
      )
    }'
}

cmd_complete_execute() {
  local id="${1:-}"
  [ -n "$id" ] || fail "usage: story.sh complete execute <story-id> [--no-close] [--no-clean]"
  shift
  local no_close="" no_clean=""
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --no-close) no_close=1; shift ;;
      --no-clean) no_clean=1; shift ;;
      *) fail "unknown argument \`$1\` — usage: story.sh complete execute <story-id> [--no-close] [--no-clean]" ;;
    esac
  done
  _complete_prepare "$id"

  local -a removed_wt=() removed_bl=() failed=() skipped=() commands=()
  local closed=false close_note=""

  # Close FIRST, and best-effort: a close failure is reported as a note, never
  # ok:false — the cleanup below is still worth doing and still succeeded.
  if [ -n "$no_close" ]; then
    skipped+=("close:$id(--no-close)")
  elif [ "$CMP_NEEDS_CLOSE" != true ]; then
    if [ -z "$CMP_DONE_STATE" ]; then
      close_note=" Could not close: no CLOSED-superstate state is defined in .storyhook/states.toml."
    fi
  elif [ -n "$DRY_RUN" ]; then
    commands+=("story move $id $CMP_DONE_STATE")
    closed=true
  else
    local mv_json mv_result
    mv_json=$(story_cli move "$id" "$CMP_DONE_STATE" --json 2>/dev/null) || true
    mv_result=$(printf '%s' "$mv_json" | jq -r '.result // ""' 2>/dev/null || printf '')
    if [ "$mv_result" = "ok" ]; then
      closed=true
    else
      close_note=" Could not close $id: $(printf '%s' "$mv_json" | jq -r '.error // "story move emitted no result"' 2>/dev/null)"
    fi
  fi

  if [ -n "$no_clean" ]; then
    skipped+=("cleanup:$CMP_WNAME(--no-clean)")
  else
    case "$CMP_WT_STATUS" in
      removable)
        if [ -n "$DRY_RUN" ]; then
          commands+=("git worktree remove $CMP_WT_PATH" "git worktree prune")
          removed_wt+=("$CMP_WT_PATH")
        elif git worktree remove "$CMP_WT_PATH" >/dev/null 2>&1; then
          # No --force: git itself refuses a dirty or current worktree, and
          # that veto is a feature, not an obstacle to route around.
          git worktree prune >/dev/null 2>&1 || true
          removed_wt+=("$CMP_WT_PATH")
        else
          failed+=("worktree:$CMP_WT_PATH")
        fi ;;
      missing) : ;;  # already gone — not an error.
      *) skipped+=("worktree:$CMP_WT_PATH($CMP_WT_STATUS)") ;;
    esac

    case "$CMP_BR_STATUS" in
      deletable)
        if [ -n "$DRY_RUN" ]; then
          commands+=("git branch -d $CMP_WT_BRANCH")
          removed_bl+=("$CMP_WT_BRANCH")
        elif delete_merged_local_branch "$CMP_WT_BRANCH" "$CMP_DEFAULT"; then
          removed_bl+=("$CMP_WT_BRANCH")
        else
          failed+=("branch:$CMP_WT_BRANCH(local)")
        fi ;;
      missing) : ;;
      protected) failed+=("branch:$CMP_WT_BRANCH(protected)") ;;
      *) skipped+=("branch:$CMP_WT_BRANCH($CMP_BR_STATUS)") ;;
    esac
  fi

  local rwt rbl fail_json skip_json cmds_json='[]'
  rwt=$(printf '%s\n' "${removed_wt[@]:-}" | jq -R -s 'split("\n")|map(select(length>0))')
  rbl=$(printf '%s\n' "${removed_bl[@]:-}" | jq -R -s 'split("\n")|map(select(length>0))')
  fail_json=$(printf '%s\n' "${failed[@]:-}" | jq -R -s 'split("\n")|map(select(length>0))')
  skip_json=$(printf '%s\n' "${skipped[@]:-}" | jq -R -s 'split("\n")|map(select(length>0))')
  [ -n "$DRY_RUN" ] && cmds_json=$(printf '%s\n' "${commands[@]:-}" | jq -R -s 'split("\n")|map(select(length>0))')

  jq -n --arg id "$id" --argjson rwt "$rwt" --argjson rbl "$rbl" \
        --argjson failed "$fail_json" --argjson skipped "$skip_json" \
        --argjson cmds "$cmds_json" --argjson closed "$closed" \
        --arg done_state "$CMP_DONE_STATE" --arg note "$close_note" \
        --argjson dry "$([ -n "$DRY_RUN" ] && echo true || echo false)" '
    {
      ok: true, id: $id, closed: $closed,
      removed: { worktrees: $rwt, branches: $rbl }
    }
    + (if $closed then {closed_as: $done_state} else {} end)
    + (if $dry then {dry_run:true, commands:$cmds} else {} end)
    + (if ($failed|length) > 0 then {failed:$failed} else {} end)
    + (if ($skipped|length) > 0 then {skipped:$skipped} else {} end)
    + { display: (
        "[story] " + (if $dry then "DRY RUN for " else "" end) + "complete " + $id + ": "
        + (if $closed then (if $dry then "would close as `" else "closed as `" end) + $done_state + "`, " else "" end)
        + (if $dry then "would remove " else "removed " end) + ($rwt|length|tostring) + " worktree(s), "
        + ($rbl|length|tostring) + " branch(es)."
        + (if ($failed|length) > 0 then " Could not: " + ($failed|join(", ")) + "." else "" end)
        + (if ($skipped|length) > 0 then " Preserved: " + ($skipped|join(", ")) + "." else "" end)
        + $note
      ) }'
}

cmd_complete() {
  local sub="${1:-}"
  shift 2>/dev/null || true
  case "$sub" in
    plan)    cmd_complete_plan "$@" ;;
    execute) cmd_complete_execute "$@" ;;
    *)       fail "usage: story.sh complete <plan|execute> <story-id>" ;;
  esac
}

# ---- router -----------------------------------------------------------------
case "${1:-}" in
  dispatch) shift; cmd_dispatch "$@" ;;
  complete) shift; cmd_complete "$@" ;;
  *)        fail "usage: story.sh <dispatch <story-id> | complete <plan|execute> <story-id>>" ;;
esac
