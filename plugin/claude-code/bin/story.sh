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
# The autonomous charter `--auto` swaps in for PROMPT_TPL — plan approval stays
# the ONE human interaction; everything past it (ambiguity, testing, merge,
# closure, hard stops) is the child's own call. Closure goes through a plain
# `story move`, never `story complete`: that verb asks a question (fatal to an
# unattended run) and would try to remove the very worktree the child occupies.
#
# It used to open by anchoring every `story` write at the main checkout,
# because a worktree carried its own copy of the tracker and a write made
# against it was silently lost. That clause is gone with the copy: one store
# means the worktree and the main checkout are the same project, and telling an
# autonomous agent to `cd` elsewhere for every write was friction bought with a
# defect that no longer exists.
AUTO_PROMPT_TPL="${STORY_AUTO_PROMPT:-Investigate and plan a fix for story <n> in this repo. Begin by reading it with \`story show <n> --json\` (its comments carry the discussion history). This is an AUTONOMOUS session: the user approves your plan once and is then unavailable -- ask no further questions after that approval and never block waiting on input. When your plan is finalized and approved, post it as a comment on <n> before you start implementing. For every later decision without a single obvious answer, convene \`/council-vote\` instead of asking, and record the outcome as a comment on <n>. Run the full local suite with \`make test\` and confirm it passes before pushing; the pre-push hook enforces it. Open a pull request whose body references story <n>, comment the PR link on <n>, then merge it yourself with \`gh pr merge --merge\` -- a merge commit, the only method this org allows -- verify the merge actually landed, and delete the source branch. Merging yourself deliberately overrides the standing \"in a linked worktree, stop after opening the PR\" rule, for the merge only. Do not bump the version or deploy from this worktree: do not run semver bump, deployit deploy, or any release/version step, and do not plan for them -- versioning and deployment happen later from the main branch, not here. Once the merge lands, judge from your own record of the work whether <n> is genuinely complete: default to closing it with \`story move <n> done\` (or the CLOSED-superstate state this project uses, if not \`done\`), and do not run \`/story complete\`, which asks a question and would try to reclaim the worktree you are standing in. If your own output shows further PRs or testing are still needed, leave <n> open and comment naming exactly what remains. If you hit a hard stop a council vote cannot resolve -- red tests, a failing merge, an unresolvable conflict -- post a comment on <n> with full diagnostics, run \`story block <n> \"<reason>\"\`, leave the PR open and this worktree intact, and stop. Never merge past a hard stop.}"
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
# State `complete` closes a story into. Empty means "ask the CLI for the
# project's state catalog and take the first CLOSED-superstate entry" — see
# story_closed_state.
DONE_STATE="${STORY_DONE_STATE:-}"
# `doctor`'s throwaway readiness probe: what it launches, and the scratch
# window it launches into. Kept separate from LAUNCH_TPL so probing a build
# never depends on a dispatch-time override.
DOCTOR_LAUNCH_TPL="${STORY_DOCTOR_LAUNCH_CMD:-claude --permission-mode plan --model opusplan}"
DOCTOR_WINDOW_NAME="${STORY_DOCTOR_WINDOW_NAME:-story-doctor}"
# Set by _project_integrity; read by cmd_doctor.
_INTEGRITY_OK=true
_INTEGRITY_SUMMARY=""
# Escalate a non-fresh base (CACHED/HEAD-FALLBACK — see Step 6 below) from a
# warning to a hard ok:false. Unrelated to story readiness (see deviation #3
# above) — this is purely about the worktree's base commit.
REQUIRE_FRESH_BASE="${STORY_REQUIRE_FRESH_BASE:-}"

# Directory the WORKTREE verbs run `story` from: the main checkout, set from
# repo_root() by dispatch, capture and complete, which need one anyway for the
# worktree bookkeeping they do. Empty for every other verb — those run `story`
# where the caller stands and let it resolve. Declared here so `set -u` is
# satisfied on paths that fail before it is assigned.
PROJECT_DIR=""

# Project named by `--project <slug>` on story.sh's own command line, forwarded
# verbatim to every `story` call. Empty means "let the CLI decide", which is the
# ordinary case.
PROJECT_SLUG=""

require_story() {
  command -v "$STORY" >/dev/null 2>&1 \
    || fail "story CLI not found — build/install it from mikeydotio/storyhook (see story --help)."
}

# story_cli <args...> — run the story CLI for the project this invocation is
# about.
#
# **It no longer decides which project that is.** Deciding is the CLI's job now
# (SH-116, SH-119, SH-151): `--project`, `$STORYHOOK_PROJECT`, the nearest
# committed pointer file at or above the working directory, then the
# repository's registered origin — and a refusal naming all three if none
# answer. Every one of those is strictly better informed than a shell walk, and
# two of them a shell cannot perform at all: only storyhook can look up a
# registered origin.
#
# What this used to do was `cd` to repo_root() first, and that was not merely
# redundant — it OVERRODE the answer. In a monorepo with a project at the
# repository root and another in `service-b`, `story.sh view` run from
# `service-b` reported `story not found` and `story.sh list` listed the ROOT
# project's stories, silently, while the CLI standing in the same directory
# answered correctly. SH-151 made a sub-project own its own identity; anchoring
# threw that away again (SH-121).
#
# So $PROJECT_DIR is set only by the verbs that genuinely need a directory —
# dispatch, capture, complete — and it means "the main checkout", not "the
# project". When it is empty this runs where the caller stands.
#
# Runs in a subshell when it does cd, so the caller's CWD is never mutated
# (several later steps -- gitignore hygiene, `git worktree add` -- use paths
# relative to it).
story_cli() {
  if [ -n "$PROJECT_SLUG" ]; then
    set -- --project "$PROJECT_SLUG" "$@"
  fi
  if [ -n "$PROJECT_DIR" ]; then
    ( CDPATH= cd -- "$PROJECT_DIR" && "$STORY" "$@" )
  else
    "$STORY" "$@"
  fi
}

# _load_ready_stories — `story list --ready --json` into $_READY_JSON, or a hard
# failure naming why it could not be asked.
#
# **Never a default** (SH-163). Both callers used to write
# `|| ready_json='{"stories":[]}'`, which cannot tell "this project has nothing
# ready" from "storyhook cannot tell which project this is" — so `story.sh list`
# outside a repository answered `{"ok": true, "count": 0}`, "No ready stories to
# pick up", over a CLI that had exited 3 and named three ways out. For the tool
# whose whole job is handing an agent its next task, a refusal rendered as "there
# is no work" is the worst shape available.
#
# Under `--json` the CLI reports a refusal as a document on **stdout** —
# `{"result":"error","error":…,"exit_code":3}` — which is what cmd_view already
# reads. Stderr is captured separately and consulted second rather than merged,
# so a stray warning can never end up concatenated with the payload on the
# success path, and so a failure mode that does print to stderr still produces a
# message instead of a bare exit code.
#
# Sets a global rather than echoing, for the reason on _project_integrity: a
# caller using $(...) would run `fail` in a subshell, so the process would carry
# on with the refusal JSON captured in a variable instead of printed.
_READY_JSON=""
_load_ready_stories() {
  local err rc=0 diag
  err=$(mktemp "${TMPDIR:-/tmp}/story-ready.XXXXXX")
  _READY_JSON=$(story_cli list --ready --json 2>"$err") || rc=$?
  if [ "$rc" -ne 0 ]; then
    diag=$(printf '%s' "$_READY_JSON" | jq -r '.error // empty' 2>/dev/null || printf '')
    [ -n "$diag" ] || diag=$(jq -r '.error // empty' <"$err" 2>/dev/null || printf '')
    [ -n "$diag" ] || diag=$(head -c 2000 <"$err")
    rm -f "$err"
    _READY_JSON=""
    fail "cannot list ready stories: ${diag:-\`story list --ready\` exited $rc}"
  fi
  rm -f "$err"
}

# valid_story_id <id> — a story id is interpolated verbatim into worktree
# paths and branch names (via resolve_wname), so it is validated at this
# boundary: non-empty, alphanumeric plus hyphen/underscore only (storyhook's
# own ids look like "STO-7"; this also rejects path-traversal/whitespace).
#
# It has always accepted a bare "5", and since SH-118 the CLI resolves one — so
# passing shape is no longer enough. Everything downstream of the store must use
# canonical_story_id() below.
valid_story_id() {
  [[ "$1" =~ ^[A-Za-z0-9][A-Za-z0-9_-]*$ ]]
}

# canonical_story_id <show-json> <typed-id> — the id storyhook resolved the
# typed one to.
#
# Since SH-118 `story show 5` succeeds, so a verb that keeps using what the user
# typed names things storyhook does not: the ready-gate compares against
# canonical ids and would call a ready story unready, and a worktree dispatched
# as "5" could never be captured or completed as "SH-5", or the reverse. Every
# name this script derives — the tmux window, the worktree directory leaf, the
# branch — comes from the *response*, so the two forms cannot produce two
# worktrees for one story.
#
# Falls back to what was typed if the response has no id, which keeps the
# failure in the caller's own error path rather than here.
canonical_story_id() {
  local resolved
  resolved=$(printf '%s' "$1" | jq -r '.story.story.id // ""' 2>/dev/null || printf '')
  if [ -n "$resolved" ]; then printf '%s' "$resolved"; else printf '%s' "$2"; fi
}

# repo_root — echo the MAIN worktree's absolute root directory, regardless of
# which worktree (or subdirectory of one) CWD is currently inside. `git
# rev-parse --git-common-dir` resolves to <main>/.git from ANY worktree of the
# repo (relative, ".git", only when CWD already IS the main worktree's own
# root), so its dirname — resolved to an absolute path via a real `cd` in a
# subshell, never the caller's shell — is stable no matter where CWD is.
#
# **This answers "where does worktree bookkeeping happen?", never "which
# project is this?"** Its three callers all create, name or remove a git
# worktree, and the shape they need is a repository. Project selection belongs
# to the CLI (see story_cli), which knows things a shell walk cannot — a
# registered origin among them.
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
  # <story-id> may appear before or after --auto; anything past that (a
  # second positional, an unknown flag) is a hard fail rather than the
  # silent ignore this verb used to give a stray trailing token — matching
  # view/list/capture/doctor/complete-plan, which already reject extras.
  local id="" auto=""
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --auto) auto=1; shift ;;
      *)
        [ -z "$id" ] || fail "unexpected argument \`$1\` — usage: story.sh dispatch <story-id> [--auto]"
        id="$1"; shift ;;
    esac
  done
  [ -n "$id" ] || fail "usage: story.sh dispatch <story-id> [--auto]"
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
  # Every name below — the tmux window, the worktree leaf, the branch — and the
  # ready-gate's membership test are derived from $id, and the ready list holds
  # canonical ids. A bare `5` would therefore pass this step and then be called
  # unready. From here on, $id is what storyhook says it is.
  id=$(canonical_story_id "$show_json" "$id")
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
  # Called plainly, NOT via $(...): _load_ready_stories may `fail`, and a
  # command substitution would run that in a subshell.
  _load_ready_stories
  ready_json="$_READY_JSON"
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
  # worktree branch: the bare story id (e.g. "STO-7"), or the
  # STORY_WINDOW_NAME override (SH-166). Also compute the PRE-SH-166 legacy
  # form ("<repo-prefix>-<id>", e.g. "sto-STO-7") purely to detect a
  # worktree an old binary already dispatched under it — see the collision
  # check below. repo_name need not be an "owner/repo" string (storyhook has
  # no GitHub-owner concept) — the git toplevel directory's own basename is
  # enough.
  local repo_name wname legacy_name wt_container worktree_path worktree_branch
  repo_name="$(basename "$dir")"
  wname=$(resolve_wname "$id")
  legacy_name=$(legacy_wname "$id" "$repo_name")
  wt_container="${WORKTREE_IGNORE_PATH%/}"
  worktree_path="$dir/$wt_container/$wname"
  worktree_branch="worktree-$wname"

  local prompt_tpl="$PROMPT_TPL"
  [ -n "$auto" ] && prompt_tpl="$AUTO_PROMPT_TPL"
  local launch_cmd prompt
  launch_cmd=$(render_template "$LAUNCH_TPL" "$id" "$wname" "$dir")
  prompt=$(render_template "$prompt_tpl" "$id" "$wname" "$dir")
  [ -n "$PROMPT_EXTRA" ] && prompt="$prompt $PROMPT_EXTRA"

  # Surfaced in both the dry-run and real result so the skill can warn the
  # caller: an auto session's ONLY human interaction is plan approval, so
  # auto-accept-edits must be chosen there or a later Bash permission prompt
  # stalls the unattended run forever.
  local auto_note=""
  if [ -n "$auto" ]; then
    auto_note=" Autonomous (--auto): choose auto-accept edits at the plan-approval prompt, or a later permission prompt can stall the unattended run. From there it runs to completion on its own -- council-voting its open questions, merging its own PR, and closing $id itself."
  fi

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
      --argjson auto "$([ -n "$auto" ] && echo true || echo false)" --arg auto_note "$auto_note" \
      --arg wtpath "$worktree_path" --arg wtbranch "$worktree_branch" '
      {
        ok: true, dry_run: true,
        id: $id, title: $title, dir: $dir,
        window_name: $wname, prompt: $prompt, state: $state, auto: $auto,
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
                  + ", and run the listed commands." + $auto_note)
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

  # Step 9: create the worktree off the resolved base commit. The collision
  # check also covers the LEGACY (pre-SH-166) name: a story dispatched by an
  # old binary already has a worktree/branch on disk that this run would
  # otherwise not see (it only looks for the bare-id form) and would
  # therefore dispatch a SECOND worktree for the same story — refuse instead,
  # same as a same-scheme collision, and name it as the legacy form so the
  # caller knows to run against the old name (e.g. `/story complete` adopts
  # it automatically).
  if git show-ref --verify --quiet "refs/heads/$worktree_branch" || [ -e "$worktree_path" ]; then
    fail "a worktree or branch for \`$wname\` already exists — already dispatched?$(claim_rollback_note "$id" "$pre_claim_state")"
  fi
  if [ -n "$legacy_name" ] \
     && { git show-ref --verify --quiet "refs/heads/worktree-$legacy_name" || [ -e "$dir/$wt_container/$legacy_name" ]; }; then
    fail "a worktree or branch for \`$legacy_name\` already exists (the pre-SH-166 name for $id) — already dispatched?$(claim_rollback_note "$id" "$pre_claim_state")"
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
  base="$base$auto_note"
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
    --argjson auto "$([ -n "$auto" ] && echo true || echo false)" \
    --arg warning "$warning" --arg tail "$tail_evidence" --arg display "$display" \
    --arg default "$default" --arg base_oid "$base_oid" --argjson base_fresh "$base_fresh" \
    --arg wtbranch "$worktree_branch" --arg wtpath "$worktree_path" '
    {
      ok: true,
      id: $id, title: $title,
      window: $window, window_name: $wname, pane: $pane,
      state: $state, auto: $auto,
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

# ---- subcommands: view / list / create ---------------------------------------
# The storyhook analogues of issue.sh's cmd_view / cmd_list / cmd_create.
# Shapes are kept deliberately identical (`option{label,description}` on each
# list row, `display` on every result) so the skill's List->Pick and
# View + Offer flows are a direct port rather than a re-derivation.

# `project_root` used to live here: repo_root(), else a walk up for the
# committed pointer file, else the CWD. Every branch of it has been overtaken.
# The CLI performs the pointer walk itself and knows two things this one never
# could — `$STORYHOOK_PROJECT`, and whether a repository's origin is registered
# — while the repo_root() branch actively overrode a monorepo sub-project's own
# identity (SH-121). Deleted rather than narrowed: a guard that decides nothing
# is a guard whose next reader has to work out that it decides nothing.

# story_state_list — `story state list`, or empty if it cannot be asked.
#
# The project's states used to be read out of `.storyhook/states.toml`. Story
# data lives in storyhook's own store now and that file is not written, so the
# catalog is asked of the CLI — which was always the better source, since the
# rendering is the contract and the file was an implementation detail.
#
# Through story_cli, so it obeys `--project` and the same working directory as
# every other read; it used to call `story` directly with its own `cd`, which
# meant a `--project` this script was given could not reach it.
#
# Lines look like: `in-progress (OPEN, active) — 2 open — some description`.
story_state_list() {
  story_cli state list 2>/dev/null || true
}

# story_active_state — the slug meaning "claimed / being worked": the state
# carrying the `active` role, else "in-progress". Mirrors the convention
# story-work already follows.
story_active_state() {
  local found=""
  found=$(story_state_list | awk -F' *\\(' '
    /\(.*active.*\)/ { gsub(/^[[:space:]]+|[[:space:]]+$/, "", $1); print $1; exit }
  ')
  printf '%s' "${found:-in-progress}"
}

cmd_view() {
  local id="${1:-}"
  [ -n "$id" ] || fail "usage: story.sh view <story-id>"
  shift
  [ "$#" -eq 0 ] || fail "usage: story.sh view <story-id>"
  valid_story_id "$id" || fail "story id must be alphanumeric (hyphens/underscores allowed) (got: $id)."

  require_story

  local show_json result title state super
  show_json=$(story_cli show "$id" --json 2>/dev/null) || true
  result=$(printf '%s' "$show_json" | jq -r '.result // ""' 2>/dev/null || printf '')
  if [ "$result" != "ok" ]; then
    fail "$(printf '%s' "$show_json" | jq -r --arg id "$id" '.error // ("story `" + $id + "` not found")' 2>/dev/null)"
  fi
  id=$(canonical_story_id "$show_json" "$id")
  title=$(printf '%s' "$show_json" | jq -r '.story.story.title // ""')
  state=$(printf '%s' "$show_json" | jq -r '.story.story.state // ""')
  super=$(printf '%s' "$show_json" | jq -r '.story.story.superstate // ""')

  # Human rendering via the CLI's own non-JSON output (it already includes
  # comments, relationships and derived fields). Best-effort: metadata above
  # already succeeded, so a rendering failure degrades to a one-liner rather
  # than a hard fail.
  local body
  body=$(story_cli show "$id" 2>/dev/null || printf '')
  [ -n "$body" ] || body="$id — $title [$state]"

  jq -n --arg id "$id" --arg title "$title" --arg state "$state" \
        --arg super "$super" --arg display "$body" '
    {ok:true, id:$id, title:$title, state:$state, superstate:$super, display:$display}'
}

cmd_list() {
  [ "$#" -eq 0 ] || fail "usage: story.sh list"
  require_story

  # `story list --ready` is the ground truth the dispatch gate already uses.
  # Two deliberate narrowings on top of it, both because is_ready() answers
  # "is this workable", not "is this available to pick up":
  #   - already-claimed stories are dropped (is_ready() returns true for an
  #     in-progress story — the exact gap /story do's own guard exists for);
  #   - parents are dropped, matching `story next`'s has_children filter,
  #     since dispatching an epic is never what the user meant.
  local ready_json active
  # Ready stories first, and plainly: it may `fail`, so it cannot be called
  # through $(...). Asking before `story_active_state` is deliberate — that one
  # degrades quietly to `in-progress` when it cannot be answered, so it would
  # otherwise hide the very refusal this has to report.
  _load_ready_stories
  ready_json="$_READY_JSON"
  active=$(story_active_state)

  printf '%s' "$ready_json" | jq --arg active "$active" '
    [ .stories[]?
      | select(.story.state != $active)
      | select([.story.relationships[]? | select(.relation == "parent-of")] | length == 0)
    ] as $rows
    | {
        ok: true,
        count: ($rows | length),
        stories: [ $rows[] | {
          id: .story.id,
          title: .story.title,
          state: .story.state,
          priority: .story.priority,
          option: {
            label: .story.id,
            description: (.story.title // "(no title)")
          }
        } ],
        display: (
          if ($rows | length) == 0
          then "No ready stories to pick up."
          else "[story] " + ($rows | length | tostring) + " ready story(ies):\n"
               + ([ $rows[] | "  " + .story.id + " [" + (.story.priority // "none") + "] " + (.story.title // "(no title)") ] | join("\n"))
          end
        )
      }'
}

cmd_create() {
  local title="" desc="" desc_file="" stype="" priority="" labels=""
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --title)            title="${2:-}";      shift 2 || fail "--title needs a value." ;;
      --description)      desc="${2:-}";       shift 2 || fail "--description needs a value." ;;
      --description-file) desc_file="${2:-}";  shift 2 || fail "--description-file needs a value." ;;
      --type)             stype="${2:-}";      shift 2 || fail "--type needs a value." ;;
      --priority)         priority="${2:-}";   shift 2 || fail "--priority needs a value." ;;
      --label|--labels)   labels="${2:-}";     shift 2 || fail "--label needs a value." ;;
      *) fail "unknown argument \`$1\` — usage: story.sh create --title <t> [--description-file <p> | --description <t>] [--type <slug>] [--priority <level>] [--label <csv>]" ;;
    esac
  done
  [ -n "$title" ] || fail "usage: story.sh create --title <t> [--description-file <p> | --description <t>] [--type <slug>] [--priority <level>] [--label <csv>]"

  # A description reaches the CLI as ONE argv element. `story new` has no
  # --description-file (unlike `gh issue create --body-file`), so the skill
  # writes the drafted markdown to a file and this reads it back — argv is
  # safe for arbitrary multi-line text, it is *shell quoting* that is not,
  # and a file keeps the markdown from ever passing through a command string.
  if [ -n "$desc_file" ]; then
    [ -r "$desc_file" ] || fail "description file not readable: $desc_file"
    desc=$(cat "$desc_file")
  fi

  require_story

  local -a args=(new "$title")
  [ -n "$stype" ]    && args+=(--type "$stype")
  [ -n "$priority" ] && args+=(--priority "$priority")
  [ -n "$labels" ]   && args+=(--labels "$labels")
  [ -n "$desc" ]     && args+=(--description "$desc")

  if [ -n "$DRY_RUN" ]; then
    jq -n --arg title "$title" --argjson cmds \
      "$(printf '%s\n' "story ${args[*]:0:1} <title>${stype:+ --type $stype}${priority:+ --priority $priority}${labels:+ --labels $labels}${desc:+ --description <text>}" | jq -R -s 'split("\n")|map(select(length>0))')" '
      {ok:true, dry_run:true, title:$title, commands:$cmds,
       display:("[story] DRY RUN — would create a story titled: " + $title)}'
    return 0
  fi

  local out result id
  out=$(story_cli "${args[@]}" --json 2>/dev/null) || true
  result=$(printf '%s' "$out" | jq -r '.result // ""' 2>/dev/null || printf '')
  if [ "$result" != "ok" ]; then
    # Deliberately NOT retried by the caller: a repeated create files a
    # duplicate story. The skill is told to report and stop.
    fail "failed to create the story: $(printf '%s' "$out" | jq -r '.error // "story new emitted no result"' 2>/dev/null)"
  fi
  id=$(printf '%s' "$out" | jq -r '.story.story.id // ""')

  jq -n --arg id "$id" --arg title "$title" '
    {ok:true, id:$id, title:$title,
     display:("[story] created " + $id + " — " + $title)}'
}

# ---- subcommands: doctor / capture ------------------------------------------
# FORKED from agentics' plugins/issue/bin/issue.sh (cmd_doctor / cmd_capture,
# as of 2026-07-25, issue plugin v2.36.0).
#
# capture is a straight port: it exists to read a dispatched session's pane,
# and pane_for_window/capture_pane_transcript were already vendored into
# lib/session.sh for exactly this — until now they were the only functions in
# the file with no caller.
#
# doctor deviates. `/issue doctor` is purely a readiness-tier drift probe, but
# the story CLI ALSO has a `story doctor` (project data integrity), so a
# `/story doctor` that ran only the tmux probe would surprise anyone who knows
# the CLI. This runs both and reports them together.

cmd_capture() {
  if [ -z "$DRY_RUN" ]; then
    [ -n "${TMUX:-}" ] || fail "story capture requires tmux — run Claude inside a tmux session."
  fi
  local id="${1:-}"
  [ -n "$id" ] || fail "usage: story.sh capture <story-id>"
  shift
  [ "$#" -eq 0 ] || fail "usage: story.sh capture <story-id>"
  valid_story_id "$id" || fail "story id must be alphanumeric (hyphens/underscores allowed) (got: $id)."

  local dir wname legacy_name
  dir=$(repo_root) || fail "not inside a git repository."

  # The one verb that otherwise reads nothing, and it still has to pay for one
  # read: the window it is looking for was named by `dispatch` from the
  # *canonical* id, so `story.sh capture 5` would otherwise hunt for a window
  # that no dispatch has ever created (SH-118). Tolerant on purpose — capture's
  # job is to find a tmux window, so a store that cannot answer leaves the id as
  # typed and lets the "no live tmux window" message below do the reporting,
  # rather than turning a capture into a story lookup failure.
  PROJECT_DIR="$dir"
  if command -v "$STORY" >/dev/null 2>&1; then
    id=$(canonical_story_id "$(story_cli show "$id" --json 2>/dev/null || printf '')" "$id")
  fi
  wname=$(resolve_wname "$id")
  legacy_name=$(legacy_wname "$id" "$(basename "$dir")")

  if [ -n "$DRY_RUN" ]; then
    jq -n --arg wname "$wname" --arg lines "$CAPTURE_LINES" '
      {
        ok: true, dry_run: true, window_name: $wname,
        commands: [ ("tmux capture-pane -p -t <pane-of " + $wname + "> -S -" + $lines) ],
        display: ("[story] DRY RUN capture: would dump the last " + $lines
                  + " rendered rows of the window " + $wname + ".")
      }'
    return 0
  fi

  # Look for the bare-id window first; fall back to the PRE-SH-166 legacy
  # name only if that fails, and adopt whichever name actually found a
  # window (SH-166) -- a story dispatched by an old binary still has a
  # live window, just not under $wname.
  local pane transcript legacy_used=false
  pane=$(pane_for_window "$wname")
  if [ -z "$pane" ] && [ -n "$legacy_name" ]; then
    pane=$(pane_for_window "$legacy_name")
    if [ -n "$pane" ]; then
      wname="$legacy_name"
      legacy_used=true
    fi
  fi
  if [ -z "$pane" ]; then
    if [ -n "$legacy_name" ]; then
      fail "no live tmux window named \`$wname\` or \`$legacy_name\` (its pre-SH-166 name) — dispatch it first with \`/story do $id\`."
    else
      fail "no live tmux window named \`$wname\` — dispatch it first with \`/story do $id\`."
    fi
  fi
  transcript=$(capture_pane_transcript "$pane") \
    || fail "failed to capture pane \`$pane\` (window \`$wname\`)."

  jq -n --arg id "$id" --arg wname "$wname" --arg pane "$pane" --arg tx "$transcript" \
    --argjson legacy "$legacy_used" '
    {
      ok: true, id: $id, window_name: $wname, pane: $pane, transcript: $tx,
      legacy_name: $legacy,
      display: ("[story] capture " + $wname + " (" + $pane + ") — recent rendered rows:\n\n" + $tx
                + (if $legacy then "\n\n(found under its pre-SH-166 window name)" else "" end))
    }'
}

# _project_integrity — run the CLI's own `story doctor` tolerantly.
#
# It exits 5 with an AppError::Integrity whenever it finds ANYTHING, and emits
# its `.issues[]` array only when that array is empty. So a non-zero exit here
# is an ordinary finding, not a failure of this probe, and must never abort it.
#
# Sets _INTEGRITY_OK and _INTEGRITY_SUMMARY rather than echoing: a caller
# capturing the summary with $(...) would run this in a subshell and silently
# lose the flag, always reporting a healthy project.
_project_integrity() {
  local out
  _INTEGRITY_OK=true
  out=$(story_cli doctor --json 2>/dev/null) || true
  case $(printf '%s' "$out" | jq -r '.result // "error"' 2>/dev/null || printf 'error') in
    ok) _INTEGRITY_SUMMARY='project integrity: OK — `story doctor` found nothing.' ;;
    *)
      _INTEGRITY_OK=false
      _INTEGRITY_SUMMARY="project integrity: $(printf '%s' "$out" \
        | jq -r '.error // "story doctor reported findings"' 2>/dev/null | tr '\n' ';')"
      ;;
  esac
}

cmd_doctor() {
  [ "$#" -eq 0 ] || fail "usage: story.sh doctor"

  require_story

  if [ -z "$DRY_RUN" ]; then
    [ -n "${TMUX:-}" ] || fail "story doctor requires tmux — run Claude inside a tmux session."
    [ -n "${TMUX_PANE:-}" ] || fail "story doctor requires \$TMUX_PANE — run Claude inside a tmux pane."
  fi

  local doctor_bin="${DOCTOR_LAUNCH_TPL%% *}"
  command -v "$doctor_bin" >/dev/null 2>&1 \
    || fail "launch binary '$doctor_bin' not found on PATH (set STORY_DOCTOR_LAUNCH_CMD)."

  if [ -n "$DRY_RUN" ]; then
    jq -n --arg launch "$DOCTOR_LAUNCH_TPL" --arg wname "$DOCTOR_WINDOW_NAME" '
      {
        ok: true, dry_run: true, window_name: $wname,
        commands: [
          "story doctor --json",
          ("tmux new-window -d -n " + $wname + " -P -F #{pane_id}"),
          ("tmux send-keys -t <pane> -l " + $launch),
          "tmux send-keys -t <pane> Enter",
          "printf %s <multi-line probe> | tmux load-buffer -b story-doctor -",
          "tmux paste-buffer -p -d -b story-doctor -t <pane>",
          "tmux capture-pane -p -t <pane>",
          "tmux kill-window -t <window>"
        ],
        display: ("[story] DRY RUN doctor: would run `story doctor` for project integrity, then spin a throwaway `"
                  + $launch + "` in window " + $wname + ", check readiness, paste a multi-line probe "
                  + "to verify bracketed-paste delivery, and tear it down.")
      }'
    return 0
  fi

  # Called plainly, NOT via $(...): it sets flags the report below reads, and a
  # command substitution would run it in a subshell and lose them.
  _project_integrity
  local integrity_summary="$_INTEGRITY_SUMMARY"

  local pane window
  if ! pane=$(tmux new-window -d -n "$DOCTOR_WINDOW_NAME" -P -F '#{pane_id}' 2>/dev/null) || [ -z "$pane" ]; then
    fail "failed to open a scratch tmux window for the readiness self-test."
  fi
  window=$(tmux display-message -p -t "$pane" '#{window_id}' 2>/dev/null || printf '')

  paste_text "$pane" "$DOCTOR_LAUNCH_TPL" || true
  tmux send-keys -t "$pane" Enter 2>/dev/null || true

  local readiness_confirmed=false tier="none" tail_evidence
  if wait_ready "$pane" "$DOCTOR_LAUNCH_TPL"; then
    readiness_confirmed=true
  fi
  tier="$WAIT_READY_TIER"
  tail_evidence=$(pane_tail "$pane")

  # Live multi-line paste probe: paste a 3-line marker through the real
  # bracketed-paste path WITHOUT submitting, then read the input box back.
  # If delivery works, all three lines land as ONE block and the FIRST sits on
  # the `❯` row; had it split at a newline, the first would have submitted and
  # only the last would remain. Diagnostic only — never presses Enter.
  local probe_ran=false probe_first_held=false probe_seen=0 probe_total=3
  if [ "$readiness_confirmed" = true ]; then
    local probe capture box_row marker
    probe=$(printf 'story-probe-alpha\nstory-probe-bravo\nstory-probe-charlie')
    if paste_prompt "$pane" "$probe" "story-doctor"; then
      probe_ran=true
      capture=$(tmux capture-pane -p -t "$pane" 2>/dev/null || printf '')
      box_row=$(input_box_text "$capture")
      for marker in story-probe-alpha story-probe-bravo story-probe-charlie; do
        case "$capture" in *"$marker"*) probe_seen=$((probe_seen + 1)) ;; esac
      done
      case "$box_row" in *story-probe-alpha*) probe_first_held=true ;; esac
    fi
  fi

  # Tear down the scratch window (best-effort — never flips ok). The probe text
  # is discarded unsubmitted along with it.
  if [ -n "$window" ]; then
    tmux kill-window -t "$window" 2>/dev/null || true
  else
    tmux kill-window -t "$pane" 2>/dev/null || true
  fi

  local display
  if [ "$readiness_confirmed" = true ]; then
    display="[story] doctor: readiness OK via the '$tier' tier — the installed Claude build is recognised."
  else
    display="[story] doctor: readiness NOT confirmed within the poll budget — the readiness marker may have drifted. See pane_tail."
  fi
  if [ "$probe_ran" = true ]; then
    if [ "$probe_first_held" = true ] && [ "$probe_seen" -eq "$probe_total" ]; then
      display="$display Multi-line paste probe: OK — all $probe_total lines held as one un-submitted block."
    else
      display="$display Multi-line paste probe: SUSPECT (first-line-held=$probe_first_held, $probe_seen/$probe_total lines seen) — bracketed paste may not be landing."
    fi
  fi
  display="$display $integrity_summary"

  jq -n \
    --argjson ready "$readiness_confirmed" --arg tier "$tier" \
    --arg tail "$tail_evidence" --arg display "$display" \
    --argjson probe_ran "$probe_ran" --argjson probe_first "$probe_first_held" \
    --argjson probe_seen "$probe_seen" --argjson probe_total "$probe_total" \
    --argjson integrity_ok "$_INTEGRITY_OK" --arg integrity "$integrity_summary" '
    {
      ok: true,
      readiness_confirmed: $ready,
      matched_tier: $tier,
      project_integrity: { ok: $integrity_ok, summary: $integrity }
    }
    + (if $probe_ran then {multiline_probe: {first_line_held: $probe_first, lines_seen: $probe_seen, lines_total: $probe_total}} else {} end)
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
# CLOSED-superstate state the project defines, or $STORY_DONE_STATE.
#
# Not hard-coded to "done": the state set is user-editable (this very repo
# defines five states, not the three `story project new` seeds). Read from
# `story state list` rather than from a file — see story_state_list.
story_closed_state() {
  if [ -n "$DONE_STATE" ]; then printf '%s' "$DONE_STATE"; return 0; fi
  story_state_list | awk -F' *\\(' '
    /\(CLOSED[,)]/ { gsub(/^[[:space:]]+|[[:space:]]+$/, "", $1); print $1; exit }
  '
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
  # Published so the caller can adopt it: `story.sh complete 5` must close and
  # clean up the same worktree `SH-5` names (SH-118).
  CMP_ID=$(canonical_story_id "$show_json" "$id")
  id="$CMP_ID"
  CMP_TITLE=$(printf '%s' "$show_json" | jq -r '.story.story.title // ""')
  CMP_STATE=$(printf '%s' "$show_json" | jq -r '.story.story.state // ""')
  CMP_SUPER=$(printf '%s' "$show_json" | jq -r '.story.story.superstate // ""')
  CMP_DONE_STATE=$(story_closed_state)

  local repo_name wt_container wname legacy_name
  repo_name="$(basename "$CMP_DIR")"
  wname=$(resolve_wname "$id")
  legacy_name=$(legacy_wname "$id" "$repo_name")
  CMP_DEFAULT=$(default_branch)
  freshen_base_ref "$CMP_DEFAULT"

  wt_container="${WORKTREE_IGNORE_PATH%/}"

  # ADOPT THE LEGACY (pre-SH-166) NAME when the canonical bare-id name has
  # NEITHER a worktree dir NOR a local branch, and the legacy name has AT
  # LEAST ONE -- e.g. `story SH-42` dispatched before this fork landed.
  # Otherwise `complete` would report the canonical name "missing"/"missing",
  # close the story, and silently strand the real worktree/branch on disk.
  # Checked BEFORE CMP_WT_PATH/CMP_WT_BRANCH are derived so plan and execute
  # (both callers of this function) stay consistent with each other. A
  # canonical worktree or branch, even a partial one, is never overridden --
  # this is adoption of an orphan, not a preference for the old name.
  CMP_LEGACY=false
  CMP_WNAME="$wname"
  if [ -n "$legacy_name" ] \
     && ! [ -e "$CMP_DIR/$wt_container/$wname" ] \
     && ! local_branch_exists "worktree-$wname" \
     && { [ -e "$CMP_DIR/$wt_container/$legacy_name" ] || local_branch_exists "worktree-$legacy_name"; }; then
    CMP_WNAME="$legacy_name"
    CMP_LEGACY=true
  fi

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
  id="$CMP_ID"

  jq -n \
    --arg id "$id" --arg title "$CMP_TITLE" --arg state "$CMP_STATE" \
    --arg super "$CMP_SUPER" --arg done_state "$CMP_DONE_STATE" \
    --arg default "$CMP_DEFAULT" --arg wtpath "$CMP_WT_PATH" \
    --arg wtstatus "$CMP_WT_STATUS" --arg branch "$CMP_WT_BRANCH" \
    --arg brstatus "$CMP_BR_STATUS" --argjson close "$CMP_NEEDS_CLOSE" \
    --argjson actions "$CMP_ACTIONS" --argjson legacy "$CMP_LEGACY" '
    {
      ok: true, id: $id, title: $title, state: $state, superstate: $super,
      default_branch: $default, legacy_name: $legacy,
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
        + (if $legacy then "\n  (found under its pre-SH-166 name)" else "" end)
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
  id="$CMP_ID"

  local -a removed_wt=() removed_bl=() failed=() skipped=() commands=()
  local closed=false close_note=""

  # Close FIRST, and best-effort: a close failure is reported as a note, never
  # ok:false — the cleanup below is still worth doing and still succeeded.
  if [ -n "$no_close" ]; then
    skipped+=("close:$id(--no-close)")
  elif [ "$CMP_NEEDS_CLOSE" != true ]; then
    if [ -z "$CMP_DONE_STATE" ]; then
      close_note=" Could not close: this project defines no state with a CLOSED superstate."
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
        --argjson legacy "$CMP_LEGACY" \
        --argjson dry "$([ -n "$DRY_RUN" ] && echo true || echo false)" '
    {
      ok: true, id: $id, closed: $closed, legacy_name: $legacy,
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
        + (if $legacy then " (found under its pre-SH-166 name)" else "" end)
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

# `--project <slug>` is story.sh's own global option, stripped here and
# forwarded to every `story` call by story_cli. It is what makes the read verbs
# usable from outside a repository, which is the shape a dashboard-invoked or
# cron-invoked caller has (SH-121, AC-3).
#
# Accepted before the verb only. A trailing one would have to be told apart from
# a story title or a `create --description` value, and this parser has no
# grammar for that; the CLI's own `--project` is still available to anyone who
# needs it mid-command.
while [ "$#" -gt 0 ]; do
  case "$1" in
    --project)
      PROJECT_SLUG="${2:-}"
      [ -n "$PROJECT_SLUG" ] || fail "--project needs a project slug — \`story project list\` shows them."
      shift 2
      ;;
    --project=*)
      PROJECT_SLUG="${1#--project=}"
      [ -n "$PROJECT_SLUG" ] || fail "--project= was given no slug — \`story project list\` shows them."
      shift
      ;;
    *) break ;;
  esac
done

case "${1:-}" in
  dispatch) shift; cmd_dispatch "$@" ;;
  complete) shift; cmd_complete "$@" ;;
  view)     shift; cmd_view "$@" ;;
  list)     shift; cmd_list "$@" ;;
  create)   shift; cmd_create "$@" ;;
  doctor)   shift; cmd_doctor "$@" ;;
  capture)  shift; cmd_capture "$@" ;;
  *)        fail "usage: story.sh <list | view <story-id> | dispatch <story-id> | create --title <t> [--description-file <p>] | complete <plan|execute> <story-id> | doctor | capture <story-id>>" ;;
esac
