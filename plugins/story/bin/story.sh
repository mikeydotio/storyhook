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
#   dispatch <id> [--auto] [--full-auto] [--force] [--resume] [--agent=claude|codex]
#                   [--model=<id>] [--effort=<id>] [--speed=standard|fast]
#                   Refuse unless <id> is READY (issue #40's core ask — see
#                    the READY-GATE step below) and not already claimed, then
#                    claim it via storyhook's CAS primitive, create a NEW git
#                    worktree at .claude/worktrees/<name> on branch
#                    worktree-<name>, open a new tmux window rooted IN that
#                    worktree, gate on claude becoming ready, then type +
#                    submit the handoff prompt. `--force` permits a named story
#                    already claimed: it reuses that existing claim
#                    without writing a redundant state transition. It bypasses
#                    no worktree, branch, tmux, or other safety gate.
#                    A typed epic is the deliberate exception: with `--auto`
#                    it starts a daemon-owned engine run scoped to the epic;
#                    without `--auto` it is refused. Neither epic path claims
#                    the epic or creates a worktree/window for it.
#
#   dispatch --next  SH-344's NEXT MODE — the id-less sibling. Instead of a
#                    caller-named id, claims whatever `story claim --next`
#                    picks — atomically, in one storyhook write transaction —
#                    then follows the identical worktree/window/prompt path
#                    above. Built for a caller dispatching several stories at
#                    once (a fleet, a loop): N concurrent `dispatch --next`
#                    calls are handed N DIFFERENT stories, where N concurrent
#                    `dispatch <id>` calls naming the SAME id would have one
#                    winner and N-1 `claim-conflict` refusals. Mutually
#                    exclusive with <id> — `--next` picks the top-priority
#                    ready story, so it has no way to honor a caller-supplied
#                    one. Since SH-482 both modes claim through the SAME verb,
#                    `story claim`; they differ only in how the story is
#                    chosen.
#
# DELIBERATE DEVIATIONS from the agentics storywork.sh this was forked from:
#
#   1. ALREADY-CLAIMED GUARD (new). Added because storyhook's own
#      `is_ready()` used to return true even for a story already
#      `in-progress` — it only checked open/unblocked, not "hasn't been
#      claimed yet" (confirmed by direct testing against the CLI: moving a
#      story to in-progress did not drop it from `story list --ready`), so a
#      readiness gate alone would not have stopped a redispatch of a story
#      someone was already working. storyhook's SH-236 fixed that root cause
#      — `story list --ready` (deviation #2's ready-gate, below) now excludes
#      an in-progress story on its own — so this guard is no longer the only
#      thing that catches it. Kept anyway: its message names the likely cause
#      ("a previous `/story do` may still be running against it") and gives a
#      concrete unstick command, which the ready-gate's fallback reason
#      ("not in `story list --ready`") does not. Explicit precondition:
#      state == the project's ACTIVE-ROLE state (SH-481 — story_active_state,
#      never the literal `in-progress`) refuses before any side effect, run
#      BEFORE the ready-gate below. SH-440 added an explicit `--force`
#      exception for an operator intentionally resuming a pre-claimed story;
#      that path skips the redundant move but keeps every later dispatch
#      safety check.
#
#   2. READY-STATE GATE (new — the literal ask of issue #40). Queries
#      `story list --ready --json` (the same ground truth every interactive
#      skill already relies on — reusing it here
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
#      the actuator has to tolerate "already claimed, skip the move."
#      This script's only caller is a human via `/story do <id>` (or the
#      /story router), with no pre-claim step — deviation #1 above already
#      guarantees `state != $claim_state` by the time the ordinary claim
#      attempt runs, so that CAS move remains unconditional (still guarded by
#      --if-state, still rolled back on a later hard failure). SH-440's
#      `--force` path is deliberately separate: it accepts only an already
#      claimed named story and records that no transition was made, so a
#      later dispatch failure never rolls back somebody else's existing claim.
#
# Everything else — worktree/tmux mechanics, the two/three-tier base-
# freshness handling, claim-rollback-on-later-failure — is unchanged from the
# agentics original. repo_root() is the exception: it still answers "where does
# worktree bookkeeping happen", but it is told which directory to answer for
# rather than reading the caller's (SH-120).
set -euo pipefail

# The daemon<->script argv contract this file implements (SH-196). The
# dashboard's dispatch endpoint (src/api/dispatch.rs) invokes this script as
# `story.sh --project <slug> dispatch <id> [--auto] [--full-auto] [--force] [--resume]
# [--agent=claude|codex] [--model=<id>] [--effort=<id>]
# [--speed=standard|fast]`; before SH-196, a daemon
# and a script that disagreed about that contract failed by relaying this
# script's own generic top-level usage error as a well-formed business
# refusal -- indistinguishable from "not ready" or "already claimed" to
# whoever read it. resolve_dispatch_script() now reads this line out of
# whichever story.sh it resolved (override, installed plugin, or dev
# checkout) and refuses by name, with a clear "plugin is out of date"
# diagnosis, before running one it does not speak.
#
# Bump this only when the *invocation* contract changes for an existing
# verb -- a new required global flag (like --project itself once did), a
# renamed or removed verb, a change to the ONE-JSON-object-on-stdout
# contract above. Do NOT normally bump it for a new optional flag (--auto), a new
# verb (`reap`), or any behavior a caller driving the old argv shape
# wouldn't notice -- resolve_dispatch_script_from()'s check is
# `declared >= required`, so a script that is merely newer must keep
# resolving, not start refusing. SH-523 is the deliberate exception: the
# daemon now always sends optional `--resume`, so protocol 2 proves an installed
# helper accepts the argv that daemon requires. Protocol 3 advances helper and
# daemon together for the versioned cleanup-lease environment and receipt: an
# older helper must never be mistaken for one that proves exact postconditions.
DISPATCH_PROTOCOL=4

# Shared tmux/worktree/pane-readiness mechanics (window/worktree naming,
# git-safety helpers, the readiness gate, confirmed-send) live in
# ../lib/session.sh — this plugin's own vendored fork, not agentics'.
# shellcheck source=../lib/session.sh
source "$(dirname "${BASH_SOURCE[0]}")/../lib/session.sh"

# storyhook_pointer / read_plugin_config (the `[plugin]` table reader
# hook_is_enabled already relies on) live in ../hooks/lib.sh. `enabled`, the
# hooks kill switch, is the only key anything reads: `tracking` retired with
# the `work` verb (SH-478), and its successor is `story claim --no-comment`,
# a per-invocation choice rather than a standing preference.
# shellcheck source=../hooks/lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/../hooks/lib.sh"

# This script's own absolute path, resolved ONCE here — before dispatch or
# reap ever `cd`s anywhere — because `${BASH_SOURCE[0]}` alone can be
# relative to whatever cwd this process started in, and every later
# directory change (enter_checkout, the dispatch worktree) would make a
# late resolution answer for the wrong place. This is what the autonomous
# charter's `<reap>` placeholder (SH-208) expands to: the exact script an
# unattended session must call back into to reclaim its own worktree.
SELF_PATH="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"
AUTO_APPROVAL_HOOK="$(cd "$(dirname "${BASH_SOURCE[0]}")/../hooks" && pwd)/full-auto.sh"

# ---- config (all env-overridable) -------------------------------------------
STORY="${STORY_BIN:-story}"

# The provider adapter selects this explicitly. `claude` is Storyhook's
# canonical public token; the old `claude-code` STORY_AGENT value remains a
# compatibility alias for callers that used the environment seam before
# SH-436. It warns on stderr so stdout keeps this helper's one-JSON contract.
canonical_agent() {
  case "$1" in
    claude) AGENT="claude" ;;
    codex) AGENT="codex" ;;
    claude-code)
      printf 'warning: STORY_AGENT=claude-code is deprecated; use STORY_AGENT=claude.\n' >&2
      AGENT="claude"
      ;;
    *) fail "unknown agent \`$1\` — supported agents: claude, codex." ;;
  esac
}

# Per-provider model/effort/speed catalogs (SH-517) -- the single source of
# truth `cmd_capabilities` reports and `cmd_dispatch`'s --model/--effort/
# --speed flags validate against. Declared as JSON literals rather than shell
# arrays because that is the shape both consumers actually want: capabilities
# emits it verbatim, and validation reads it back out with `jq -r '.[].id'`.
# Kept beside DEFAULT_*_LAUNCH_TPL in the same per-provider case block below
# so a provider change touches one place, not two.
claude_capability_json() {
  jq -n '{
    models: [
      {id:"opusplan", label:"Opus+Sonnet", default:true},
      {id:"opus",     label:"Opus"},
      {id:"fable",    label:"Fable"},
      {id:"sonnet",   label:"Sonnet"},
      {id:"haiku",    label:"Haiku"}
    ],
    efforts: [{id:"low"},{id:"medium"},{id:"high"},{id:"xhigh"},{id:"max"}],
    # No "standard" entry: speed is a plain toggle with no wire-level
    # "explicit standard" distinct from omitting --speed entirely, so the
    # one selectable alternative to the placeholder "Default" a caller
    # (the web dispatch dialog, most concretely) would render is this.
    speeds: [{id:"fast", label:"Fast"}]
  }'
}

codex_capability_json() {
  # No default model: an unselected dispatch passes no `-m` flag at all and
  # lets the operator's own ~/.codex/config.toml decide, matching today's
  # behavior untouched.
  jq -n '{
    models: [
      {id:"gpt-5.6-sol",   label:"GPT-5.6 Sol"},
      {id:"gpt-5.6-terra", label:"GPT-5.6 Terra"},
      {id:"gpt-5.6-luna",  label:"GPT-5.6 Luna"}
    ],
    efforts: [{id:"none"},{id:"low"},{id:"medium"},{id:"high"},{id:"xhigh"},{id:"max"}],
    # No "standard" entry: speed is a plain toggle with no wire-level
    # "explicit standard" distinct from omitting --speed entirely, so the
    # one selectable alternative to the placeholder "Default" a caller
    # (the web dispatch dialog, most concretely) would render is this.
    speeds: [{id:"fast", label:"Fast"}]
  }'
}

# agent_catalog_ids <models|efforts|speeds> -- newline list of valid ids for
# the CURRENTLY CONFIGURED agent's catalog, read back out of the same JSON
# capabilities reports rather than a second, driftable list.
agent_catalog_ids() {
  "${AGENT}_capability_json" | jq -r --arg field "$1" '.[$field][].id'
}

# validate_agent_model/effort <value> -- a no-op for an empty (unselected)
# value; otherwise refuses by name, listing the current agent's own valid
# set, BEFORE cmd_dispatch's caller reaches any claim or worktree side
# effect -- the same fail-fast contract --agent already has.
validate_agent_model() {
  local v="$1"
  [ -n "$v" ] || return 0
  agent_catalog_ids models | grep -qxF "$v" \
    || fail "unknown model \`$v\` for agent \`$AGENT\` — valid models: $(agent_catalog_ids models | paste -sd, -)."
}

validate_agent_effort() {
  local v="$1"
  [ -n "$v" ] || return 0
  agent_catalog_ids efforts | grep -qxF "$v" \
    || fail "unknown effort \`$v\` for agent \`$AGENT\` — valid efforts: $(agent_catalog_ids efforts | paste -sd, -)."
}

# validate_agent_speed <value> -- speed is NOT provider-scoped (both
# providers offer the identical standard/fast pair), so this is a plain
# membership check rather than a catalog lookup. --speed's own flag parsing
# already rejects anything else before this runs; this second check is what
# also catches an invalid $STORY_SPEED, which bypasses that parse.
validate_agent_speed() {
  local v="$1"
  case "$v" in
    standard | fast) ;;
    *) fail "unknown speed \`$v\` — use --speed=standard or --speed=fast." ;;
  esac
}

# compose_launch_tpl <base|auto> <model> <effort> <speed> -- builds AGENT's
# launch command line for MODE from a model/effort/speed selection, any of
# which may be "" to mean "provider default". "full-auto" is not a MODE
# here: it reuses "auto"'s composition exactly, the same sharing
# DEFAULT_AUTO_LAUNCH_TPL and FULL_AUTO_LAUNCH_TPL already had before
# SH-517. Called with all three empty, this reproduces the literal strings
# DEFAULT_LAUNCH_TPL/DEFAULT_AUTO_LAUNCH_TPL held before SH-517, byte for
# byte -- an unselected dispatch's argv must not change.
compose_launch_tpl() {
  case "$AGENT" in
    claude) compose_claude_launch_tpl "$@" ;;
    codex) compose_codex_launch_tpl "$@" ;;
  esac
}

# Claude's `--settings` flag is not repeatable. A fast selection MERGES
# `fastMode:true` into whatever settings object the mode already carries
# (auto's `{"permissions":{"defaultMode":"acceptEdits"}}`) rather than
# appending a second --settings flag, which the CLI would not accept.
compose_claude_launch_tpl() {
  local mode="$1" model="${2:-opusplan}" effort="$3" speed="$4"
  local cmd="claude --permission-mode plan --model $model"
  [ -z "$effort" ] || cmd="$cmd --effort $effort"
  local settings=""
  [ "$mode" = "base" ] || settings='{"permissions":{"defaultMode":"acceptEdits"}}'
  if [ "$speed" = "fast" ]; then
    if [ -n "$settings" ]; then
      settings=$(printf '%s' "$settings" | jq -c '. + {fastMode:true}')
    else
      settings='{"fastMode":true}'
    fi
  fi
  # $settings is always jq's own output over a fixed set of static keys, so
  # it never contains a literal single quote -- the same invariant the
  # pre-SH-517 hardcoded DEFAULT_AUTO_LAUNCH_TPL string relied on.
  [ -z "$settings" ] || cmd="$cmd --settings '$settings'"
  printf '%s' "$cmd"
}

compose_codex_launch_tpl() {
  local mode="$1" model="$2" effort="$3" speed="$4"
  local cmd="codex --no-alt-screen -c check_for_update_on_startup=false"
  [ "$mode" = "base" ] || cmd="$cmd --approve-for-me --dangerously-bypass-hook-trust"
  [ -z "$model" ] || cmd="$cmd -m $model"
  [ -z "$effort" ] || cmd="$cmd -c model_reasoning_effort=\"$effort\""
  [ "$speed" != "fast" ] || cmd="$cmd -c service_tier=\"priority\""
  printf '%s' "$cmd"
}

configure_agent() {
  canonical_agent "$1"
  case "$AGENT" in
  claude)
    AGENT_LABEL="Claude Code"
    DEFAULT_LAUNCH_TPL=$(compose_claude_launch_tpl base "" "" "")
    DEFAULT_AUTO_LAUNCH_TPL=$(compose_claude_launch_tpl auto "" "" "")
    DEFAULT_WORKTREE_IGNORE_PATH=".claude/worktrees/"
    DEFAULT_READY_PROCESS_PATTERN='^(claude|node)$'
    DEFAULT_READY_PROMPT_GLYPH='❯'
    DEFAULT_SUBMIT_KEY='Enter'
    DEFAULT_EMPTY_INPUT_PATTERN=''
    ;;
  codex)
    AGENT_LABEL="Codex"
    # Storyhook owns this child session's startup lifecycle. A newly available
    # Codex version otherwise inserts an interactive update chooser before the
    # prompt, so the readiness gate correctly refuses to type and rolls the
    # dispatch back. Suppress that chooser for this one managed process; the
    # user's durable Codex update preference remains untouched.
    DEFAULT_LAUNCH_TPL=$(compose_codex_launch_tpl base "" "" "")
    DEFAULT_AUTO_LAUNCH_TPL=$(compose_codex_launch_tpl auto "" "" "")
    DEFAULT_WORKTREE_IGNORE_PATH=".codex/worktrees/"
    DEFAULT_READY_PROCESS_PATTERN='^(codex)$'
    DEFAULT_READY_PROMPT_GLYPH='›'
    DEFAULT_SUBMIT_KEY='Tab'
    DEFAULT_EMPTY_INPUT_PATTERN='^[[:space:]]*Ask Codex to do anything[[:space:]]*$'
    ;;
  esac

  # These values are derived from the provider but remain individually
  # overridable. Recompute them when dispatch's --agent flag overrides the
  # environment-selected provider; doing only AGENT would launch one provider
  # with the other's worktree and keyboard contract.
  LAUNCH_TPL="${STORY_LAUNCH_CMD:-$DEFAULT_LAUNCH_TPL}"
  # AUTONOMOUS BLAST RADIUS (SH-511). These provider defaults keep Plan mode
  # while allowing the approved plan to proceed without a person: Claude's
  # PermissionRequest-scoped helper selects Auto and returns to acceptEdits;
  # Codex's exact-pane watcher accepts its plan and workspace-write automatic
  # review handles later tool approvals while trusting the packaged hook.
  # The child still lives in a disposable worktree and its charter still bans
  # version/release/deploy work. STORY_LAUNCH_CMD remains a wholesale expert
  # override, so an autonomous dispatch reports when it cannot guarantee this
  # posture rather than silently claiming that it can.
  if [ -n "${STORY_LAUNCH_CMD:-}" ]; then
    AUTO_LAUNCH_TPL="$STORY_LAUNCH_CMD"
    AUTO_LAUNCH_SOURCE="STORY_LAUNCH_CMD"
    AUTO_LAUNCH_OVERRIDDEN=true
  else
    AUTO_LAUNCH_TPL="$DEFAULT_AUTO_LAUNCH_TPL"
    AUTO_LAUNCH_SOURCE="builtin"
    AUTO_LAUNCH_OVERRIDDEN=false
  fi
  # ENGINE FULL AUTO BLAST RADIUS (SH-461). An engine lane gives the selected
  # agent edit rights inside one disposable worktree. The autonomous charter's
  # version/release/deploy prohibitions remain intact, and merge is allowed only
  # through its certified path. Reuse the provider posture proven by SH-511,
  # but isolate it from the daemon's general expert override: an inherited
  # STORY_LAUNCH_CMD may weaken ordinary dispatches, never an engine lane.
  if [ -n "${STORY_FULL_AUTO_LAUNCH_CMD:-}" ]; then
    FULL_AUTO_LAUNCH_TPL="$STORY_FULL_AUTO_LAUNCH_CMD"
    FULL_AUTO_LAUNCH_SOURCE="STORY_FULL_AUTO_LAUNCH_CMD"
    FULL_AUTO_LAUNCH_OVERRIDDEN=true
  else
    FULL_AUTO_LAUNCH_TPL="$DEFAULT_AUTO_LAUNCH_TPL"
    FULL_AUTO_LAUNCH_SOURCE="builtin"
    FULL_AUTO_LAUNCH_OVERRIDDEN=false
  fi
  if [ -n "${STORY_LAUNCH_CMD:-}" ]; then
    FULL_AUTO_IGNORED_GENERAL_OVERRIDE="STORY_LAUNCH_CMD"
  else
    FULL_AUTO_IGNORED_GENERAL_OVERRIDE=""
  fi
  WORKTREE_IGNORE_PATH="${STORY_WORKTREE_IGNORE_PATH:-$DEFAULT_WORKTREE_IGNORE_PATH}"
  READY_PROMPT_GLYPH="${STORY_READY_PROMPT_GLYPH:-$DEFAULT_READY_PROMPT_GLYPH}"
  READY_PROCESS_PATTERN="${STORY_READY_PROCESS_PATTERN:-$DEFAULT_READY_PROCESS_PATTERN}"
  READY_LAUNCH_BIN="${LAUNCH_TPL%% *}"
  SUBMIT_KEY="${STORY_SUBMIT_KEY:-$DEFAULT_SUBMIT_KEY}"
  EMPTY_INPUT_PATTERN="${STORY_EMPTY_INPUT_PATTERN:-$DEFAULT_EMPTY_INPUT_PATTERN}"
  DOCTOR_LAUNCH_TPL="${STORY_DOCTOR_LAUNCH_CMD:-$DEFAULT_LAUNCH_TPL}"
}

# Start with canonical Claude defaults so function definitions and shared
# configuration below have concrete values. The selected environment value is
# resolved at command dispatch time: an explicit `dispatch --agent=...` must
# be able to override even a stale or invalid STORY_AGENT value.
configure_agent "claude"
# Launch command, run INSIDE the worktree dispatch already created — must NOT
# include `-w`/`--worktree` (would try to create a second worktree at the
# same path). <name> renders to the resolved window/worktree name, <n> to the
# story id, for a custom override that wants either.
LAUNCH_TPL="${STORY_LAUNCH_CMD:-$DEFAULT_LAUNCH_TPL}"
# The handoff prompt — the only lever the dispatcher has over the child
# session. Delivery is via a bracketed paste (paste_prompt), so a multi-line
# STORY_PROMPT override is safe. Storyhook stories aren't GitHub issues: no
# label to apply, no `Closes #N` convention — the child instead posts its plan
# back via `story comment` and references the story id in every PR body.
#
# CHARTER-INERT (SH-226). Both shipped templates below quote the commands they
# name with ‘ ’ (U+2018/U+2019), never backticks, and carry none of
# ` $ ; & | < > ! ( ) [ ] { } * ? ~ # \ ' or a newline. Those characters are
# non-special in every POSIX-family shell, so the rendered prompt delivered
# verbatim to one is a single command word list whose worst outcome is one
# `command not found` — it cannot execute anything it names. That matters
# because this text reaches places with no gate in front of them: a doc quoting
# it, a transcript, an agent reading this file. tests/test-charter-inert.sh
# enforces it against the REAL render path, and also asserts the instructions
# survive, so the invariant can't be satisfied by deleting them.
#
# (This comment used to claim "single-line + ASCII (no backticks) by default".
# It had been true of neither half since the --auto charter landed. The ‘ ’
# quotes deliberately break the ASCII half; UTF-8 already rides this pipeline,
# since READY_PROMPT_GLYPH is U+276F and appears in every capture.)
PROMPT_TPL="${STORY_PROMPT:-Investigate and plan a fix for story <n> in this repo. Begin by reading it with ‘story show <n> --json’ -- its comments carry the discussion history. When your plan is finalized and approved, post it as a comment on <n> via ‘story comment <n> your-plan’ before you start implementing. Implement the approved work and run only its new and directly impacted tests. Commit and push the work, open one pull request whose body references story <n>, link it with ‘story link-pr <n> PR-URL’, and comment the PR link on <n>. Then move the story with ‘story move <n> verifying’ as your absolute last action and stop: the centralized verifier owns the full suite, merge, completion, and worktree cleanup. If verification returns the story to you, repair the existing PR without rewriting published history, run the new and impacted tests, push, move <n> back to verifying, and stop again. Do not run make test, land-pr.sh, story move <n> done, reap, semver bump, deployit deploy, or any release/version step from this worktree, and do not plan for them.}"
# Claude's ExitPlanMode tool gives the PreToolUse hook an approval boundary at
# which it can remind the model to persist the plan. Codex changes modes in the
# TUI and exposes no corresponding tool call, so its built-in charter has to
# carry that boundary across the turn itself: make persistence step one of the
# approved plan, then the approved plan is the instruction Codex resumes from.
# The same provider clause owns PR-title traceability: every Codex-authored PR
# carries the resolved story id where an operator can see it in a PR list.
# Kept provider-specific so Claude's established prompt stays byte-identical,
# and applied only to built-ins so STORY_PROMPT and the two autonomous overrides
# remain wholesale overrides rather than unexpectedly acquiring policy text.
CODEX_BUILTIN_CLAUSE="Codex Plan mode cannot write that comment before approval. In the plan you present, make ‘story comment <n> your-exact-approved-plan’ the first implementation step. After approval, execute that step before changing files or running tests, and post the plan verbatim rather than summarizing it. Ensure every linked pull request title contains the exact story ID <n>."
RESUME_PROMPT_CLAUSE="You are resuming work already started and left behind by a previous agent. Before changing anything, inspect the worktree, git status, git log, git diff, story comments, and relevant tests to determine exactly where it stopped. The previous agent may have encountered an error or stopped uncleanly. Preserve valid existing work, then continue under every remaining instruction in this charter."
# The autonomous charter `--auto` swaps in for PROMPT_TPL. SH-511 removed its
# last human interaction: plan approval is scoped by provider events (with one
# exact-gated tmux Return for Claude), and question refusal is provider-native;
# targeted testing and repair remain the child's own call. The daemon-owned
# verifier takes over after the child publishes exactly one linked PR and moves
# the story to `verifying`; it owns the full suite, merge, closure, and reap.
#
# It used to open by anchoring every `story` write at the main checkout,
# because a worktree carried its own copy of the tracker and a write made
# against it was silently lost. That clause is gone with the copy: one store
# means the worktree and the main checkout are the same project, and telling an
# autonomous agent to `cd` elsewhere for every write was friction bought with a
# defect that no longer exists.
#
# SH-219: whether the dispatched session can actually reach ‘/council-vote’
# isn't knowable from prose alone, so the charter is assembled from a shared
# HEAD and TAIL plus one of two swappable decision clauses — COUNCIL when
# council_vote_available (below cmd_dispatch's own helpers) finds the skill on
# disk, SOLO when it does not. Composed here, once, so the two rendered
# charters can never drift on the obligations they share. STORY_AUTO_PROMPT
# and STORY_AUTO_PROMPT_SOLO still let a caller override either wholesale, same
# as STORY_PROMPT always has for the attended template.
AUTO_PROMPT_HEAD="Investigate and plan a fix for story <n> in this repo. Begin by reading it with ‘story show <n> --json’ -- its comments carry the discussion history. This is an AUTONOMOUS session: the user approves your plan once and is then unavailable -- ask no further questions after that approval and never block waiting on input. When your plan is finalized and approved, post it as a comment on <n> before you start implementing. For every later decision, first judge whether it has one clear best answer: when it does, research current best practice for it, decide it yourself, and note the decision and your reasoning as a comment on <n>."
# SH-371: both decision clauses say WHEN, not just what — record the outcome
# the moment it is reached, before resuming the work. A council writes its trail
# into a directory relative to wherever the agent was standing, which for a
# dispatched session is this worktree, so a verdict deferred to the end-of-story
# comment dies with the teardown that reclaims it; a solo decision has no trail
# on disk at all and dies with the session that made it. The user determination
# on SH-371 accepts that loss rather than gating teardown on it — a teardown
# beating the council's own close is urgent by construction, the story being
# abandoned, re-filed or fast-tracked — and that only holds while the charter
# names the earlier moment. Shared in kind between COUNCIL and SOLO, for the
# same reason AUTO_SCOPE_CLAUSE below is shared outright.
AUTO_COUNCIL_CLAUSE="Only when a decision has two or more genuinely defensible answers -- a question of scope, direction, or technical approach that research alone cannot settle -- convene ‘/council-vote’ instead of asking, and record its outcome as a comment on <n> the moment the council concludes, before you resume the work: the council writes its own trail into a directory this worktree owns, so a verdict left there and nowhere else does not survive this worktree being reclaimed. If ‘/council-vote’ turns out not to be available to you, or the council aborts without a decision, do not stall: research the alternatives, choose the one you can best defend, and comment on <n> naming the decision, what you weighed, and why you chose as you did."
AUTO_SOLO_CLAUSE="When a decision has two or more genuinely defensible answers -- a question of scope, direction, or technical approach that research alone cannot settle -- do not stall waiting for a person to answer: choose the one you can best defend, and comment on <n> naming the decision, what you weighed, and why you chose as you did. Comment it the moment you decide, before you resume the work: a decision held only in this session is gone with the session."
# SH-402: filing a story for something you already understand, while
# already holding the context, throws that context away and pays somebody
# else to rebuild it -- and this project's own backlog grew faster than it
# closed partly because nothing ever said so. The context condition is
# deliberately a fraction of the window, never a raw token count -- a bare
# number states an opinion about how big this operator's window is, which
# is not a fact about every install, and it reads as unconditionally true
# on a smaller one. This project's own numeric calibration against its own
# window lives in CLAUDE.md, not here. Shared between both autonomous
# charters, same as the decision clause above it: filing is not a decision
# with a single right answer independent of whether council-vote is
# reachable, so it does not vary between COUNCIL and SOLO.
AUTO_SCOPE_CLAUSE="When you uncover a second problem while working <n>, prefer adopting it into <n> over filing a new story: fix it in its own commit with its own regression test, and comment on <n> what you adopted and why -- that is two hats intact, since two hats governs commits, not stories. Adopt and fix it now only while at least half your context window is still unused and the extra work is small enough to finish and test in this session. Otherwise widen <n> to cover it without doing the work now -- comment what you found and leave <n> open rather than closing it, so the next session picks up where you stopped. If you cannot tell how much context remains, treat it as spent. File a new story only for work that is genuinely separate, genuinely too large for one session, or blocked on something you cannot reach."
AUTO_PROMPT_TAIL="Implement the approved work and run only its new and directly impacted tests. Commit and push the work, open exactly one pull request whose body references story <n>, link it with ‘story link-pr <n> PR-URL’, and comment the PR link on <n>. Then move the story with ‘story move <n> verifying’ as your absolute last action and stop: the centralized verifier owns the full suite, merge, completion, and worktree cleanup. If verification returns the story to you, repair the existing PR without rewriting published history, run the new and impacted tests, push, move <n> back to verifying, and stop again. Do not run make test, land-pr.sh, story move <n> done, reap, semver bump, deployit deploy, or any release/version step from this worktree, and do not plan for them. If you hit a hard stop you cannot resolve before submission, post a comment on <n> with full diagnostics, run ‘story block <n> the-reason’, leave the worktree intact, and stop."
AUTO_PROMPT_TPL="${STORY_AUTO_PROMPT:-$AUTO_PROMPT_HEAD $AUTO_COUNCIL_CLAUSE $AUTO_SCOPE_CLAUSE $AUTO_PROMPT_TAIL}"
AUTO_PROMPT_SOLO_TPL="${STORY_AUTO_PROMPT_SOLO:-$AUTO_PROMPT_HEAD $AUTO_SOLO_CLAUSE $AUTO_SCOPE_CLAUSE $AUTO_PROMPT_TAIL}"
# Which charter --auto gets: 'auto' (default) probes council_vote_available
# for a real answer, 'on'/'off' force it without touching disk — the escape
# hatch AND the test seam, same shape as READY_PROCESS_PATTERN's own hatch
# below.
COUNCIL_MODE="${STORY_COUNCIL:-auto}"
# Extra clause a caller appends to the handoff prompt (daemon-caller seam).
# Appended VERBATIM with a single space separator, AFTER <n>/<name> templating.
PROMPT_EXTRA="${STORY_PROMPT_EXTRA:-}"
# New-window (and worktree) name override; supports the <n> placeholder.
WINDOW_NAME_TPL="${STORY_WINDOW_NAME:-}"
# Focus policy: the new window is created DETACHED (-d) by default.
FOREGROUND="${STORY_FOREGROUND:-}"
# Target tmux session for the dispatch window (non-interactive-caller seam).
TARGET_SESSION="${STORY_TARGET_SESSION:-}"
# Create TARGET_SESSION if it doesn't already exist (SH-50 — the dashboard
# daemon has no tmux session of its own to arrange ahead of time). Off by
# default: an interactive caller, or a non-interactive one with a
# pre-arranged session, must see no behavior change.
CREATE_SESSION="${STORY_CREATE_SESSION:-}"
# Per-story git-worktree hygiene AND the worktree container itself.
WORKTREE_IGNORE_PATH="${STORY_WORKTREE_IGNORE_PATH:-$DEFAULT_WORKTREE_IGNORE_PATH}"
WORKTREE_IGNORE_COMMENT="# story per-story git worktrees (ephemeral — never commit)"
# Private dispatch-to-verification cleanup protocol. Keep these literals in
# lockstep with src/domain.rs; protocol/version tests pin the agreement.
CLEANUP_LEASE_VERSION=1
CLEANUP_LEASE_MARKER="storyhook-cleanup-lease-v1.json"
# Readiness gate before typing the prompt — see lib/session.sh's wait_ready
# for the full two-tier rationale (marker footer match, or structural
# frame+glyph+stabilise fallback), and wait_ready_sentinel for the
# sentinel-file gate the Claude provider actually uses.
READY_PATTERN="${STORY_READY_PATTERN:-for shortcuts|for agents|mode on|to cycle}"
# READY_ATTEMPTS * READY_DELAY is the whole poll budget both gates share (SH-544):
# 180 * 0.25s = 45s, derived from src/daemon/lifecycle.rs's own
# SPAWN_LOCK_DEADLINE (30s — this project's own documented tolerance for
# ordinary daemon contention) plus a 15s margin for tmux round trips and the
# time Claude itself takes to reach its first render, rather than a bare
# literal (this project's own CLAUDE.md doctrine: a wall-clock ceiling must
# derive from the deadline it disproves). Was 15s (60*0.25) — undocumented,
# and well under SPAWN_LOCK_DEADLINE even though wait_ready_sentinel polls for
# a file a daemon request has to complete to produce; a request that gets
# queued behind ordinary contention on this project's own bounded worker pool
# (`DISPATCHERS`, src/daemon/serve.rs) could legitimately outlast 15s without
# anything about the launched agent being wrong. Widening this only ever costs
# latency on a poll that would otherwise have timed out — both gates return
# the instant their own conditions hold, every earlier poll included.
# tests/dispatch_ready_budget.rs pins `>= SPAWN_LOCK_DEADLINE` so the two
# numbers cannot silently drift apart again (the SH-136 shape).
READY_ATTEMPTS="${STORY_READY_ATTEMPTS:-180}"
READY_DELAY="${STORY_READY_DELAY:-0.25}"
READY_FALLBACK_DELAY="${STORY_READY_FALLBACK_DELAY:-3}"
READY_STABLE_POLLS="${STORY_READY_STABLE_POLLS:-3}"
READY_FRAME_GLYPH="${STORY_READY_FRAME_GLYPH:-─}"
READY_PROMPT_GLYPH="${STORY_READY_PROMPT_GLYPH:-$DEFAULT_READY_PROMPT_GLYPH}"
# The pane's foreground command must be the launch binary before ANY text is
# delivered to it (SH-226). This pattern is the NAME half of that test: `node`
# is here because Claude Code installs as a Node wrapper on some paths and tmux
# reports the foreground process; excluding it would refuse real sessions. It
# still excludes every shell, which is the discrimination the failure needs.
# Heuristic, and deliberately overridable: setting this to `.` matches anything
# and restores the pre-SH-226 behaviour with no code change — the escape hatch
# for an environment where Claude reports an unexpected name. `story doctor`
# prints the name it actually observed, so an operator can see what to put here.
#
# Since SH-239 it is no longer the ONLY way to match — pane_runs also accepts
# the launch binary by identity, which is what a version-named install needs and
# what no fixed pattern can keep up with. The hatch stays for genuinely
# unrecognised names (a wrapper script, a renamed build).
READY_PROCESS_PATTERN="${STORY_READY_PROCESS_PATTERN:-$DEFAULT_READY_PROCESS_PATTERN}"
# The launch command's FIRST WORD, whose resolved binary pane_runs recognises by
# identity (SH-239). Derived from LAUNCH_TPL rather than configured separately:
# the process worth recognising is, by definition, the one we launched.
# cmd_doctor reassigns it around its own probe, which uses a different template.
READY_LAUNCH_BIN="${LAUNCH_TPL%% *}"
READY_TAIL_LINES="${STORY_READY_TAIL_LINES:-8}"
CONFIRM_ATTEMPTS="${STORY_CONFIRM_ATTEMPTS:-8}"
CONFIRM_DELAY="${STORY_CONFIRM_DELAY:-0.3}"
SEND_RETRIES="${STORY_SEND_RETRIES:-2}"
SUBMIT_KEY="${STORY_SUBMIT_KEY:-$DEFAULT_SUBMIT_KEY}"
EMPTY_INPUT_PATTERN="${STORY_EMPTY_INPUT_PATTERN:-$DEFAULT_EMPTY_INPUT_PATTERN}"
PASTE_SETTLE_DELAY="${STORY_PASTE_SETTLE_DELAY:-0.2}"
READY_ACCEPT_PATTERN="${STORY_READY_ACCEPT_PATTERN:-esc to interrupt|Working|Thinking|Crunching|tokens|to interrupt}"
CAPTURE_LINES="${STORY_CAPTURE_LINES:-200}"
DRY_RUN="${STORY_DRY_RUN:-}"
# State `complete` closes a story into. Empty means "ask the CLI for the
# project's state catalog and take the first CLOSED-superstate entry" — see
# story_closed_state.
DONE_STATE="${STORY_DONE_STATE:-}"
# `doctor`'s throwaway readiness probe: what it launches, and the scratch
# window it launches into. Kept separate from LAUNCH_TPL so probing a build
# never depends on a dispatch-time override.
DOCTOR_LAUNCH_TPL="${STORY_DOCTOR_LAUNCH_CMD:-$DEFAULT_LAUNCH_TPL}"
DOCTOR_WINDOW_NAME="${STORY_DOCTOR_WINDOW_NAME:-story-doctor}"
CODEX_PLAN_KEY="${STORY_CODEX_PLAN_KEY:-BTab}"
CODEX_PLAN_KEY_RETRIES="${STORY_CODEX_PLAN_KEY_RETRIES:-2}"
# Set by _project_integrity; read by cmd_doctor.
_INTEGRITY_OK=true
_INTEGRITY_SUMMARY=""
# Escalate a non-fresh base (CACHED/HEAD-FALLBACK — see Step 6 below) from a
# warning to a hard ok:false. Unrelated to story readiness (see deviation #3
# above) — this is purely about the worktree's base commit.
REQUIRE_FRESH_BASE="${STORY_REQUIRE_FRESH_BASE:-}"

# Project this invocation is about, forwarded verbatim to every `story` call.
#
# Seeded by `--project <slug>` on story.sh's own command line, and PINNED by
# resolve_checkout to whatever the CLI resolved — see that function for why the
# pin has to happen before anything changes directory. Empty means "let the CLI
# decide", which is the ordinary case for the read verbs.
PROJECT_SLUG=""

# The project's linked checkout, and the repository root reached from it. Set by
# resolve_checkout / enter_checkout, empty for every verb that needs no
# directory. Declared here so `set -u` is satisfied on paths that fail before
# they are assigned.
CHECKOUT_PATH=""
CHECKOUT_ERROR=""
PROJECT_ROOT=""

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
# It no longer cds either. The three verbs that need a directory take it from
# the project's linked checkout and enter it once, for real (enter_checkout), so
# there is no second working directory for this function to switch between —
# and $PROJECT_SLUG is pinned at that moment, which is what keeps every call
# after the cd talking about the project the caller named rather than the one
# the new directory would answer for.
# story_cli [--actor <label>] <args...> — the CLI, scoped to $PROJECT_SLUG.
#
# **Declares who is writing** (SH-246). Storyhook records the command it
# dispatched against every event, but this script's claim and its
# claim-rollback are both `story move` — indistinguishable in a trail without
# something more. That ambiguity is exactly what made SH-239's reversion take a
# store dump and three source files to explain, so every write here says which
# code path it came from.
#
# The label is per-call rather than exported once: a script-wide value would say
# only "story.sh", which is the part a reader can already infer. `--actor` must
# lead, and is consumed here rather than passed on.
story_cli() {
  local cli_actor="${STORY_ACTOR_DEFAULT:-story.sh}"
  if [ "$1" = "--actor" ]; then
    cli_actor="story.sh:$2"
    shift 2
  fi
  if [ -n "$PROJECT_SLUG" ]; then
    set -- --project "$PROJECT_SLUG" "$@"
  fi
  STORYHOOK_ACTOR="$cli_actor" "$STORY" "$@"
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

# repo_root <dir> — echo the MAIN worktree's absolute root directory of the
# repository <dir> belongs to, regardless of which worktree (or subdirectory of
# one) <dir> itself is. `git rev-parse --git-common-dir` resolves to <main>/.git
# from ANY worktree of the repo (relative, ".git", only when <dir> already IS
# the main worktree's own root), so its dirname — resolved to an absolute path
# via a real `cd` in a subshell, never the caller's shell — is stable no matter
# where <dir> sits.
#
# **This answers "where does worktree bookkeeping happen?", never "which
# project is this?"** Its three callers all create, name or remove a git
# worktree, and the shape they need is a repository. Project selection belongs
# to the CLI (see story_cli), which knows things a shell walk cannot — a
# registered origin among them.
#
# **<dir> is REQUIRED, and deliberately has no default.** `set -u` turns a
# caller that forgets it into a loud failure, because the default it would
# otherwise take — the working directory — is precisely the inference SH-120
# exists to remove.
#
# It MUST be used instead of `git rev-parse --show-toplevel`: --show-toplevel is
# relative to where it runs, so from inside one of THIS script's own worktrees
# it would return that worktree's root, not the main repo's — a `/story do`
# invoked from inside an existing dispatched worktree would then nest its new
# worktree inside it, matching nothing in `git worktree list`.
repo_root() {
  local dir="$1" common
  common=$(git -C "$dir" rev-parse --git-common-dir 2>/dev/null) || return 1
  # Two cds, not one: --git-common-dir answers relative to <dir> whenever it can
  # (it is the bare string ".git" in a main worktree's own root), so the dirname
  # has to be resolved from there rather than from wherever this script stands.
  ( CDPATH= cd -- "$dir" && CDPATH= cd -- "$(dirname -- "$common")" && pwd -P )
}

# cd_resolve <base> <path> — <path> as an absolute, symlink-resolved directory,
# interpreting a relative <path> against <base> rather than against wherever
# this script happens to stand. Non-zero for an empty or unreachable <path>.
cd_resolve() {
  [ -n "$2" ] || return 1
  ( CDPATH= cd -- "$1" && CDPATH= cd -- "$2" && pwd -P )
}

# is_linked_worktree <dir> — true when <dir> belongs to a LINKED git worktree
# rather than to the repository's main one.
#
# `--git-dir` and `--git-common-dir` name the same directory everywhere in the
# main worktree, including its subdirectories, and differ in a linked one, where
# the former is <main>/.git/worktrees/<name>. Both are resolved through a real
# cd so a relative answer and an absolute one compare equal.
is_linked_worktree() {
  local dir="$1" gitdir common
  gitdir=$(cd_resolve "$dir" "$(git -C "$dir" rev-parse --git-dir 2>/dev/null)") || return 1
  common=$(cd_resolve "$dir" "$(git -C "$dir" rev-parse --git-common-dir 2>/dev/null)") || return 1
  [ "$gitdir" != "$common" ]
}

# ---- the project's checkout --------------------------------------------------
#
# `story project show` answers two questions in one round trip — which project
# this invocation resolved to, and where that project's repo-side work runs —
# and the three verbs that touch a repository need both. Asking twice is not an
# option: the second question would be answered from a DIFFERENT working
# directory, which in a monorepo whose sub-project owns its own identity is
# entitled to name a different project (SH-151).
#
# resolve_project — ask, then PIN $PROJECT_SLUG and $CHECKOUT_PATH to the
# answer. A project may legitimately have no checkout; callers that only need
# its daemon/API identity (epic Auto dispatch, SH-469) still have a complete
# answer in that case.
#
# **The pin happens even when there is no checkout**, because the slug is
# answered either way and a caller that tolerates a missing directory (capture)
# still needs to be talking about the right project.
#
# Sets globals rather than echoing, for the reason on _load_ready_stories: a
# caller using $(...) would run `fail` in a subshell, so the script would carry
# on with the refusal captured in a variable instead of printed.
resolve_project() {
  local show_json result
  CHECKOUT_PATH=""
  CHECKOUT_ERROR=""
  show_json=$(story_cli project show --json 2>/dev/null) || true
  result=$(printf '%s' "$show_json" | jq -r '.result // ""' 2>/dev/null || printf '')
  if [ "$result" != "ok" ]; then
    # Relayed VERBATIM, never a list composed here. Project selection has FOUR
    # steps — `--project`, $STORYHOOK_PROJECT, the nearest committed pointer
    # file, then the repository's registered origin — and a shell can perform
    # neither of the last two. A hand-written list would be wrong today and
    # wrong again the next time the resolver grows a step.
    CHECKOUT_ERROR=$(printf '%s' "$show_json" \
      | jq -r '.error // "`story project show` emitted no result"' 2>/dev/null \
      || printf '`story project show` emitted no result')
    return 1
  fi
  PROJECT_SLUG=$(printf '%s' "$show_json" | jq -r '.project.slug // ""')
  CHECKOUT_PATH=$(printf '%s' "$show_json" | jq -r '.project.checkout // ""')
  return 0
}

# require_checkout — validate the checkout half of resolve_project's answer.
# Split from the lookup so an API-only operation can preserve EngineService's
# own no-checkout refusal instead of inventing a second shell-side policy.
require_checkout() {
  if [ -z "$CHECKOUT_PATH" ]; then
    CHECKOUT_ERROR="project \`$PROJECT_SLUG\` has no checkout on this machine, so there is nowhere to run a git worktree — record one with \`story --project $PROJECT_SLUG project link checkout <path>\`. Its stories stay readable meanwhile; only the repo-side verbs need a directory."
    return 1
  fi
  return 0
}

# resolve_checkout — the historical combined contract for repo-side verbs.
# It still asks exactly once, pins the project, and then requires its checkout.
resolve_checkout() {
  resolve_project || return 1
  require_checkout
}

# enter_checkout — validate the resolved checkout and MOVE INTO the repository
# it belongs to, publishing that repository's root as $PROJECT_ROOT.
#
# **One real cd, not `git -C` threaded through every call site.** There are
# roughly twenty bare git invocations across this file and lib/session.sh,
# several of them inside shared helpers whose signatures would all have to grow
# a parameter — and no invariant would stop a twenty-first from being added
# bare. story.sh is always executed and never sourced, so the cd cannot leak
# into the caller's shell; repo_root() already cds for that reason.
#
# It enters the repository ROOT rather than the recorded path, because worktree
# bookkeeping belongs at the top of a repository (see repo_root). A checkout
# recorded as a subdirectory therefore still works. A checkout recorded as a
# LINKED WORKTREE does not: resolving that one up would silently operate on a
# different tree from the one recorded, and this codebase reports a
# disagreement between recorded state and reality rather than correcting it.
enter_checkout() {
  local root
  [ -d "$CHECKOUT_PATH" ] \
    || fail "project \`$PROJECT_SLUG\` records its checkout at \`$CHECKOUT_PATH\`, and there is no such directory — re-record it with \`story --project $PROJECT_SLUG project link checkout <path>\` if it moved, or run \`story doctor --fix\`, which forgets the recorded path and leaves the project and its stories exactly as they are."
  root=$(repo_root "$CHECKOUT_PATH") \
    || fail "project \`$PROJECT_SLUG\`'s checkout \`$CHECKOUT_PATH\` is not a git repository, and this verb has to create a git worktree in it — clone it there, or re-record the checkout with \`story --project $PROJECT_SLUG project link checkout <path>\`."
  if is_linked_worktree "$CHECKOUT_PATH"; then
    fail "project \`$PROJECT_SLUG\`'s checkout \`$CHECKOUT_PATH\` is a linked git worktree of \`$root\`, not a checkout — worktrees created for it would land in \`$root\`, which is not the directory recorded. Record the main checkout instead: \`story --project $PROJECT_SLUG project link checkout $root\`."
  fi
  PROJECT_ROOT="$root"
  CDPATH= cd -- "$root" || fail "cannot enter project \`$PROJECT_SLUG\`'s checkout \`$root\`."
}

# claim_rollback_note <id> <pre-claim-state> <claim-transitioned> <claimed-state> —
# cmd_dispatch's claim is a HARD
# PRECONDITION performed BEFORE any worktree/window side effect. If it
# succeeds and a LATER step still hard-fails (worktree/branch collision, `git
# worktree add`, `tmux new-window`), the story would otherwise be left
# permanently stuck in the claimed state with no worktree, no window, and no
# session — silent. This releases the claim through `story unclaim` (SH-484)
# and echoes a clause for the failure message naming the outcome either way. A
# forced redispatch of an already-claimed story passes false because this
# dispatch did not create the claim and therefore must not roll it back.
#
# ROUTED THROUGH THE VERB, not a hand-rolled `story move` (SH-484). `unclaim`
# is a compare-and-swap on the project's active-role state, so the guard SH-481
# had to fix here is now the CLI's own, and the destination is derived from the
# story's own event log rather than carried around by this script — which means
# the note reports where the story ACTUALLY landed, fallback included, instead
# of where the caller assumed it would. `--no-comment` because dispatch's own
# claim is silent in BOTH modes (`story claim --next --no-comment` and, since
# SH-482, `story claim <id> --no-comment`): a dispatch that rolled itself back
# cleanly did, in the end, nothing, and a comment saying otherwise would be the
# only trace of a non-event.
#
# THE DIVERGENCE THIS DOC USED TO WARN ABOUT IS CLOSED (SH-482). Both halves of
# the pair now go through a CLI verb, so both resolve the active-role state the
# same way, in the same process, from the same table: an ID-MODE claim can no
# longer be released by a CAS the CLI resolves differently from the slug this
# script picked, because this script no longer picks one.
#
# <pre-claim-state> survives as a PARAMETER even so, because the FAILURE
# message still needs it — the warning names a copy-pasteable manual move for
# an operator whose `unclaim` itself refused. It now carries the CLI's own
# `claimed_from` rather than this script's earlier `show` read, so the two
# sides of that sentence cannot disagree either.
# <claimed-state> likewise stays REQUIRED, not defaulted: under `set -u` a
# forgotten argument aborts loudly, where a default of `in-progress` would
# quietly reinstate the wrong-guard stranding SH-481 was filed for.
# dispatch_ready_note — one clause naming WHY the readiness check gave up, from
# the globals it sets (wait_ready_sentinel's reasons since SH-231; wait_ready's
# own "timeout" default survives for cmd_doctor's still-unported call). A
# timeout and "a shell is sitting in that pane" are different situations with
# different remedies, and the operator needs to be told which.
PLAN_MODE_REASON="not-required"
ensure_provider_plan_mode() {
  local pane="$1" attempt=0 key_attempt=0 content
  PLAN_MODE_REASON="not-required"
  [ "$AGENT" = "codex" ] || return 0

  PLAN_MODE_REASON="not-confirmed"
  if content=$(tmux capture-pane -p -t "$pane" 2>/dev/null) \
     && printf '%s' "$content" | grep -qF -- 'Plan mode (shift+tab to cycle)'; then
    PLAN_MODE_REASON="already-plan"
    return 0
  fi

  # Codex paints its input box before the selected model has finished loading.
  # Shift+Tab sent in that interval is accepted by tmux but discarded by the
  # TUI, leaving dispatch to wait forever for a mode change that never began.
  # Wait for that explicit transient footer to clear before sending the one
  # mode-cycle key. Do not retry the key blindly: once Plan mode is selected, a
  # second Shift+Tab would cycle straight back out of it.
  while [ "$attempt" -lt "$READY_ATTEMPTS" ]; do
    content=$(tmux capture-pane -p -t "$pane" 2>/dev/null) || content=""
    if printf '%s' "$content" | grep -qF -- 'Plan mode (shift+tab to cycle)'; then
      PLAN_MODE_REASON="already-plan"
      return 0
    fi
    if ! printf '%s' "$content" | grep -Eq -- 'model:[[:space:]]+loading([[:space:]]|$)'; then
      break
    fi
    sleep "$READY_DELAY"
    attempt=$((attempt + 1))
  done
  if [ "$attempt" -ge "$READY_ATTEMPTS" ]; then
    PLAN_MODE_REASON="model-loading"
    return 1
  fi

  # A late Codex startup transition can also discard the first Shift+Tab even
  # after the model label has stopped saying "loading". Retry only when a full
  # confirmation window never showed the literal Plan footer. That makes a
  # dropped key recoverable without ever cycling an already-confirmed Plan
  # session back into another mode.
  while [ "$key_attempt" -lt "$CODEX_PLAN_KEY_RETRIES" ]; do
    tmux send-keys -t "$pane" "$CODEX_PLAN_KEY" 2>/dev/null \
      || { PLAN_MODE_REASON="mode-key-failed"; return 1; }
    attempt=0
    while [ "$attempt" -lt "$READY_ATTEMPTS" ]; do
      if content=$(tmux capture-pane -p -t "$pane" 2>/dev/null) \
         && printf '%s' "$content" | grep -qF -- 'Plan mode (shift+tab to cycle)'; then
        PLAN_MODE_REASON="selected-plan"
        return 0
      fi
      sleep "$READY_DELAY"
      attempt=$((attempt + 1))
    done
    key_attempt=$((key_attempt + 1))
  done
  return 1
}

# Codex has no hook event at its plan-review dialog (measured on CLI 0.149.0).
# Arm the exact-pane watcher after Plan mode is confirmed and before the prompt
# is submitted. tmux owns the continuation, so it shares the pane's lifetime
# instead of the short-lived dispatch process's. All shell-bound values are
# quoted before tmux hands the command to its shell.
schedule_codex_plan_approval() {
  local pane="$1" auto_marker="$2" full_auto_marker="$3"
  local hook_q pane_q auto_q full_auto_q
  [ "$AGENT" = codex ] || return 0
  [[ "$pane" =~ ^%[0-9]+$ ]] || return 1
  printf -v hook_q '%q' "$AUTO_APPROVAL_HOOK"
  printf -v pane_q '%q' "$pane"
  printf -v auto_q '%q' "$auto_marker"
  printf -v full_auto_q '%q' "$full_auto_marker"
  tmux run-shell -b -t "$pane" \
    "env STORYHOOK_AUTO=$auto_q STORYHOOK_FULL_AUTO=$full_auto_q bash $hook_q --approve-codex-plan $pane_q" \
    >/dev/null 2>&1
}

dispatch_ready_note() {
  case "$WAIT_READY_REASON" in
    wrong-process)
      printf 'that pane is running `%s`, not a process matching `%s` — the launch never started. Set STORY_READY_PROCESS_PATTERN if your %s reports a different name; `.` matches anything' \
        "${WAIT_READY_COMMAND:-?}" "$READY_PROCESS_PATTERN" "$AGENT_LABEL"
      ;;
    pid-mismatch)
      printf 'the pane no longer shows the process this dispatch launched — something else now occupies it (a respawn, or a second dispatch racing the same window)'
      ;;
    pid-exited)
      printf 'the launched process exited before its SessionStart hook could publish a dispatch sentinel — check `pane_tail` below for why it quit'
      ;;
    no-sentinel)
      printf 'timed out waiting for its SessionStart hook to publish a dispatch sentinel. Possible causes: the plugin'\''s hooks are not installed in that worktree; %s has not started yet; the sentinel write failed silently on the daemon side (check daemon.log for a "could not publish its dispatch sentinel" warning, SH-544); or the daemon was too slow or busy to answer the hook'\''s own request within its budget (run `story doctor install` to check daemon health)' "$AGENT_LABEL"
      ;;
    *)
      if [ -n "$WAIT_READY_COMMAND" ]; then
        printf 'timed out waiting for it to render; the pane is running `%s`' "$WAIT_READY_COMMAND"
      else
        printf 'timed out waiting for it to render, and the pane occupant could not be observed'
      fi
      ;;
  esac
}

claim_rollback_note() {
  local id="$1" pre_state="$2" transitioned="$3" claimed_state="$4"
  if [ "$transitioned" != true ]; then
    printf ' The pre-existing `%s` state was left unchanged.' "$claimed_state"
    return 0
  fi
  local rb_json rb_result rb_to
  rb_json=$(story_cli --actor dispatch-rollback unclaim "$id" --no-comment --json 2>/dev/null) || true
  rb_result=$(printf '%s' "$rb_json" | jq -r '.result // ""' 2>/dev/null || printf '')
  if [ "$rb_result" = "ok" ]; then
    rb_to=$(printf '%s' "$rb_json" | jq -r '.story.story.state // "?"' 2>/dev/null || printf '?')
    printf ' Rolled the claim back to `%s`.' "$rb_to"
  else
    printf ' WARNING: story %s is now stranded at `%s` with no worktree/window — run `story unclaim %s`, or `story move %s %s --if-state %s` if that refuses.' \
      "$id" "$claimed_state" "$id" "$id" "$pre_state" "$claimed_state"
  fi
}

# registered_worktree_branch <absolute-path> — print the local branch checked
# out at exactly PATH and return 0. A detached worktree prints DETACHED. Return
# 1 when PATH is not registered. Git's porcelain format is stable specifically
# for machine parsing; path and branch are read as whole lines so spaces remain
# intact.
registered_worktree_branch() {
  local target="$1" line current="" branch="" found=false
  while IFS= read -r line; do
    case "$line" in
      worktree\ *)
        if [ "$found" = true ]; then
          printf '%s' "${branch:-DETACHED}"
          return 0
        fi
        current="${line#worktree }"
        branch=""
        [ "$current" != "$target" ] || found=true
        ;;
      branch\ *)
        [ "$found" != true ] || branch="${line#branch refs/heads/}"
        ;;
      '')
        if [ "$found" = true ]; then
          printf '%s' "${branch:-DETACHED}"
          return 0
        fi
        current=""
        branch=""
        ;;
    esac
  done < <(git worktree list --porcelain 2>/dev/null)
  if [ "$found" = true ]; then
    printf '%s' "${branch:-DETACHED}"
    return 0
  fi
  return 1
}

# cleanup_dispatch_git <path> <branch> <worktree-created> <branch-created> —
# remove only resources this invocation created. Resume may attach an existing
# branch or reuse a dirty worktree; those are evidence and are never rollback
# targets, even when a later provider handoff fails.
cleanup_dispatch_git() {
  local path="$1" branch="$2" worktree_created="$3" branch_created="$4"
  # A failed handoff must not leave a marker that a later manual transition
  # could mistake for a successful dispatch. This is safe for a resumed
  # worktree too: the current attempt replaced the marker with its own exact
  # tmux generation before any later gate could fail.
  local private_git_dir
  private_git_dir=$(git -C "$path" rev-parse --absolute-git-dir 2>/dev/null || printf '')
  local marker="" failure=""
  DISPATCH_CLEANUP_NOTE="resources created by this attempt were rolled back and pre-existing work was preserved"
  [ -z "$private_git_dir" ] || marker="$private_git_dir/$CLEANUP_LEASE_MARKER"
  if [ -n "$marker" ]; then
    rm -f "$marker" 2>/dev/null || failure="could not remove cleanup marker $marker"
    [ ! -e "$marker" ] && [ ! -L "$marker" ] \
      || failure="${failure:+$failure; }cleanup marker remains at $marker"
  fi
  if [ "$worktree_created" = true ]; then
    git worktree remove --force "$path" >/dev/null 2>&1 \
      || failure="${failure:+$failure; }git worktree remove refused for $path"
    git worktree prune >/dev/null 2>&1 || true
    if registered_worktree_branch "$path" >/dev/null 2>&1 || [ -e "$path" ]; then
      failure="${failure:+$failure; }worktree remains at $path"
    fi
  fi
  if [ "$branch_created" = true ]; then
    git branch -D "$branch" >/dev/null 2>&1 \
      || failure="${failure:+$failure; }could not delete branch $branch"
    local_branch_exists "$branch" \
      && failure="${failure:+$failure; }branch remains at $branch" \
      || true
  fi
  if [ -n "$failure" ]; then
    DISPATCH_CLEANUP_NOTE="WARNING: dispatch cleanup is incomplete: $failure; inspect the preserved resources before retrying"
    return 1
  fi
  return 0
}

dispatch_cleanup_note() {
  printf '%s' "${DISPATCH_CLEANUP_NOTE:-resources created by this attempt were rolled back and pre-existing work was preserved}"
}

# write_cleanup_lease_marker <project> <story> <repository> <worktree>
# <branch> <pane> — atomically publish the exact Git resources this dispatch
# owns and the tmux server on which it placed the story window.
write_cleanup_lease_marker() {
  local project="$1" story="$2" repository="$3" worktree="$4" branch="$5" pane="$6"
  local private_git_dir repository_real worktree_real
  local socket_path
  local marker temp

  private_git_dir=$(git -C "$worktree" rev-parse --absolute-git-dir 2>/dev/null) || return 1
  repository_real=$(cd_resolve / "$repository") || return 1
  worktree_real=$(cd_resolve / "$worktree") || return 1
  socket_path=$(tmux display-message -p -t "$pane" '#{socket_path}' 2>/dev/null) || return 1

  [ -n "$socket_path" ] && [ "${socket_path#/}" != "$socket_path" ] || return 1

  marker="$private_git_dir/$CLEANUP_LEASE_MARKER"
  temp=$(mktemp "$private_git_dir/.storyhook-cleanup-lease.XXXXXX") || return 1
  local lease_json
  if ! lease_json=$(jq -n \
      --argjson version "$CLEANUP_LEASE_VERSION" \
      --arg project "$project" --arg story "$story" \
      --arg repository "$repository_real" --arg worktree "$worktree_real" \
      --arg branch "$branch" --arg socket "$socket_path" \
      '{version:$version, project_slug:$project, story_id:$story,
        repository_path:$repository, worktree_path:$worktree, branch:$branch,
        tmux:{socket_path:$socket}}'); then
    rm -f "$temp"
    return 1
  fi
  if ! printf '%s\n' "$lease_json" >"$temp"; then
    rm -f "$temp"
    return 1
  fi
  chmod 600 "$temp" 2>/dev/null || { rm -f "$temp"; return 1; }
  mv -f "$temp" "$marker" || { rm -f "$temp"; return 1; }
  DISPATCH_CLEANUP_LEASE="$lease_json"
}

# missing_claim_state_vocabulary <claim-state> — confirm, never infer, that
# this project's state vocabulary lacks the state cmd_dispatch just tried to
# claim into. Echoes the observed comma-separated vocabulary and
# returns 0 only for a positively-parsed absence. A failed call, empty message,
# unparseable message, or any output that DOES define <claim-state> degrades to
# the ordinary move failure instead of asserting a diagnosis without evidence.
#
# Kept on the already-failed claim path so the successful path pays nothing.
# The CLI's error names the cause; this wrapper adds a machine-readable reason
# and Storyhook's own repair command without removing the upstream error.
#
# SH-481 made the target a parameter rather than the literal `in-progress`.
# Two consequences worth stating. It is now correctly INERT whenever the
# project assigns the `active` role, because story_active_state read that slug
# out of this same vocabulary and it is therefore present by construction; the
# only way to reach a positive answer is the fallback target `in-progress`,
# which is exactly the SH-440 case this was written for. And the membership
# test is an exact match on each line's first field, not a substring of the
# whole message: a substring test on a caller-supplied slug would report a
# state as present because some UNRELATED slug contains it.
missing_claim_state_vocabulary() {
  local claim_state="$1" list_json message states
  list_json=$(story_cli state list --json 2>/dev/null) || true
  message=$(printf '%s' "$list_json" | jq -r '.message // ""' 2>/dev/null) || true
  [ -n "$message" ] || return 1
  printf '%s\n' "$message" | awk -v want="$claim_state" 'NF && $1 == want { found = 1 } END { exit !found }' \
    && return 1
  states=$(printf '%s\n' "$message" | awk 'NF { printf "%s%s", (n++ ? ", " : ""), $1 }')
  [ -n "$states" ] || return 1
  printf '%s' "$states"
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

# _council_plugin_enabled <plugin-key> <project-root> — "false" if <plugin-key>
# is explicitly disabled for enabledPlugins in the MOST SPECIFIC settings file
# that mentions it — the project's own .claude/settings.local.json, then its
# .claude/settings.json, then the user's own ~/.claude/settings.json — "true"
# otherwise, including when nothing mentions it at all: that is Claude Code's
# own default for an installed plugin, and this probe must agree with it.
_council_plugin_enabled() {
  local key="$1" project_root="$2" cfg="${CLAUDE_CONFIG_DIR:-$HOME/.claude}"
  local candidates=()
  if [ -n "$project_root" ]; then
    candidates+=("$project_root/.claude/settings.local.json" "$project_root/.claude/settings.json")
  fi
  candidates+=("$cfg/settings.json")
  local f val
  for f in "${candidates[@]}"; do
    [ -f "$f" ] || continue
    # NOT `.enabledPlugins[$k] // empty` -- jq's `//` treats a `false` value
    # as absent, same as null, which is exactly the one value this lookup
    # exists to detect. `has` distinguishes "explicitly false" from "key
    # missing" instead.
    val=$(jq -r --arg k "$key" \
      '(.enabledPlugins // {}) | if has($k) then (.[$k] | tostring) else "" end' \
      "$f" 2>/dev/null) || continue
    case "$val" in
      false) printf 'false'; return 0 ;;
      true) printf 'true'; return 0 ;;
    esac
  done
  printf 'true'
}

# council_vote_available <project-root> — true (exit 0) if a ‘/council-vote’
# skill will resolve in a session dispatched under this identity: a bare skill
# file directly under ~/.claude/skills or <project-root>/.claude/skills, OR a
# plugin from installed_plugins.json whose own tree ships
# skills/council-vote/SKILL.md and that isn't explicitly disabled. Probes for
# the SKILL FILE, not for a plugin literally named "council" — SH-219 asks
# "is the council-vote plugin installed", but what actually matters is whether
# the skill answers, the same "ask what it IS, not how it is spelled" rule
# SH-239 wrote into this project's CLAUDE.md over a process-name lookalike.
#
# COUNCIL_MODE overrides this entirely: 'on'/'off' force the answer without
# touching disk (the escape hatch and this probe's own test seam); 'auto' (the
# default) runs the probe. ANY probe failure — no config dir, an unreadable or
# malformed registry — answers "not available", silently: a broken registry
# file must never fail a dispatch.
council_vote_available() {
  local project_root="${1:-}"
  case "$COUNCIL_MODE" in
    on) return 0 ;;
    off) return 1 ;;
  esac

  # Codex does not expose a stable machine-readable inventory of every skill
  # visible to a child conversation. Guessing cache paths would make dispatch
  # depend on private install layout, so automatic discovery degrades to the
  # safe solo charter. STORY_COUNCIL=on remains the explicit capability seam.
  [ "$AGENT" = "codex" ] && return 1

  local cfg="${CLAUDE_CONFIG_DIR:-$HOME/.claude}"

  [ -f "$cfg/skills/council-vote/SKILL.md" ] && return 0
  [ -n "$project_root" ] && [ -f "$project_root/.claude/skills/council-vote/SKILL.md" ] && return 0

  local registry="$cfg/plugins/installed_plugins.json"
  [ -f "$registry" ] || return 1

  local keys
  keys=$(jq -r '(.plugins // {}) | keys[]?' "$registry" 2>/dev/null) || return 1
  [ -n "$keys" ] || return 1

  local key install_path enabled
  while IFS= read -r key; do
    [ -n "$key" ] || continue
    install_path=$(jq -r --arg k "$key" '.plugins[$k][0].installPath // empty' "$registry" 2>/dev/null) || continue
    [ -n "$install_path" ] || continue
    [ -f "$install_path/skills/council-vote/SKILL.md" ] || continue
    enabled=$(_council_plugin_enabled "$key" "$project_root")
    [ "$enabled" = "false" ] && continue
    return 0
  done < <(printf '%s\n' "$keys")

  return 1
}

# cmd_dispatch_epic <id> <auto> <full-auto> <force> — the named-epic branch.
#
# An epic is a scope, not a lane. `--auto` therefore starts the daemon-owned
# engine through SH-468's authenticated HTTP control surface and returns before
# any claim, git, tmux or provider handoff. `--full-auto` remains the private
# identity modifier the engine adds when it later dispatches one actionable
# descendant; accepting it here would collapse those two boundaries again.
cmd_dispatch_epic() {
  local id="$1" auto="$2" full_auto="$3" force="$4"

  [ -n "$auto" ] || refuse "epic-requires-auto" \
    "story $id is an epic and carries no actionable steps of its own — use \`/story do $id --auto\` to start the Full Auto engine on its descendants."
  [ -z "$force" ] || refuse "epic-force-unsupported" \
    "story $id is an epic, so \`--force\` cannot reuse a story claim; omit it to start the Full Auto engine."
  [ -z "$full_auto" ] || refuse "epic-full-auto-internal" \
    "\`--full-auto\` identifies a descendant lane launched by the engine and cannot start epic $id; use only \`--auto\`."

  local display="[story] Would start the Full Auto engine for epic $id with 1 $AGENT lane."
  if [ -n "$DRY_RUN" ]; then
    jq -n --arg epic "$id" --arg agent "$AGENT" --arg display "$display" \
      '{ok:true, dry_run:true, kind:"engine-run", epic:$epic, agent:$agent, lanes:1, display:$display}'
    return 0
  fi

  command -v curl >/dev/null 2>&1 \
    || fail "starting the Full Auto engine requires \`curl\`, but it is not available on PATH."

  local daemon_status daemon_url daemon_port token payload response http_status body detail run_id
  daemon_status=$(story_cli daemon status 2>/dev/null) \
    || fail "could not ask the storyhook daemon where it is listening."
  daemon_url=$(printf '%s\n' "$daemon_status" | awk 'NR == 1 && $1 == "storyhook" && $2 == "daemon" && $4 == "running" && $5 == "at" { print $6 }')
  daemon_url="${daemon_url%/}"
  daemon_port="${daemon_url##*:}"
  case "$daemon_port" in
    ''|*[!0-9]*) fail "storyhook daemon status did not report a usable HTTP port: ${daemon_status%%$'\n'*}" ;;
  esac

  token=$(story_cli daemon token 2>/dev/null) \
    || fail "could not read the storyhook daemon token needed to start the Full Auto engine."
  [ -n "$token" ] \
    || fail "the storyhook daemon returned an empty token; refusing to send an unauthenticated engine request."
  payload=$(jq -cn --arg epic "$id" --arg agent "$AGENT" \
    '{epic:$epic, lanes:1, agent:$agent}')

  if ! response=$(curl --silent --show-error --max-time 30 \
      --request POST \
      --header 'Content-Type: application/json' \
      --header 'X-Storyhook: 1' \
      --header "X-Storyhook-Token: $token" \
      --data "$payload" \
      --write-out $'\n%{http_code}' \
      "http://127.0.0.1:$daemon_port/api/repos/$PROJECT_SLUG/engine" 2>&1); then
    fail "could not reach the Full Auto engine endpoint: $response"
  fi
  http_status="${response##*$'\n'}"
  body="${response%$'\n'*}"

  if [ "$http_status" != "201" ]; then
    detail=$(printf '%s' "$body" \
      | jq -r '.error // .display // .message // empty' 2>/dev/null \
      || printf '')
    [ -n "$detail" ] || detail="HTTP $http_status"
    refuse "engine-start-refused" "Full Auto engine refused epic $id: $detail"
  fi
  printf '%s' "$body" \
    | jq -e '.result == "ok" and (.run | type == "object") and (.run.id | type == "string")' \
      >/dev/null 2>&1 \
    || fail "Full Auto engine returned HTTP 201 without a valid run result."

  run_id=$(printf '%s' "$body" | jq -r '.run.id')
  display="[story] Started Full Auto engine run $run_id for epic $id with 1 $AGENT lane."
  jq -n \
    --arg epic "$id" --arg agent "$AGENT" --arg display "$display" \
    --argjson run "$(printf '%s' "$body" | jq '.run')" \
    '{ok:true, kind:"engine-run", epic:$epic, agent:$agent, run:$run, display:$display}'
}

# ---- subcommand: capabilities -----------------------------------------------
# The provider-knowledge catalog dispatch's own --model/--effort/--speed
# flags validate against (SH-517): one JSON object naming AGENT's valid
# models (each with a display label; at most one marked `default:true`),
# efforts, and speeds. Read by the daemon's `GET /api/dispatch-options`
# (src/api/dispatch.rs) so the web dispatch dialog can build its selects
# from the SAME data this file's own configure_agent block already owns,
# rather than a second, driftable copy in Rust or the dashboard.
cmd_capabilities() {
  local requested_agent=""
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --agent=*)
        [ -z "$requested_agent" ] || fail "--agent may be specified only once — usage: story.sh capabilities [--agent=claude|codex]"
        requested_agent="${1#--agent=}"
        shift ;;
      *) fail "usage: story.sh capabilities [--agent=claude|codex]" ;;
    esac
  done
  configure_agent "${requested_agent:-${STORY_AGENT:-claude}}"
  jq -n --arg agent "$AGENT" --argjson caps "$("${AGENT}_capability_json")" \
    '{ok:true, agent:$agent} + $caps'
}

# ---- subcommand: dispatch ---------------------------------------------------
cmd_dispatch() {
  # <story-id> XOR --next may appear before or after --auto/--full-auto/--force/--agent; anything past
  # that (a second positional, an unknown flag) is a hard fail rather than
  # the silent ignore this verb used to give a stray trailing token —
  # matching view/list/capture/doctor/complete-plan, which already reject
  # extras. --next (SH-344) is the id-less mode: it claims whatever
  # `story claim --next` picks, atomically, rather than a caller-named id —
  # see the NEXT MODE section below for why this is a second mode and not a
  # rewrite of the id-directed claim, which since SH-482 goes through the same
  # verb as `story claim <id>`.
  local usage='story.sh dispatch (<story-id> | --next) [--auto] [--full-auto] [--force] [--resume] [--agent=claude|codex] [--model=<id>] [--effort=<id>] [--speed=standard|fast]'
  local id="" auto="" full_auto="" want_next="" force="" resume="" requested_agent=""
  local requested_model="" requested_effort="" requested_speed=""
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --auto)
        [ -z "$auto" ] || fail "--auto may be specified only once — usage: story.sh dispatch (<story-id> | --next) [--auto] [--full-auto] [--force] [--agent=claude|codex] [--model=<id>] [--effort=<id>] [--speed=standard|fast]"
        auto=1; shift ;;
      --full-auto)
        [ -z "$full_auto" ] || fail "--full-auto may be specified only once — usage: story.sh dispatch <story-id> --auto [--full-auto] [--force] [--agent=claude|codex] [--model=<id>] [--effort=<id>] [--speed=standard|fast]"
        full_auto=1; shift ;;
      --force)
        [ -z "$force" ] || fail "--force may be specified only once — usage: $usage"
        force=1; shift ;;
      --resume)
        [ -z "$resume" ] || fail "--resume may be specified only once — usage: $usage"
        resume=1; shift ;;
      --agent=*)
        [ -z "$requested_agent" ] || fail "--agent may be specified only once — usage: story.sh dispatch (<story-id> | --next) [--auto] [--full-auto] [--force] [--agent=claude|codex] [--model=<id>] [--effort=<id>] [--speed=standard|fast]"
        requested_agent="${1#--agent=}"
        case "$requested_agent" in
          claude|codex) ;;
          *) fail "unknown agent \`$requested_agent\` — use --agent=claude or --agent=codex." ;;
        esac
        shift ;;
      --model=*)
        [ -z "$requested_model" ] || fail "--model may be specified only once — usage: story.sh dispatch (<story-id> | --next) [--auto] [--full-auto] [--force] [--agent=claude|codex] [--model=<id>] [--effort=<id>] [--speed=standard|fast]"
        requested_model="${1#--model=}"
        [ -n "$requested_model" ] || fail "--model was given no value — usage: story.sh dispatch (<story-id> | --next) [--auto] [--full-auto] [--force] [--agent=claude|codex] [--model=<id>] [--effort=<id>] [--speed=standard|fast]"
        shift ;;
      --effort=*)
        [ -z "$requested_effort" ] || fail "--effort may be specified only once — usage: story.sh dispatch (<story-id> | --next) [--auto] [--full-auto] [--force] [--agent=claude|codex] [--model=<id>] [--effort=<id>] [--speed=standard|fast]"
        requested_effort="${1#--effort=}"
        [ -n "$requested_effort" ] || fail "--effort was given no value — usage: story.sh dispatch (<story-id> | --next) [--auto] [--full-auto] [--force] [--agent=claude|codex] [--model=<id>] [--effort=<id>] [--speed=standard|fast]"
        shift ;;
      --speed=*)
        [ -z "$requested_speed" ] || fail "--speed may be specified only once — usage: story.sh dispatch (<story-id> | --next) [--auto] [--full-auto] [--force] [--agent=claude|codex] [--model=<id>] [--effort=<id>] [--speed=standard|fast]"
        requested_speed="${1#--speed=}"
        case "$requested_speed" in
          standard|fast) ;;
          *) fail "unknown speed \`$requested_speed\` — use --speed=standard or --speed=fast." ;;
        esac
        shift ;;
      --next)
        [ -z "$want_next" ] || fail "--next may be specified only once — usage: story.sh dispatch (<story-id> | --next) [--auto] [--full-auto] [--force] [--agent=claude|codex] [--model=<id>] [--effort=<id>] [--speed=standard|fast]"
        [ -z "$id" ] || fail "--next cannot be combined with a story id \`$id\` — usage: story.sh dispatch (<story-id> | --next) [--auto] [--full-auto] [--force] [--agent=claude|codex] [--model=<id>] [--effort=<id>] [--speed=standard|fast]"
        want_next=1; shift ;;
      *)
        [ -z "$id" ] || fail "unexpected argument \`$1\` — usage: $usage"
        [ -z "$want_next" ] || fail "--next cannot be combined with a story id \`$1\` — usage: $usage"
        id="$1"; shift ;;
    esac
  done
  [ -n "$id" ] || [ -n "$want_next" ] || fail "usage: $usage"
  [ -z "$want_next" ] || [ -z "$force" ] \
    || fail "--force requires a named story id and cannot be combined with --next — usage: story.sh dispatch <story-id> [--auto] [--full-auto] [--force] [--agent=claude|codex] [--model=<id>] [--effort=<id>] [--speed=standard|fast]"
  [ -z "$want_next" ] || [ -z "$resume" ] \
    || fail "--resume requires a named story id and cannot be combined with --next — usage: $usage"
  [ -z "$resume" ] || [ -z "$force" ] \
    || fail "--resume and --force cannot be combined: --resume reconstructs surviving resources, while --force only reuses an active claim — usage: $usage"
  [ -z "$full_auto" ] || [ -n "$auto" ] \
    || fail "--full-auto requires --auto — usage: story.sh dispatch <story-id> --auto --full-auto [--force] [--agent=claude|codex] [--model=<id>] [--effort=<id>] [--speed=standard|fast]"
  [ -z "$full_auto" ] || [ -n "$id" ] \
    || fail "--full-auto requires a named story id and cannot be combined with --next — usage: story.sh dispatch <story-id> --auto --full-auto [--force] [--agent=claude|codex] [--model=<id>] [--effort=<id>] [--speed=standard|fast]"
  [ -z "$id" ] || valid_story_id "$id" || fail "story id must be alphanumeric (hyphens/underscores allowed) (got: $id)."

  # The explicit dispatch option outranks STORY_AGENT. Both are resolved
  # before the tmux/story/checkout gates, so an invalid provider can never
  # claim a story or create a worktree.
  if [ -n "$requested_agent" ]; then
    configure_agent "$requested_agent"
  else
    configure_agent "${STORY_AGENT:-claude}"
  fi

  # model/effort/speed selectors (SH-517): explicit flag > STORY_MODEL/
  # STORY_EFFORT/STORY_SPEED > provider default -- the same precedence
  # --agent already has beneath STORY_AGENT, above. Resolved and validated
  # here, before any claim/worktree side effect, same as --agent.
  local resolved_model="${requested_model:-${STORY_MODEL:-}}"
  local resolved_effort="${requested_effort:-${STORY_EFFORT:-}}"
  local resolved_speed="${requested_speed:-${STORY_SPEED:-standard}}"
  if [ -n "$resolved_model" ] || [ -n "$resolved_effort" ] || [ "$resolved_speed" != standard ]; then
    validate_agent_model "$resolved_model"
    validate_agent_effort "$resolved_effort"
    validate_agent_speed "$resolved_speed"
    # $STORY_LAUNCH_CMD/$STORY_FULL_AUTO_LAUNCH_CMD are wholesale operator
    # overrides (configure_agent, above) with no seam to splice a selector
    # into without guessing at their shape. Refuse by name rather than
    # silently ignore the selector or mangle the operator's own command
    # line -- the same posture SH-511's header comment already commits to
    # for a launch override that weakens unattendedness.
    [ "$AUTO_LAUNCH_OVERRIDDEN" != true ] \
      || fail "--model/--effort/--speed cannot be combined with \$STORY_LAUNCH_CMD -- it is a wholesale launch override with no seam for a selector. Unset \$STORY_LAUNCH_CMD, or drop the selector."
    [ -z "$full_auto" ] || [ "$FULL_AUTO_LAUNCH_OVERRIDDEN" != true ] \
      || fail "--model/--effort/--speed cannot be combined with \$STORY_FULL_AUTO_LAUNCH_CMD -- it is a wholesale launch override with no seam for a selector. Unset \$STORY_FULL_AUTO_LAUNCH_CMD, or drop the selector."
    LAUNCH_TPL=$(compose_launch_tpl base "$resolved_model" "$resolved_effort" "$resolved_speed")
    AUTO_LAUNCH_TPL=$(compose_launch_tpl auto "$resolved_model" "$resolved_effort" "$resolved_speed")
    FULL_AUTO_LAUNCH_TPL="$AUTO_LAUNCH_TPL"
  fi
  # Reported in the result JSON below. Claude always launches SOME model
  # (opusplan is baked into its templates even unselected); Codex has no
  # such default -- an unselected Codex model stays "" and is omitted from
  # the JSON entirely, the same presence-signals-selection contract effort
  # already has.
  local effective_model="$resolved_model"
  [ -n "$effective_model" ] || { [ "$AGENT" != claude ] || effective_model="opusplan"; }

  local launch_source="$AUTO_LAUNCH_SOURCE" launch_overridden="$AUTO_LAUNCH_OVERRIDDEN"
  local ignored_general_override=""
  if [ -n "$full_auto" ]; then
    LAUNCH_TPL="$FULL_AUTO_LAUNCH_TPL"
    launch_source="$FULL_AUTO_LAUNCH_SOURCE"
    launch_overridden="$FULL_AUTO_LAUNCH_OVERRIDDEN"
    ignored_general_override="$FULL_AUTO_IGNORED_GENERAL_OVERRIDE"
    READY_LAUNCH_BIN="${LAUNCH_TPL%% *}"
  elif [ -n "$auto" ]; then
    LAUNCH_TPL="$AUTO_LAUNCH_TPL"
    READY_LAUNCH_BIN="${LAUNCH_TPL%% *}"
  fi

  # A named target is identified before the tmux/worktree gates because an
  # epic never crosses either boundary: it is an engine scope, not a story to
  # claim and hand to one provider session. `--next` cannot return epics and
  # retains its existing gate order below.
  local show_json="" result="" title="" state=""
  if [ -n "$id" ]; then
    require_story
    resolve_project || fail "$CHECKOUT_ERROR"
    show_json=$(story_cli show "$id" --json 2>/dev/null) || true
    result=$(printf '%s' "$show_json" | jq -r '.result // ""' 2>/dev/null || printf '')
    if [ "$result" != "ok" ]; then
      fail "$(printf '%s' "$show_json" | jq -r --arg id "$id" '.error // ("story `" + $id + "` not found")' 2>/dev/null)"
    fi
    # Every later resource name and the engine scope use the canonical id.
    id=$(canonical_story_id "$show_json" "$id")
    title=$(printf '%s' "$show_json" | jq -r '.story.story.title // ""')
    state=$(printf '%s' "$show_json" | jq -r '.story.story.state // ""')

    # SH-499: epic identity is the explicit type, never the mere presence of
    # a parent-of edge. Ordinary stories with subtasks still reach ID MODE.
    if [ "$(printf '%s' "$show_json" | jq -r '.story.story.story_type // ""')" = "epic" ]; then
      [ -z "$resume" ] \
        || fail "--resume applies only to an ordinary named story — $id is an epic and has no story worktree or pane to reconstruct."
      # An epic dispatch never reaches LAUNCH_TPL -- SH-468's engine run
      # payload carries only agent/lanes, the same "engine lanes keep
      # today's behavior" boundary SH-517 already draws for --full-auto.
      # A resolved selector here would otherwise validate cleanly and then
      # be silently discarded; refuse by name instead, matching the
      # $STORY_LAUNCH_CMD refusal just above rather than a quiet no-op.
      [ -n "$resolved_model" ] || [ -n "$resolved_effort" ] || [ "$resolved_speed" != standard ] \
        && fail "--model/--effort/--speed are not yet supported for an epic's Full Auto engine run -- $id is an epic. Omit the selector, or dispatch a single story instead."
      cmd_dispatch_epic "$id" "$auto" "$full_auto" "$force"
      return 0
    fi
  fi

  # Step 1: tmux precondition for story/lane dispatch (relaxed under dry-run
  # and under
  # STORY_TARGET_SESSION — a non-interactive caller outside tmux dispatches
  # into a NAMED session, so its own tmux context is irrelevant).
  if [ -z "$DRY_RUN" ] && [ -z "$TARGET_SESSION" ]; then
    [ -n "${TMUX:-}" ] || fail "story requires tmux — run $AGENT_LABEL inside a tmux session."
    [ -n "${TMUX_PANE:-}" ] || fail "story requires \$TMUX_PANE — run $AGENT_LABEL inside a tmux pane."
  fi

  # Steps 2-3: the project's checkout — a LOOKUP, never an inference from the
  # caller's working directory. Every refusal here lands BEFORE the
  # compare-and-swap claim in step 6, so a dispatch that cannot proceed never
  # strands a story in the claimed state with no worktree to show for it.
  if [ -n "$want_next" ]; then
    require_story
    resolve_checkout || fail "$CHECKOUT_ERROR"
  else
    require_checkout || fail "$CHECKOUT_ERROR"
  fi
  enter_checkout
  local dir="$PROJECT_ROOT"

  # Resume inventory runs before the claim gate and before every write. The
  # deterministic story identity gives all four expected resource names; any
  # disagreement is a safety refusal, never an invitation to overwrite it.
  local wname="" wt_container="" worktree_path="" worktree_branch=""
  local registered_branch="" registered_worktree=false existing_pane=""
  local worktree_reused=false branch_reused=false window_reused=false resumed=false
  local worktree_created=false branch_created=false resources_exist=false artifacts_exist=false
  local worktree_status="missing" branch_status="missing" window_status="missing" claim_status="missing"
  local claim_state=""
  local resources_json='{}'
  if [ -n "$id" ]; then
    wname=$(resolve_wname "$id")
    wt_container="${WORKTREE_IGNORE_PATH%/}"
    worktree_path="$dir/$wt_container/$wname"
    worktree_branch="worktree-$wname"

    if registered_branch=$(registered_worktree_branch "$worktree_path"); then
      registered_worktree=true
      resources_exist=true
      artifacts_exist=true
      worktree_status="present"
      worktree_reused=true
      branch_reused=true
      [ -d "$worktree_path" ] || refuse "resume-unsafe" \
        "story $id has a stale git worktree registration at \`$worktree_path\`, but that directory is missing. Run \`git worktree repair\` or remove the stale registration explicitly before retrying."
      [ "$registered_branch" = "$worktree_branch" ] || refuse "resume-unsafe" \
        "story $id's expected worktree path is registered on branch \`$registered_branch\`, not \`$worktree_branch\`; refusing to replace or repurpose it."
    elif [ -e "$worktree_path" ]; then
      refuse "resume-unsafe" \
        "story $id's expected path \`$worktree_path\` exists but is not a registered git worktree; refusing to overwrite unclassified evidence."
    fi

    if local_branch_exists "$worktree_branch"; then
      resources_exist=true
      artifacts_exist=true
      branch_status="present"
      branch_reused=true
      is_protected_branch "$worktree_branch" && refuse "resume-unsafe" \
        "story $id's expected branch \`$worktree_branch\` is protected by repository policy; refusing to use it as a disposable story branch."
    elif [ "$registered_worktree" = true ]; then
      refuse "resume-unsafe" \
        "story $id's registered worktree names branch \`$worktree_branch\`, but that local branch does not exist; refusing to infer ownership from an inconsistent registration."
    fi

    existing_pane=$(pane_for_window "$wname") || existing_pane=""
    if [ -n "$existing_pane" ]; then
      resources_exist=true
      artifacts_exist=true
      window_status="present"
    fi

    claim_state=$(story_active_state)
    if [ "$state" = "$claim_state" ]; then
      resources_exist=true
      claim_status="present"
    fi

    resources_json=$(jq -n \
      --arg worktree "$worktree_status" --arg branch "$branch_status" \
      --arg window "$window_status" --arg claim "$claim_status" --arg path "$worktree_path" \
      --arg branch_name "$worktree_branch" --arg pane "$existing_pane" \
      '{claim:$claim, worktree:$worktree, branch:$branch, window:$window,
        worktree_path:$path, branch_name:$branch_name}
       + (if $pane == "" then {} else {pane:$pane} end)')

    if [ -z "$resume" ] \
       && { [ "$artifacts_exist" = true ] || { [ "$claim_status" = present ] && [ -z "$force" ]; }; }; then
      local recovery_display
      if [ "$artifacts_exist" = true ] && [ "$claim_status" = present ]; then
        recovery_display="story $id already has surviving dispatch resources: its \`$claim_state\` claim and the dispatch artifacts reported below. Re-run with \`--resume\` to preserve and reconstruct them, or inspect the inventory before changing anything."
      elif [ "$artifacts_exist" = true ]; then
        recovery_display="story $id already has surviving dispatch artifacts reported below. Re-run with \`--resume\` to preserve and reconstruct them, or inspect the inventory before changing anything."
      else
        recovery_display="story $id already has a surviving \`$claim_state\` claim. Re-run with \`--resume\` to preserve it, use \`--force\` to reuse only this claim for a fresh dispatch, or inspect the inventory before changing anything."
      fi
      refuse_with "resume-available" \
        "$recovery_display" \
        "$(jq -n --argjson resources "$resources_json" '{resources:$resources}')"
    fi
    if [ -n "$resume" ] && [ -n "$existing_pane" ] \
       && [ "$existing_pane" = "${TMUX_PANE:-}" ]; then
      refuse_with "resume-unsafe" \
        "story $id's surviving window is the current pane \`$existing_pane\`; refusing to kill and respawn the dispatcher itself." \
        "$(jq -n --argjson resources "$resources_json" '{resources:$resources}')"
    fi
    [ -z "$resume" ] || [ "$resources_exist" != true ] || resumed=true
  fi

  # Steps 4-6: story identified, verified ready, and claimed. Two mutually
  # exclusive paths — ID MODE (a caller-named story) and NEXT MODE (SH-344,
  # --next, id-less) — that converge on the same four facts ($id, $title,
  # $state, $pre_claim_state) plus a $claim_cmd_desc for the dry-run preview,
  # so everything from here on (worktree, tmux, the claim-rollback path) runs
  # identically regardless of which one produced them. Since SH-482 both write
  # through `story claim`, so those four facts are read out of one response
  # shape on both paths as well.
  local pre_claim_state claim_cmd_desc
  local claim_transitioned=false reused_claim=false
  if [ -n "$want_next" ]; then
    # NEXT MODE (SH-344): a single `story claim --next` call does steps 4-6 in
    # one go for an id nobody named — pick a ready story AND claim it,
    # atomically, inside one write transaction, so two concurrent
    # `dispatch --next` callers are handed two DIFFERENT stories rather than
    # one winner and N-1 `claim-conflict` refusals. This is a second mode
    # rather than a replacement for ID MODE, because `--next` picks whatever
    # is top-priority and so has no way to honor a caller-supplied id — but
    # since SH-482 the two modes reach the store through the SAME verb, and
    # what remains different between them is only how the story is CHOSEN.
    #
    # --no-comment (SH-477): the claim's default comment names the CALLING
    # process's tmux window, which here is the dispatcher's own, not the
    # window dispatch is about to create for the work. That window cannot be
    # named yet either — its name derives from the id this very call is what
    # produces. So the claim stays silent rather than recording a window the
    # work will not happen in (SH-476: never a fabricated window).
    claim_cmd_desc="story claim --next --no-comment"
    local next_json next_result next_cmd_ran
    if [ -n "$DRY_RUN" ]; then
      # Dry-run: a claim is a WRITE, so preview with the plain, non-claiming
      # read instead — the same reads-real/writes-symbolic asymmetry every
      # other dry-run path in this file follows.
      next_cmd_ran="story next"
      next_json=$(story_cli next --json 2>/dev/null) || true
    else
      next_cmd_ran="story claim --next --no-comment"
      next_json=$(story_cli --actor dispatch claim --next --no-comment --json 2>/dev/null) || true
    fi
    next_result=$(printf '%s' "$next_json" | jq -r '.result // ""' 2>/dev/null || printf '')
    if [ "$next_result" != "ok" ]; then
      fail "$next_cmd_ran failed: $(printf '%s' "$next_json" | jq -r '.error // "emitted no result"' 2>/dev/null)."
    fi
    if [ "$(printf '%s' "$next_json" | jq -r '.story // empty')" = "" ]; then
      fail "$(printf '%s' "$next_json" | jq -r '.message // "no ready stories"' 2>/dev/null) — nothing to dispatch."
    fi
    id=$(printf '%s' "$next_json" | jq -r '.story.story.id')
    title=$(printf '%s' "$next_json" | jq -r '.story.story.title // ""')
    state=$(printf '%s' "$next_json" | jq -r '.story.story.state // ""')
    # Dry-run's `$next_json` came from the non-claiming read, so `.state` is
    # already the pre-claim value and `.claimed_from` was never asked for;
    # the real claim's `.claimed_from` is what the story actually came from.
    if [ -n "$DRY_RUN" ]; then
      pre_claim_state="$state"
    else
      pre_claim_state=$(printf '%s' "$next_json" | jq -r '.claimed_from // ""')
      claim_transitioned=true
    fi
  else
    # ID MODE: a caller-named story, so the guards below have a specific id to
    # answer about and the claim honors it rather than picking. Steps 4-5 are
    # this mode's alone — NEXT MODE's story arrives already claimed and already
    # ready by construction — and step 6 is the shared verb.
    #
    # SH-481: the state meaning "claimed" is whichever one carries this
    # project's `active` role — the SAME resolver cmd_list already uses —
    # not the literal `in-progress`. SH-125 requires
    # `in-progress` to exist in every project, but the role is assigned
    # separately, and `is_claimable` (src/domain.rs) drops a story from
    # readiness only when its state equals the ACTIVE-ROLE one. A project that
    # moved the role therefore got a claim that did not claim: the story went
    # somewhere valid, reported ok, and `story next` kept handing it out.
    #
    # Resolved HERE, after `story show` has already answered, and not earlier:
    # story_active_state degrades quietly to `in-progress` when it cannot be
    # asked, so a CLI that cannot answer at all must have failed loudly before
    # this line rather than being papered over by that fallback (cmd_list
    # documents the same ordering hazard from the other side).
    [ -n "$claim_state" ] || claim_state=$(story_active_state)

    # Step 5 (deviation #1 — see header): ALREADY-CLAIMED GUARD. Checked
    # before the ready-gate for its more specific message — since SH-236 the
    # ready-gate below would also catch this, just less helpfully.
    if [ "$state" = "$claim_state" ]; then
      if [ -z "$force" ] && [ -z "$resume" ]; then
        fail "story $id is already \`$claim_state\` — not redispatching (a previous \`/story do\` may still be running against it; pass \`--resume\` to reconstruct that work, pass \`--force\` to reuse only the claim when no resources survive, or move it back to a ready state first if it should restart cleanly)."
      fi
      reused_claim=true
      [ -z "$resume" ] || resumed=true
    fi

    # Step 6 (deviation #2 — see header): READY-STATE GATE, issue #40's core
    # ask. Ground-truth membership check against the CLI's own `is_ready()`,
    # via the exact command every interactive skill already relies on.
    pre_claim_state="$state"
    claim_cmd_desc=""
    if [ "$reused_claim" != true ]; then
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

      # SH-482: the claim IS `story claim <id>` — the same verb NEXT MODE
      # reaches through `--next`, so both dispatch modes now write through one
      # primitive. What that buys over the `show` + `move --if-state` dance it
      # replaces is that the active-role target, the compare-and-swap and the
      # `claimed_from` answer are all resolved inside the CLI's own write
      # transaction: this script no longer holds a second opinion about any of
      # them, which is the drift SH-481 was filed for.
      #
      # --no-comment for the reason SH-477 gave NEXT MODE: `story claim`'s
      # default sentence names the CALLING process's tmux window, which here is
      # the DISPATCHER's, not the window this dispatch is about to open for the
      # work. A claim that named it would be recording a window the work will
      # not happen in, and SH-476's determination forbids a fabricated one
      # outright. What dispatch should say once that window genuinely exists is
      # SH-490's question, for both modes at once, and is deliberately not
      # answered here.
      #
      # The precondition NARROWS, which is the whole behaviour delta of the
      # collapse and is stated rather than glossed. `--if-state` refused any
      # state change at all between this script's `show` and its write; the verb
      # refuses exactly "already claimed" (SH-476: a claim's precondition is not
      # one slug, it is any state but the active one). The race the refusal
      # exists for is still refused — and more strongly, since the verb CASes on
      # the story's head sequence inside one transaction rather than on a
      # witness read a round trip earlier — while the transitions that stop
      # being refused were ones the old message described falsely anyway.
      #
      # The forced pre-claimed path above never reaches this claim and therefore
      # never writes a redundant transition.
      claim_cmd_desc="story claim $id --no-comment"

      if [ -z "$DRY_RUN" ]; then
        local claim_json claim_result claim_error states
        claim_json=$(story_cli --actor dispatch claim "$id" --no-comment --json 2>/dev/null) || true
        claim_result=$(printf '%s' "$claim_json" | jq -r '.result // ""' 2>/dev/null || printf '')
        case "$claim_result" in
          ok)
            # Both facts come from the claim's own answer rather than from
            # $claim_state and the earlier `show`: `.claimed_from` is the state
            # the CLI actually moved the story out of, and `.story.story.state`
            # is a fresh read taken after the transition hooks ran, so a hook
            # that moved the story again is visible here instead of being
            # overwritten by what this script assumed.
            pre_claim_state=$(printf '%s' "$claim_json" | jq -r '.claimed_from // ""' 2>/dev/null || printf '')
            state=$(printf '%s' "$claim_json" | jq -r '.story.story.state // ""' 2>/dev/null || printf '')
            claim_transitioned=true ;;
          conflict)
            refuse "claim-conflict" "story $id was claimed before this dispatch could claim it (now \`$(printf '%s' "$claim_json" | jq -r '.actual // "?"' 2>/dev/null)\`) — another dispatch likely won the race." ;;
          *)
            claim_error=$(printf '%s' "$claim_json" | jq -r '.error // ""' 2>/dev/null) || true
            [ -n "$claim_error" ] || claim_error="story claim emitted no result"
            if states=$(missing_claim_state_vocabulary "$claim_state"); then
              refuse "claim-state-missing" \
                "story $id cannot be claimed: this Storyhook project's state vocabulary has no \`$claim_state\` state (it defines: $states) — run \`story doctor --fix\` in this repo to add it, then re-run this dispatch. (\`story claim\` reported: $claim_error)"
            fi
            fail "story claim $id failed: $claim_error." ;;
        esac
      fi
    fi
  fi

  # NEXT mode learns its deterministic resource names only after the atomic
  # claim chooses an id. Resume is intentionally unavailable in this mode.
  if [ -n "$want_next" ]; then
    wname=$(resolve_wname "$id")
    wt_container="${WORKTREE_IGNORE_PATH%/}"
    worktree_path="$dir/$wt_container/$wname"
    worktree_branch="worktree-$wname"
    resources_json=$(jq -n --arg path "$worktree_path" --arg branch "$worktree_branch" \
      '{worktree:"missing", branch:"missing", window:"missing",
        worktree_path:$path, branch_name:$branch}')
  fi

  # SH-219: --auto gets one of TWO charters, decided here rather than left to
  # the child's own judgment about its skill roster -- council_vote_available
  # probes for the actual ‘/council-vote’ skill file (see its own doc comment
  # above cmd_dispatch). $council is also surfaced in the result JSON below so
  # a caller can tell which charter went out without re-deriving it from
  # prompt text.
  local prompt_tpl="$PROMPT_TPL" prompt_builtin="false" council="false"
  [ -z "${STORY_PROMPT:-}" ] && prompt_builtin="true"
  if [ -n "$auto" ]; then
    prompt_builtin="false"
    if council_vote_available "$dir"; then
      council="true"
    fi
    # An explicit STORY_AUTO_PROMPT is a wholesale override of the ENTIRE
    # autonomous charter -- same seam contract it has always had -- so it
    # wins regardless of what the probe found; $council above still reports
    # the probe's own ground truth for the result JSON either way. Only a
    # caller who left STORY_AUTO_PROMPT unset gets the composed charter that
    # actually matches this machine.
    if [ -n "${STORY_AUTO_PROMPT:-}" ] || [ "$council" = "true" ]; then
      prompt_tpl="$AUTO_PROMPT_TPL"
      [ -z "${STORY_AUTO_PROMPT:-}" ] && prompt_builtin="true"
    else
      prompt_tpl="$AUTO_PROMPT_SOLO_TPL"
      [ -z "${STORY_AUTO_PROMPT_SOLO:-}" ] && prompt_builtin="true"
    fi
  fi
  if [ "$AGENT" = "codex" ] && [ "$prompt_builtin" = "true" ]; then
    prompt_tpl="$prompt_tpl $CODEX_BUILTIN_CLAUSE"
  fi
  # Keep legacy placeholders available to wholesale prompt overrides even
  # though the built-in charters no longer delegate completion or reap.
  local launch_cmd prompt reap_cmd completion_state=""
  local auto_marker="" full_auto_marker="" marker_tmux_args=""
  reap_cmd="bash \"$SELF_PATH\" --project \"$PROJECT_SLUG\" reap \"$id\""
  if [ -n "$auto" ]; then
    completion_state=$(story_closed_state)
  fi
  launch_cmd=$(render_template "$LAUNCH_TPL" "$id" "$wname" "$dir")
  if [ -n "$full_auto" ]; then
    full_auto_marker="$id"
  elif [ -n "$auto" ]; then
    auto_marker="$id"
  fi
  # STORYHOOK_DISPATCH=1 on EVERY dispatch, auto or not (SH-544) — unlike
  # auto_marker/full_auto_marker, which name a specific story only for an
  # --auto/--full-auto launch. session-start.sh reads it to decide whether
  # THIS SessionStart may spend longer than an ordinary human launch waiting
  # on a busy daemon (a dispatched pane is polled for, not watched), without
  # slowing every interactive `claude` session down to match. Appended AFTER
  # the AUTO/FULL_AUTO pair, never between: several tests in this suite
  # assert the exact contiguous substring "-e STORYHOOK_AUTO=… -e
  # STORYHOOK_FULL_AUTO=…", and appending here keeps that substring intact.
  marker_tmux_args="-e STORYHOOK_AUTO=$auto_marker -e STORYHOOK_FULL_AUTO=$full_auto_marker -e STORYHOOK_DISPATCH=1 "
  prompt=$(render_template "$prompt_tpl" "$id" "$wname" "$dir" "$reap_cmd" "$completion_state")
  [ "$resumed" != true ] || prompt="$prompt $RESUME_PROMPT_CLAUSE"
  [ -n "$PROMPT_EXTRA" ] && prompt="$prompt $PROMPT_EXTRA"

  # Surfaced in both the dry-run and real result. SH-511 removed autonomous
  # dispatch's last human interaction: the packaged hook accepts Claude's
  # plan review, Codex's exact-pane watcher accepts its plan, and both providers
  # deny question tools instead of waiting for a person who is not there.
  local auto_note=""
  if [ -n "$auto" ]; then
    local autonomy_label="Autonomous (--auto)"
    [ -z "$full_auto" ] || autonomy_label="Full Auto (--auto --full-auto)"
    if [ "$council" = "true" ]; then
      auto_note=" $autonomy_label: the child starts in Plan mode, Storyhook approves the plan automatically, and the provider launch posture handles later permissions. It researches and decides its easy questions itself, convenes council-vote for the genuinely hard ones, then publishes one linked PR and hands $id to the centralized verifier."
    else
      auto_note=" $autonomy_label: the child starts in Plan mode, Storyhook approves the plan automatically, and the provider launch posture handles later permissions. No council-vote skill was found, so it researches and decides its hard questions too, rather than convening a council. It then publishes one linked PR and hands $id to the centralized verifier."
    fi
    [ "$AGENT" != "codex" ] || auto_note="$auto_note Codex has no stable machine-readable skill inventory, so automatic council discovery uses the safe solo charter unless STORY_COUNCIL=on is set explicitly."
    if [ -n "$full_auto" ]; then
      auto_note="$auto_note Full Auto launch source: $launch_source (launch_overridden=$launch_overridden)."
    fi
    [ "$launch_overridden" = false ] || auto_note="$auto_note Launch override $launch_source is active and may weaken unattendedness."
    [ -z "$ignored_general_override" ] || auto_note="$auto_note Ignored $ignored_general_override because Full Auto accepts only its dedicated launch override."
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
    local dry_claim_note="would claim it via \`$claim_cmd_desc\`"
    if [ "$reused_claim" = true ]; then
      if [ -n "$resume" ]; then
        dry_claim_note="would reuse its existing \`$state\` claim (\`--resume\`; no state transition)"
      else
        dry_claim_note="would reuse its existing \`$state\` claim (\`--force\`; no state transition)"
      fi
    fi
    jq -n \
      --arg id "$id" --arg title "$title" --arg dir "$dir" \
      --arg agent "$AGENT" --arg agent_label "$AGENT_LABEL" \
      --arg submit_key "$SUBMIT_KEY" --arg plan_key "$CODEX_PLAN_KEY" \
      --arg approval_hook "$AUTO_APPROVAL_HOOK" \
      --arg wname "$wname" --arg launch "$launch_cmd" --arg prompt "$prompt" \
      --arg state "$state" --arg claim_cmd "$claim_cmd_desc" \
      --arg claim_note "$dry_claim_note" \
      --arg ignore_status "$ignore_status" \
      --arg detach "$detach" --arg target "$target" \
      --argjson auto "$([ -n "$auto" ] && echo true || echo false)" --arg auto_note "$auto_note" \
      --argjson full_auto "$([ -n "$full_auto" ] && echo true || echo false)" \
      --arg launch_source "$launch_source" \
      --argjson launch_overridden "$launch_overridden" \
      --arg ignored_general_override "$ignored_general_override" \
      --arg marker_tmux_args "$marker_tmux_args" \
      --arg auto_marker "$auto_marker" --arg full_auto_marker "$full_auto_marker" \
      --argjson council "$council" \
      --argjson forced "$([ -n "$force" ] && echo true || echo false)" \
      --argjson reused_claim "$reused_claim" \
      --argjson resume_requested "$([ -n "$resume" ] && echo true || echo false)" \
      --argjson resumed "$resumed" --argjson resources "$resources_json" \
      --argjson worktree_reused "$worktree_reused" --argjson branch_reused "$branch_reused" \
      --argjson window_reused "$window_reused" --arg pane "$existing_pane" \
      --arg wtpath "$worktree_path" --arg wtbranch "$worktree_branch" \
      --arg model "$effective_model" --arg effort "$resolved_effort" --arg speed "$resolved_speed" '
      {
        ok: true, dry_run: true,
        id: $id, title: $title, dir: $dir,
        window_name: $wname, prompt: $prompt, state: $state, auto: $auto, council: $council,
        forced: $forced, reused_claim: $reused_claim, claim_transitioned: false,
        resume_requested: $resume_requested, resumed: $resumed, resources: $resources,
        worktree_reused: $worktree_reused, branch_reused: $branch_reused,
        window_reused: $window_reused, worktree_created: false, branch_created: false,
        agent: $agent, agent_label: $agent_label, submit_key: $submit_key,
        worktree_branch: $wtbranch, worktree_path: $wtpath,
        gitignore: (if $ignore_status == "already-ignored" then "already-ignored" else "would-add" end),
        commands: ((if $claim_cmd == "" then [] else [$claim_cmd] end)
          + (if $worktree_reused then []
             elif $branch_reused then [("git worktree add " + $wtpath + " " + $wtbranch)]
             else [("git worktree add --no-track -b " + $wtbranch + " " + $wtpath + " <base-oid>")] end)
          + [(if $window_reused then
               ("tmux respawn-pane -k -c " + $wtpath + $marker_tmux_args + "-t " + $pane + " " + $launch)
             else
               ("tmux new-window " + $target + $detach + $marker_tmux_args + "-c " + $wtpath + " -n " + $wname + " -P -F #{pane_id} " + $launch
                + " \\; set-window-option -t " + $wname + " remain-on-exit on"
                + " \\; set-window-option -t " + $wname + " automatic-rename off"
                + " \\; set-window-option -t " + $wname + " allow-rename off"
                + " \\; set-window-option -t " + $wname + " @storyhook-agent " + $agent)
             end),
          (if $agent == "codex" then ("tmux send-keys -t <pane> " + $plan_key + " # if Plan footer is absent") else empty end),
          (if $agent == "codex" and $auto then
             ("tmux run-shell -b -t <pane> env STORYHOOK_AUTO=" + $auto_marker
              + " STORYHOOK_FULL_AUTO=" + $full_auto_marker + " bash " + $approval_hook
              + " --approve-codex-plan <pane>")
           else empty end),
          ("printf %s " + $prompt + " | tmux load-buffer -b story-" + $id + " -"),
          ("tmux paste-buffer -p -d -b story-" + $id + " -t <pane>"),
          ("tmux send-keys -t <pane> " + $submit_key)
        ]),
        display: ("[story] DRY RUN for " + $id + " (" + $title
                  + "): " + $claim_note + ", create worktree " + $wtpath + " (branch " + $wtbranch
                  + "), open a new tmux window named " + $wname + " with " + $agent_label
                  + ", and run the listed commands." + $auto_note)
      }
      + (if $auto then {
          launch_source: $launch_source,
          launch_overridden: $launch_overridden
        } else {} end)
      + (if $full_auto then {full_auto: true} else {} end)
      + (if $ignored_general_override == "" then {}
         else {ignored_general_override: $ignored_general_override} end)
      + (if $model == "" then {} else {model: $model} end)
      + (if $effort == "" then {} else {effort: $effort} end)
      + {speed: $speed}'
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
  # story in the claimed state.
  local default fetch_rc=0
  default=$(default_branch)
  local base_oid="" base_fresh=false base_note=""
  if [ "$worktree_reused" = true ]; then
    base_oid=$(git -C "$worktree_path" rev-parse --verify 'HEAD^{commit}' 2>/dev/null) \
      || fail "cannot resolve HEAD in the surviving worktree at $worktree_path.$(claim_rollback_note "$id" "$pre_claim_state" "$claim_transitioned" "$state")"
    base_note="resumed the existing worktree at ${base_oid:0:8}; no base refresh or reset was attempted"
  elif [ "$branch_reused" = true ]; then
    base_oid=$(git rev-parse --verify "refs/heads/$worktree_branch^{commit}" 2>/dev/null) \
      || fail "cannot resolve the surviving branch \`$worktree_branch\` to a commit.$(claim_rollback_note "$id" "$pre_claim_state" "$claim_transitioned" "$state")"
    base_note="reattached surviving branch $worktree_branch at ${base_oid:0:8}; no base refresh or reset was attempted"
  else
    git fetch --quiet origin "+refs/heads/$default:refs/remotes/origin/$default" \
      >/dev/null 2>&1 || fetch_rc=$?
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
        fail "could not determine a fresh origin/$default and STORY_REQUIRE_FRESH_BASE is set — refusing to dispatch on a possibly-stale base.$(claim_rollback_note "$id" "$pre_claim_state" "$claim_transitioned" "$state")"
      fi
    else
      fail "cannot resolve a base commit for the new worktree (no origin/$default and HEAD has no commits).$(claim_rollback_note "$id" "$pre_claim_state" "$claim_transitioned" "$state")"
    fi
  fi

  # Step 9: preserve a valid existing worktree, reattach a branch-only find,
  # or create both resources for a genuinely fresh dispatch.
  local wt_err
  if [ "$worktree_reused" = true ]; then
    :
  elif [ "$branch_reused" = true ]; then
    if ! wt_err=$(git worktree add "$worktree_path" "$worktree_branch" 2>&1); then
      fail "failed to reattach surviving branch \`$worktree_branch\` at $worktree_path: $(printf '%s' "$wt_err" | tail -n 2)$(claim_rollback_note "$id" "$pre_claim_state" "$claim_transitioned" "$state")"
    fi
    worktree_created=true
  else
    if ! wt_err=$(git worktree add --no-track -b "$worktree_branch" "$worktree_path" "$base_oid" 2>&1); then
      fail "failed to create worktree at $worktree_path: $(printf '%s' "$wt_err" | tail -n 2)$(claim_rollback_note "$id" "$pre_claim_state" "$claim_transitioned" "$state")"
    fi
    worktree_created=true
    branch_created=true
  fi

  # Step 9b: ensure the target session exists (SH-50, opt-in via
  # STORY_CREATE_SESSION — see that var's definition above). A caller that
  # names a session without asking for creation gets today's behavior
  # unchanged: `tmux new-window -t <name>:` itself refuses if the session
  # isn't there, and step 10 below reports that the ordinary way. A creation
  # failure here rolls back the worktree/branch and the claim exactly like a
  # failed `new-window` does, so a session that couldn't be made never leaves
  # a worktree with nothing to show for it.
  local session_created=false
  if [ -n "$TARGET_SESSION" ] && [ -n "$CREATE_SESSION" ] \
     && ! tmux has-session -t "$TARGET_SESSION" 2>/dev/null; then
    if ! tmux new-session -d -s "$TARGET_SESSION" -c "$dir" 2>/dev/null; then
      cleanup_dispatch_git "$worktree_path" "$worktree_branch" "$worktree_created" "$branch_created" || true
      fail "failed to create tmux session \`$TARGET_SESSION\`. $(dispatch_cleanup_note).$(claim_rollback_note "$id" "$pre_claim_state" "$claim_transitioned" "$state")"
    fi
    session_created=true
  fi

  # Step 10: open the window running the launch command directly (rooted IN
  # the new worktree). A failure here rolls back the just-created
  # worktree/branch and the claim so a failed dispatch leaves no litter.
  #
  # EXEC, DON'T TYPE (SH-226 -> SH-230). The launch command is passed as
  # `new-window`'s own trailing shell-command argument, which spawns a
  # DEDICATED, single-purpose shell to run it once, rather than typing it as
  # keystrokes into an ALREADY-RUNNING interactive login shell whose rc
  # files (oh-my-zsh's update nag, for one) have already executed and can
  # still be holding an interactive prompt open to swallow exactly those
  # keystrokes — the root cause SH-226 traced four accidental story closures
  # to. Verified empirically against this machine's tmux (3.7b): for the
  # simple single-command case every shipped launch template actually is,
  # `#{pane_current_command}` reports the launched binary itself within
  # 300ms, not an intervening shell — a one-shot `-c` invocation never
  # sources the interactive-only blocks those nags live in, so the class is
  # gone at its root here, not narrowed by the process check Part A added on
  # top of it.
  #
  # `remain-on-exit on`, `automatic-rename off` and `allow-rename off` are
  # chained onto the SAME tmux invocation via `\;` rather than set
  # afterward in separate `tmux` calls (as this used to do, and as
  # cmd_doctor's own scratch window below still does): each separate `tmux`
  # call is a fresh process launch, and with the shell command now
  # executing AT window-creation time rather than after a later typed step,
  # that gap is a real window for a title escape to land in before the pin
  # does — chaining collapses it to one tmux-server-side command batch
  # instead of N round trips through this script.
  #
  # remain-on-exit matters because execing the launch, rather than typing it
  # into a shell that persists after claude exits, means nothing survives in
  # that pane once claude's process ends — crashed, refused, or simply
  # finished without running <reap>. Today's "the shell resumes" behavior is
  # how an operator inspects pane_tail after a failure; remain-on-exit's
  # frozen dead pane preserves exactly that diagnostic capability
  # (tmux capture-pane still reads it) without leaving a LIVE shell a stray
  # keystroke could execute a command in. This is a deliberate, documented
  # behavior change: an attended user can no longer keep typing in that same
  # pane after claude exits normally — `tmux respawn-pane -t <pane>`
  # reactivates it if that is wanted.
  # The chained set-window-option calls are BEST-EFFORT, exactly like the
  # separate `|| true`-guarded calls this replaced: `tmux` reports a single
  # exit code for the WHOLE `\;`-chained invocation, and that code reflects
  # the LAST failure in the chain, not specifically new-window's own result
  # (verified empirically: a later set-window-option targeting a bad target
  # makes tmux exit 1 even though new-window already succeeded, the window
  # already exists, and `-P -F` already printed its pane id). Testing the
  # combined exit code here would roll back — and leak, since the rollback
  # below never kills a window it believes was never created — a dispatch
  # that actually succeeded, over a cosmetic option pin failing. The pane id
  # itself is the only fact that can only be true if new-window succeeded, so
  # it alone decides failure; stderr is still discarded either way since none
  # of these calls has a message worth surfacing on the happy path.
  local new_window_args pane="" window set_target
  set_target="$wname"
  [ -n "$TARGET_SESSION" ] && set_target="$TARGET_SESSION:$wname"
  if [ -n "$existing_pane" ]; then
    pane="$existing_pane"
    # This file is a generated SessionStart witness, not project work. A
    # replacement process must publish its own witness before handoff.
    rm -f "$worktree_path/.claude/dispatch-sentinel.json"
    if ! tmux respawn-pane -k -c "$worktree_path" \
         -e "STORYHOOK_AUTO=$auto_marker" -e "STORYHOOK_FULL_AUTO=$full_auto_marker" \
         -e "STORYHOOK_DISPATCH=1" \
         -t "$pane" "$launch_cmd" 2>/dev/null; then
      cleanup_dispatch_git "$worktree_path" "$worktree_branch" "$worktree_created" "$branch_created" || true
      fail "failed to respawn surviving tmux pane \`$pane\`. $(dispatch_cleanup_note).$(claim_rollback_note "$id" "$pre_claim_state" "$claim_transitioned" "$state")"
    fi
    tmux set-window-option -t "$pane" remain-on-exit on >/dev/null 2>&1 || true
    tmux set-window-option -t "$pane" automatic-rename off >/dev/null 2>&1 || true
    tmux set-window-option -t "$pane" allow-rename off >/dev/null 2>&1 || true
    tmux set-window-option -t "$pane" @storyhook-agent "$AGENT" >/dev/null 2>&1 || true
    window_reused=true
  else
    new_window_args=(-c "$worktree_path" -n "$wname" -P -F '#{pane_id}')
    new_window_args=(-e "STORYHOOK_AUTO=$auto_marker" -e "STORYHOOK_FULL_AUTO=$full_auto_marker" -e "STORYHOOK_DISPATCH=1" "${new_window_args[@]}")
    [ -z "$FOREGROUND" ] && new_window_args=(-d "${new_window_args[@]}")
    [ -n "$TARGET_SESSION" ] && new_window_args=(-t "$TARGET_SESSION:" "${new_window_args[@]}")
    pane=$(tmux new-window "${new_window_args[@]}" "$launch_cmd" \; \
             set-window-option -t "$set_target" remain-on-exit on \; \
             set-window-option -t "$set_target" automatic-rename off \; \
             set-window-option -t "$set_target" allow-rename off \; \
             set-window-option -t "$set_target" @storyhook-agent "$AGENT" \
           2>/dev/null || true)
    if [ -z "$pane" ]; then
      cleanup_dispatch_git "$worktree_path" "$worktree_branch" "$worktree_created" "$branch_created" || true
      fail "failed to open a new tmux window. $(dispatch_cleanup_note).$(claim_rollback_note "$id" "$pre_claim_state" "$claim_transitioned" "$state")"
    fi
  fi
  window=$(tmux display-message -p -t "$pane" '#{window_id}' 2>/dev/null || printf '')
  # The pane's pid, captured right after the window opened: SH-231 (R2, the
  # sentinel/SessionStart-hook readiness redesign) consumes this as the
  # authoritative identity the readiness gate re-checks liveness against,
  # instead of re-deriving one from an unconfirmed hook payload — see
  # wait_ready_sentinel's own doc comment in lib/session.sh. Best-effort — a
  # capture failure here must never fail a dispatch that has already opened
  # its window; wait_ready_sentinel refuses on an empty pid on its own.
  local pane_pid
  pane_pid=$(tmux display-message -p -t "$pane" '#{pane_pid}' 2>/dev/null || printf '')

  # Publish the durable-cleanup handoff before any gate that can leave this
  # window/worktree behind. Every rollback path calls cleanup_dispatch_git,
  # which removes this attempt's marker before preserving or deleting Git
  # resources. A transition to verifying can therefore distinguish a real
  # dispatched worktree from a manual/legacy submission without consulting
  # the project's mutable checkout registration.
  if ! write_cleanup_lease_marker \
      "$PROJECT_SLUG" "$id" "$dir" "$worktree_path" "$worktree_branch" "$pane"; then
    cleanup_dispatch_git "$worktree_path" "$worktree_branch" "$worktree_created" "$branch_created" || true
    refuse "cleanup-lease-unavailable" \
      "[story] $id → the agent window opened, but dispatch could not record its exact cleanup identity. Nothing was typed into that pane; $(dispatch_cleanup_note). The diagnostic window was preserved."
  fi

  # Step 11: readiness GATE before typing the prompt. This gates (SH-226); it
  # used to only annotate. An unconfirmed pane gets no text at all: the charter
  # is an autonomous instruction document whose backticked spans a shell executes
  # as commands, so "type it anyway and warn" is not a safe default when nobody
  # is watching the pane. The window is deliberately left standing — it is the
  # only place the launch failure's own words survive — while the worktree,
  # branch and any claim created by this dispatch are rolled back so an
  # immediate retry is not answered with "already dispatched?" (the collision
  # guard keys on worktree/branch). A forced pre-existing claim stays put.
  local provider_ready=false
  if [ "$AGENT" = "codex" ]; then
    wait_ready "$pane" "$launch_cmd" && provider_ready=true
  else
    wait_ready_sentinel "$pane" "$pane_pid" "$worktree_path" && provider_ready=true
  fi
  if [ "$provider_ready" != true ]; then
    local ready_tail
    ready_tail=$(pane_tail "$pane")
    cleanup_dispatch_git "$worktree_path" "$worktree_branch" "$worktree_created" "$branch_created" || true
    refuse_with pane-not-ready \
      "[story] $id → could not confirm $AGENT_LABEL is running in window \`$wname\` ($(dispatch_ready_note)). Nothing was typed into that pane. The window is left open so you can look at it; $(dispatch_cleanup_note).$(claim_rollback_note "$id" "$pre_claim_state" "$claim_transitioned" "$state")" \
      "$(jq -n --arg id "$id" --arg window "$window" --arg wname "$wname" \
            --arg pane "$pane" --arg cmd "$WAIT_READY_COMMAND" \
            --arg wreason "$WAIT_READY_REASON" --arg tail "$ready_tail" \
            --arg pattern "$READY_PROCESS_PATTERN" --argjson claimed "$reused_claim" \
            '{id:$id, window:$window, window_name:$wname, pane:$pane,
              readiness_confirmed:false, pane_command:$cmd,
              wait_ready_reason:$wreason, ready_process_pattern:$pattern,
              pane_tail:$tail, claimed:$claimed}')"
  fi
  local readiness_confirmed=true

  # Claude selected plan mode in its launch argv. Current Codex exposes a real
  # Plan mode through Shift+Tab, and the footer is the confirmation boundary.
  # Refuse before delivering the charter if that handoff cannot be established.
  local plan_mode_confirmed=true
  if ! ensure_provider_plan_mode "$pane"; then
    local mode_tail
    mode_tail=$(pane_tail "$pane")
    cleanup_dispatch_git "$worktree_path" "$worktree_branch" "$worktree_created" "$branch_created" || true
    refuse_with plan-mode-unconfirmed \
      "[story] $id → $AGENT_LABEL is ready in window \`$wname\`, but its Plan mode could not be confirmed ($PLAN_MODE_REASON). Nothing was typed into that pane. The window is left open; $(dispatch_cleanup_note).$(claim_rollback_note "$id" "$pre_claim_state" "$claim_transitioned" "$state")" \
      "$(jq -n --arg id "$id" --arg window "$window" --arg wname "$wname" \
            --arg pane "$pane" --arg reason "$PLAN_MODE_REASON" --arg tail "$mode_tail" \
            --argjson claimed "$reused_claim" \
            '{id:$id, window:$window, window_name:$wname, pane:$pane,
              readiness_confirmed:true, plan_mode_confirmed:false,
              plan_mode_reason:$reason, pane_tail:$tail, claimed:$claimed}')"
  fi

  # `--approve-for-me` reviews Codex tool calls, not the separate Plan-mode
  # dialog. Refuse before handoff if the exact-pane Return watcher cannot even
  # be armed; otherwise this command would claim unattendedness while knowingly
  # launching a session that must stop for a person.
  if [ -n "$auto" ] && [ "$AGENT" = codex ] \
     && ! schedule_codex_plan_approval "$pane" "$auto_marker" "$full_auto_marker"; then
    local approval_tail
    approval_tail=$(pane_tail "$pane")
    cleanup_dispatch_git "$worktree_path" "$worktree_branch" "$worktree_created" "$branch_created" || true
    refuse_with plan-approval-unarmed \
      "[story] $id → Codex Plan mode is confirmed in window \`$wname\`, but its autonomous plan-approval watcher could not be armed. Nothing was typed into that pane. The window is left open; $(dispatch_cleanup_note).$(claim_rollback_note "$id" "$pre_claim_state" "$claim_transitioned" "$state")" \
      "$(jq -n --arg id "$id" --arg window "$window" --arg wname "$wname" \
            --arg pane "$pane" --arg tail "$approval_tail" --argjson claimed "$reused_claim" \
            '{id:$id, window:$window, window_name:$wname, pane:$pane,
              readiness_confirmed:true, plan_mode_confirmed:true,
              plan_approval_armed:false, pane_tail:$tail, claimed:$claimed}')"
  fi

  # Step 12: type + submit the prompt, confirmed. SEND_PROMPT_PHASE distinguishes
  # a handoff that never left the keyboard from one that may already be in front
  # of a live agent — see the rollback asymmetry below.
  local prompt_confirmed=false prompt_accepted_flag=false
  if send_prompt_confirmed "$pane" "$prompt" "story-$id"; then
    prompt_confirmed=true
    if prompt_accepted "$pane"; then
      prompt_accepted_flag=true
    fi
  else
    local send_tail
    send_tail=$(pane_tail "$pane")
    if [ "$SEND_PROMPT_PHASE" = undelivered ]; then
      # Nothing reached the input box, so nothing was submitted (guaranteed by
      # send_prompt_confirmed: no Enter is sent before receipt is observed).
      # Safe to roll everything back, exactly as a failed window-open does.
      cleanup_dispatch_git "$worktree_path" "$worktree_branch" "$worktree_created" "$branch_created" || true
      refuse_with handoff-undelivered \
      "[story] $id → $AGENT_LABEL is running in window \`$wname\`, but the prompt never reached its input box, so nothing was submitted. The window is left open; $(dispatch_cleanup_note).$(claim_rollback_note "$id" "$pre_claim_state" "$claim_transitioned" "$state")" \
        "$(jq -n --arg id "$id" --arg window "$window" --arg wname "$wname" \
              --arg pane "$pane" --arg tail "$send_tail" --argjson claimed "$reused_claim" \
              '{id:$id, window:$window, window_name:$wname, pane:$pane,
                readiness_confirmed:true, delivery_phase:"undelivered",
                pane_tail:$tail, claimed:$claimed}')"
    fi
    # received-unsubmitted: the box HELD the prompt and submission was never
    # confirmed. It may already be in front of a live agent, so the claim and the
    # worktree STAY — rolling back here would return the story to the ready list
    # and let the next dispatch hand the same work to a second session.
    local release_note="run \`story move $id $pre_claim_state --if-state $state\` to release it"
    [ "$reused_claim" = true ] \
      && release_note="leave or change the pre-existing \`$state\` state explicitly; this dispatch did not create a claim transition to release"
    refuse_with handoff-unconfirmed \
      "[story] $id → $AGENT_LABEL is running in window \`$wname\` and the prompt reached its input box, but the submission was never confirmed. The claim and worktree are DELIBERATELY left in place: the agent may already be working. Look with \`story.sh capture $id\`, then either re-submit in that window or $release_note." \
      "$(jq -n --arg id "$id" --arg window "$window" --arg wname "$wname" \
            --arg pane "$pane" --arg tail "$send_tail" \
            '{id:$id, window:$window, window_name:$wname, pane:$pane,
              readiness_confirmed:true, delivery_phase:"received-unsubmitted",
              pane_tail:$tail, claimed:true}')"
  fi

  # Result. Reaching here means BOTH readiness and submission were confirmed —
  # every other outcome refused above. `ok:true` therefore now means "the story
  # was claimed AND the charter reached a confirmed agent session", which is
  # what a caller has always read it as; before SH-226 it meant only the former,
  # and the difference was carried in prose nothing downstream read.
  local warning="" display base claim_success_note="and claimed it (now \`$state\`)"
  if [ "$reused_claim" = true ]; then
    if [ -n "$resume" ]; then
      claim_success_note="and reused its existing \`$state\` claim (\`--resume\`; no state transition)"
    else
      claim_success_note="and reused its existing \`$state\` claim (\`--force\`; no state transition)"
    fi
  fi
  local session_action="opened tmux window \`$wname\`"
  [ "$window_reused" != true ] || session_action="respawned existing tmux pane \`$pane\` in window \`$wname\`"
  base="[story] $id ($title) → $session_action on worktree \`$worktree_path\` @ \`${base_oid:0:8}\`, launched $AGENT_LABEL with \`$launch_cmd\` (plan mode), submitted the prompt with \`$SUBMIT_KEY\`, $claim_success_note."
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
    --arg agent "$AGENT" --arg agent_label "$AGENT_LABEL" --arg submit_key "$SUBMIT_KEY" \
    --argjson ready "$readiness_confirmed" --argjson plan "$plan_mode_confirmed" \
    --argjson pconf "$prompt_confirmed" \
    --argjson paccept "$prompt_accepted_flag" \
    --argjson auto "$([ -n "$auto" ] && echo true || echo false)" --argjson council "$council" \
    --argjson full_auto "$([ -n "$full_auto" ] && echo true || echo false)" \
    --arg launch_source "$launch_source" \
    --argjson launch_overridden "$launch_overridden" \
    --arg ignored_general_override "$ignored_general_override" \
    --argjson forced "$([ -n "$force" ] && echo true || echo false)" \
    --argjson reused_claim "$reused_claim" --argjson claim_transitioned "$claim_transitioned" \
    --argjson resume_requested "$([ -n "$resume" ] && echo true || echo false)" \
    --argjson resumed "$resumed" --argjson resources "$resources_json" \
    --argjson worktree_reused "$worktree_reused" --argjson branch_reused "$branch_reused" \
    --argjson window_reused "$window_reused" --argjson worktree_created "$worktree_created" \
    --argjson branch_created "$branch_created" \
    --arg warning "$warning" --arg tail "$tail_evidence" --arg display "$display" \
    --arg default "$default" --arg base_oid "$base_oid" --argjson base_fresh "$base_fresh" \
    --arg wtbranch "$worktree_branch" --arg wtpath "$worktree_path" \
    --arg session "$TARGET_SESSION" --argjson session_created "$session_created" \
    --arg pane_pid "$pane_pid" \
    --argjson cleanup_lease "$DISPATCH_CLEANUP_LEASE" \
    --arg model "$effective_model" --arg effort "$resolved_effort" --arg speed "$resolved_speed" '
    {
      ok: true,
      id: $id, title: $title,
      window: $window, window_name: $wname, pane: $pane,
      state: $state, auto: $auto, council: $council,
      forced: $forced, reused_claim: $reused_claim, claim_transitioned: $claim_transitioned,
      resume_requested: $resume_requested, resumed: $resumed, resources: $resources,
      worktree_reused: $worktree_reused, branch_reused: $branch_reused,
      window_reused: $window_reused, worktree_created: $worktree_created,
      branch_created: $branch_created,
      agent: $agent, agent_label: $agent_label, submit_key: $submit_key,
      readiness_confirmed: $ready, plan_mode_confirmed: $plan,
      prompt_confirmed: $pconf, prompt_accepted: $paccept,
      claimed: true, gitignore: $gitignore,
      base_branch: $default, base_ref: ("origin/" + $default),
      base_oid: $base_oid, base_fresh: $base_fresh,
      worktree_branch: $wtbranch, worktree_path: $wtpath,
      cleanup_lease: $cleanup_lease
    }
    + (if $auto then {
        launch_source: $launch_source,
        launch_overridden: $launch_overridden
      } else {} end)
    + (if $full_auto then {full_auto: true} else {} end)
    + (if $ignored_general_override == "" then {}
       else {ignored_general_override: $ignored_general_override} end)
    + (if $warning == "" then {} else {warning: $warning} end)
    + (if $tail == "" then {} else {pane_tail: $tail} end)
    + (if $session == "" then {} else {session: $session, session_created: $session_created} end)
    + (if $pane_pid == "" then {} else {pane_pid: $pane_pid} end)
    + (if $model == "" then {} else {model: $model} end)
    + (if $effort == "" then {} else {effort: $effort} end)
    + {speed: $speed}
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
# carrying the `active` role, else "in-progress". The same convention the
# CLI's own `story claim` resolves server-side.
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
  # Two narrowings on top of it:
  #   - already-claimed stories are dropped. Since storyhook's SH-236,
  #     `list --ready` already excludes an in-progress story on its own, so
  #     this is now redundant defense-in-depth rather than load-bearing —
  #     kept because a no-op filter costs nothing and removing it would only
  #     mask a future storyhook regression here, not prevent one;
  #   - parents are dropped, matching `story next`'s has_children filter,
  #     since dispatching an epic is never what the user meant — this one
  #     stays load-bearing, `list --ready` does not filter parents itself.
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

# ---- subcommands: ensure-cli / context / sync / handoff ---------------------
#
# SH-308. The four read-only session-scaffolding verbs six skills used to
# spell out in prose — each with its own hand-copied "is the CLI installed"
# preamble, and each hand-copying a CLI default (`--since 2h`, `--since 7d`,
# `--stale 3d`) that only the CLI itself is allowed to change. A hand-copied
# default is a value with two owners; `04ac259` found `--since 2h` wrong by
# 12x in exactly this shape (the plugin's own reference doc AND the CLI's help
# text had drifted from `DEFAULT_HANDOFF_HOURS`), and `skills/story-handoff/
# SKILL.md` still said `2h` after that fix landed, because the fix touched
# neither the skill nor this script — this script did not exist yet. The rule
# these four follow: never restate a CLI default, only ever pass one through
# when the caller actually gave one. What the CLI defaults to is the CLI's
# problem now, and it can only ever have one answer.

# cmd_ensure_cli — the ONE verb that must NOT call require_story: its entire
# job is to report whether the CLI is there, so `fail`ing on its absence would
# make the one caller who needs to ask that question unable to. Always
# ok:true — "not installed" is a successful check of a real fact, not a
# failure of this verb.
cmd_ensure_cli() {
  [ "$#" -eq 0 ] || fail "usage: story.sh ensure-cli"
  if ! command -v "$STORY" >/dev/null 2>&1; then
    jq -n '{ok:true, installed:false, version:"",
            display:"[story] the `story` CLI is not installed."}'
    return 0
  fi
  local version
  version=$("$STORY" --version 2>/dev/null | head -1) || version=""
  jq -n --arg version "$version" '
    {ok:true, installed:true, version:$version,
     display:("[story] the CLI is installed (" + (if $version == "" then "version unknown" else $version end) + ").")}'
}

# cmd_context [--full] — `story load-context` already IS the "comprehensive
# project overview" the skill used to assemble by hand from `story context`
# plus a separate `story next --count 3 --json` call; it has covered both
# since the CLI renamed `context` to `load-context` (see `story help
# load-context`). `--full` appends the three deep-dive reads the skill used to
# run itself, none of which take a hand-copyable default: `--critical-path`
# and `--blocked` are flags, and STORY_STALE_THRESHOLD names the one number
# here the CLI has no opinion on at all (`--stale` REQUIRES a value; there is
# no default to drift from) rather than hard-coding it a second place.
cmd_context() {
  local full=""
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --full) full=1; shift ;;
      *) fail "unknown argument \`$1\` — usage: story.sh context [--full]" ;;
    esac
  done
  require_story

  local body
  body=$(story_cli load-context 2>/dev/null) || true
  [ -n "$body" ] || fail "story load-context produced no output."

  if [ -n "$full" ]; then
    local stale="${STORY_STALE_THRESHOLD:-3d}" graph blocked stale_list
    graph=$(story_cli graph --critical-path 2>/dev/null || printf '(unavailable)')
    blocked=$(story_cli list --blocked 2>/dev/null || printf '(unavailable)')
    # NOT soft-degraded like graph/blocked above: those are flags that cannot
    # be misconfigured, but STORY_STALE_THRESHOLD is a user-editable value
    # that CAN be malformed, and the CLI already names the mistake clearly --
    # swallowing it into "(unavailable)" would hide exactly the error a
    # caller most needs to see (same fix as cmd_triage's own stale fetch).
    stale_list=$(story_cli list --stale "$stale" 2>&1) || {
      fail "story list --stale $stale: $stale_list"
    }
    body="$body"$'\n\n## Critical path\n\n'"$graph"$'\n\n## Blocked stories\n\n'"$blocked"$'\n\n## Stale ('"$stale"$'+) stories\n\n'"$stale_list"
  fi

  jq -n --arg display "$body" --argjson full "$([ -n "$full" ] && echo true || echo false)" '
    {ok:true, full:$full, display:$display}'
}

# cmd_sync [--since <duration>] — wraps `story commit-sync`. Idempotent by the
# CLI's own design ("safe to run repeatedly — skips already-synced commits"),
# so unlike the destructive verbs elsewhere in this file this needs no
# STORY_DRY_RUN handling: there is nothing here dry-run would protect against
# that the CLI does not already protect against on every run.
cmd_sync() {
  local since=""
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --since) since="${2:-}"; shift 2 || fail "--since needs a value." ;;
      *) fail "unknown argument \`$1\` — usage: story.sh sync [--since <duration>]" ;;
    esac
  done
  require_story

  local -a args=(commit-sync)
  [ -n "$since" ] && args+=(--since "$since")
  local body
  body=$(story_cli "${args[@]}" 2>/dev/null) || true
  [ -n "$body" ] || fail "story commit-sync produced no output."

  jq -n --arg display "$body" '{ok:true, display:$display}'
}

# cmd_handoff [--since <duration>] — wraps `story handoff` plus `story summary
# --json` for the current-state snapshot the skill's own step 2 asks for
# separately. `--since`, passed through ONLY when given, never restated —
# `story handoff` defaults to DEFAULT_HANDOFF_HOURS on its own, whatever that
# constant is today.
cmd_handoff() {
  local since=""
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --since) since="${2:-}"; shift 2 || fail "--since needs a value." ;;
      *) fail "unknown argument \`$1\` — usage: story.sh handoff [--since <duration>]" ;;
    esac
  done
  require_story

  local -a args=(handoff)
  [ -n "$since" ] && args+=(--since "$since")
  local body summary_json
  body=$(story_cli "${args[@]}" 2>/dev/null) || true
  [ -n "$body" ] || fail "story handoff produced no output."
  summary_json=$(story_cli summary --json 2>/dev/null) || summary_json='{}'
  printf '%s' "$summary_json" | jq -e . >/dev/null 2>&1 || summary_json='{}'

  jq -n --arg display "$body" --argjson summary "$summary_json" '
    {ok:true, display:$display, summary:$summary}'
}

# ---- subcommand: triage -------------------------------------------------------
#
# _find_blocking_cycles — READ-ONLY. stdin is `<blocker-id>\t<blocked-id>`
# edges, one per line (blocker must close before blocked is ready); echoes
# every story id that sits on a cycle, one per line, empty when there is
# none. Kahn's algorithm: repeatedly strip a node with no remaining
# unresolved blocker, decrementing its neighbors' counts; whatever is left
# once nothing more can be stripped cannot be explained by anything BUT a
# cycle, because every acyclic path bottoms out at a node with in-degree
# zero. This is what skills/story-triage/SKILL.md used to hand to the model
# as "eyeball `story graph`'s output ... this is a manual check" — the CLI
# does not expose raw edges via `story graph`, but `story list --json`
# already carries every story's own `blocked-by` relationships, which is
# all a cycle check needs. Bare (no --include-closed): a closed story is
# never a blocker worth resolving -- `is_ready` already treats a
# `blocked-by` edge to a closed story as non-blocking -- so SH-409's
# default exclusion narrows this edge set to exactly the ones a cycle
# here could actually stall, not fewer.
_find_blocking_cycles() {
  awk -F'\t' '
    NF == 2 { blocker[NR]=$1; blocked[NR]=$2; nodes[$1]=1; nodes[$2]=1; n=NR }
    END {
      for (i=1;i<=n;i++) {
        indeg[blocked[i]]++
        adj[blocker[i]] = adj[blocker[i]] (adj[blocker[i]] == "" ? "" : "\x1f") blocked[i]
      }
      qn=0
      for (id in nodes) if (indeg[id]+0 == 0) { queue[qn++]=id; queued[id]=1 }
      qi=0
      while (qi<qn) {
        cur=queue[qi++]
        removed[cur]=1
        split(adj[cur], parts, "\x1f")
        for (k in parts) {
          nb=parts[k]
          if (nb == "") continue
          indeg[nb]--
          if (indeg[nb] == 0 && !(nb in queued)) { queue[qn++]=nb; queued[nb]=1 }
        }
      }
      for (id in nodes) if (!(id in removed)) print id
    }'
}

# cmd_triage — SH-308. Gathers the four reads story-triage's own step 1 used
# to run one at a time (list, --stale, --blocked, and the cycle check above)
# and returns them pre-classified into findings[], so the skill's job
# narrows to presenting them and asking what to do — the actual RESOLUTION
# commands (prioritize/label/block/unblock/relate/…) stay direct `story`
# calls in the skill, exactly as they always were: each is already a single,
# unambiguous CLI invocation with nothing to parse, and re-wrapping an
# already-trivial call here would be complexity this story does not buy
# anything with.
cmd_triage() {
  [ "$#" -eq 0 ] || fail "usage: story.sh triage"
  require_story

  local stale="${STORY_STALE_THRESHOLD:-3d}"
  local list_json stale_json blocked_json
  list_json=$(story_cli list --json 2>/dev/null) || true
  [ -n "$list_json" ] || fail "story list produced no output."

  # NOT defaulted to empty on failure: `--blocked` is a boolean flag that
  # cannot itself be malformed, but `--stale` takes a value
  # (STORY_STALE_THRESHOLD is env-overridable), and a bad one is a real,
  # user-facing error the CLI already names clearly -- swallowing it into
  # "no stale stories" would silently hide exactly the mistake a caller most
  # needs to see. Mirrors _load_ready_stories' own "never a default" rule.
  stale_json=$(story_cli list --stale "$stale" --json 2>/dev/null) || true
  if [ "$(printf '%s' "$stale_json" | jq -r '.result // ""' 2>/dev/null)" != "ok" ]; then
    fail "$(printf '%s' "$stale_json" | jq -r --arg stale "$stale" '.error // ("story list --stale " + $stale + " emitted no result")' 2>/dev/null)"
  fi
  blocked_json=$(story_cli list --blocked --json 2>/dev/null) || blocked_json='{"stories":[]}'

  local edges cycle_ids cycle_json
  edges=$(printf '%s' "$list_json" | jq -r '
    .stories[]? | .story as $s
    | ($s.relationships[]? | select(.relation == "blocked-by") | [.other_id, $s.id] | @tsv)
  ')
  cycle_ids=$(printf '%s\n' "$edges" | _find_blocking_cycles)
  cycle_json=$(printf '%s\n' "$cycle_ids" | jq -R -s 'split("\n") | map(select(length > 0))')

  # A full-project `list --json` is too big for --argjson: it goes on jq's own
  # argv, and a large backlog (this project's own has 300+ stories) blows past
  # the OS's ARG_MAX. --slurpfile reads a FILE's content instead, with no such
  # ceiling — the tradeoff is each blob arrives wrapped in a 1-element array
  # ($all[0], not $all), which the `as` binding below unwraps once.
  local tmpdir
  tmpdir=$(mktemp -d /tmp/story-triage.XXXXXX) || fail "could not create a scratch directory for triage."
  printf '%s' "$list_json"    >"$tmpdir/all.json"
  printf '%s' "$stale_json"   >"$tmpdir/stale.json"
  printf '%s' "$blocked_json" >"$tmpdir/blocked.json"
  printf '%s' "$cycle_json"   >"$tmpdir/cycles.json"

  jq -n --slurpfile all_f "$tmpdir/all.json" --slurpfile stale_f "$tmpdir/stale.json" \
        --slurpfile blocked_f "$tmpdir/blocked.json" --slurpfile cycles_f "$tmpdir/cycles.json" \
        --arg stale_window "$stale" '
    def rows($j): [ $j.stories[]? | .story ];
    ($all_f[0])     as $all
    | ($stale_f[0])   as $stale
    | ($blocked_f[0]) as $blocked
    | ($cycles_f[0])  as $cycles
    | (rows($all))       as $all_rows
    | (rows($stale))   as $stale_rows
    | (rows($blocked)) as $blocked_rows
    # SH-359: "nobody assessed this", not "priority is none". `none` is a real
    # level meaning deliberately parked, and flagging those was the conflation
    # this category existed to surface. `// false` because the field is omitted
    # from the JSON when false (`skip_serializing_if`), so absent means the same
    # as explicitly false -- and it is also what an older daemon sends.
    | ([ $all_rows[] | select((.priority_assessed // false) == false) ])    as $unprioritized
    | ([ $all_rows[] | select((.relationships // []) | length == 0) ])      as $orphans
    | ([ $all_rows[] | select(.id as $id | $cycles | index($id) != null) ]) as $cyc_rows
    | (
        [ $blocked_rows[]    | {id, title, category:"blocked",       detail: (.awaiting // "blocked")} ]
        + [ $stale_rows[]    | {id, title, category:"stale",         detail: ("stale " + $stale_window + "+")} ]
        + [ $unprioritized[] | {id, title, category:"unprioritized", detail: "priority never assessed"} ]
        + [ $cyc_rows[]      | {id, title, category:"cycle",         detail: "on a blocking cycle"} ]
        + [ $orphans[]       | {id, title, category:"orphan",        detail: "no relationships"} ]
      ) as $findings
    | {
        ok: true,
        findings: $findings,
        counts: {
          blocked: ($blocked_rows|length), stale: ($stale_rows|length),
          unprioritized: ($unprioritized|length), cycle: ($cyc_rows|length),
          orphan: ($orphans|length)
        },
        display: (
          "[story] triage — " + ($findings|length|tostring) + " finding(s):\n"
          + (if ($blocked_rows|length) > 0 then
               "\nBlocked (" + ($blocked_rows|length|tostring) + "):\n"
               + ([ $blocked_rows[] | "  " + .id + " -- " + (.title // "(no title)") + (if .awaiting then " (" + .awaiting + ")" else "" end) ] | join("\n"))
             else "" end)
          + (if ($stale_rows|length) > 0 then
               "\nStale (" + $stale_window + "+, " + ($stale_rows|length|tostring) + "):\n"
               + ([ $stale_rows[] | "  " + .id + " -- " + (.title // "(no title)") ] | join("\n"))
             else "" end)
          + (if ($unprioritized|length) > 0 then
               "\nUnprioritized (" + ($unprioritized|length|tostring) + "):\n"
               + ([ $unprioritized[] | "  " + .id + " -- " + (.title // "(no title)") ] | join("\n"))
             else "" end)
          + (if ($cyc_rows|length) > 0 then
               "\nBlocking cycle (" + ($cyc_rows|length|tostring) + "):\n"
               + ([ $cyc_rows[] | "  " + .id + " -- " + (.title // "(no title)") ] | join("\n"))
             else "" end)
          + (if ($orphans|length) > 0 then
               "\nOrphans (" + ($orphans|length|tostring) + "):\n"
               + ([ $orphans[] | "  " + .id + " -- " + (.title // "(no title)") ] | join("\n"))
             else "" end)
          + (if ($findings|length) == 0 then "\nBacklog looks clean." else "" end)
        )
      }'
  rm -rf "$tmpdir"
}

# ---- subcommands: scaffold-{agents,claude}-md --------------------------------
#
# _cmd_scaffold_instructions <kind> <default-path> [--path <file>] — SH-308.
# Sentinel-delimited insert-or-replace of Storyhook's canonical scaffold into
# an agent-instruction file. The shared setup path uses `agents-md`; the
# `claude-md` wrapper remains for the legacy Claude-specific integration.
#
# The exact-scaffold `unchanged` case matters after `story project new`: current
# Storyhook already creates AGENTS.md, and appending a sentinel-wrapped copy
# would duplicate every instruction. Existing user-owned text is never replaced
# unless it is already between the Storyhook sentinel pair.
#
# Originally this implemented only `story scaffold claude-md` into CLAUDE.md,
# file, replacing skills/story-setup/SKILL.md's step 4 hand-rolled text
# surgery ("wrap the output in sentinel markers... if CLAUDE.md already
# contains the begin marker, replace the existing block"). A model doing
# this by hand risks exactly the bug this class of change tends to produce —
# a near-miss on the sentinel string, or a replace that eats a line it
# shouldn't — for a mechanical operation with one right answer. NOT awk: BSD
# awk (this project's target, per lib/session.sh's own header) refuses a raw
# newline inside a `-v` value ("newline in string"), so the multi-line
# replacement block is threaded through a plain bash read loop instead.
_cmd_scaffold_instructions() {
  local kind="$1" default_path="$2"; shift 2
  local path="$default_path" verb="scaffold-${kind}"
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --path) path="${2:-}"; shift 2 || fail "--path needs a value." ;;
      *) fail "unknown argument \`$1\` — usage: story.sh $verb [--path <file>]" ;;
    esac
  done
  [ -n "$path" ] || fail "--path was given no value."
  require_story

  local content
  content=$(story_cli scaffold "$kind" 2>/dev/null) || true
  [ -n "$content" ] || fail "story scaffold $kind produced no output."

  local content_file
  content_file=$(mktemp /tmp/story-scaffold.XXXXXX) || fail "could not create a scratch file."
  printf '%s\n' "$content" >"$content_file"

  local begin='<!-- BEGIN STORYHOOK -->' end='<!-- END STORYHOOK -->'
  local action
  if [ -f "$path" ]; then
    local begin_count end_count begin_line end_line
    begin_count=$(grep -cxF "$begin" "$path" 2>/dev/null || true)
    end_count=$(grep -cxF "$end" "$path" 2>/dev/null || true)
    begin_line=$(grep -nxF "$begin" "$path" 2>/dev/null | cut -d: -f1 | head -1 || true)
    end_line=$(grep -nxF "$end" "$path" 2>/dev/null | cut -d: -f1 | head -1 || true)
    if [ "$begin_count" -ne "$end_count" ] || [ "$begin_count" -gt 1 ] \
       || { [ "$begin_count" -eq 1 ] && [ "${begin_line:-0}" -ge "${end_line:-0}" ]; }; then
      rm -f "$content_file"
      fail "$path has a malformed Storyhook sentinel block; refusing to rewrite user instructions."
    fi
    if [ "$begin_count" -eq 1 ]; then
      action="replaced"
    elif cmp -s "$path" "$content_file"; then
      action="unchanged"
    else
      action="appended"
    fi
  else
    action="created"
  fi

  if [ -n "$DRY_RUN" ]; then
    rm -f "$content_file"
    jq -n --arg path "$path" --arg action "$action" --arg verb "$verb" '
      # $action is the PAST-tense form the real run reports (created/replaced/
      # appended) — kept as one vocabulary across both modes rather than a
      # second set of verbs, so a caller comparing a dry-run preview against
      # the real result is comparing the same word. Only this one display
      # string needs the present tense, so it maps locally instead.
      ({created:"create", replaced:"replace", appended:"append", unchanged:"leave unchanged"}[$action]) as $action_verb
      | {ok:true, dry_run:true, path:$path, action:$action,
         display:("[story] DRY RUN " + $verb + ": would " + $action_verb + " the Storyhook instructions in `" + $path + "`.")}'
    return 0
  fi

  case "$action" in
    replaced)
      # A plain bash line-reader, not sed/awk: it never has to know the
      # block's own contents in advance (sed's -e would need the WHOLE
      # replacement pre-escaped into its own script text), and it edits
      # nothing outside the sentinel pair — everything else in the file
      # passes through byte-for-byte.
      local tmp in_block=0 line
      tmp=$(mktemp "${path}.XXXXXX") || fail "could not create a scratch file next to $path."
      while IFS= read -r line || [ -n "$line" ]; do
        if [ "$line" = "$begin" ]; then
          printf '%s\n' "$begin"
          cat "$content_file"
          printf '%s\n' "$end"
          in_block=1
          continue
        fi
        if [ "$in_block" = 1 ]; then
          [ "$line" = "$end" ] && in_block=0
          continue
        fi
        printf '%s\n' "$line"
      done <"$path" >"$tmp"
      mv "$tmp" "$path"
      ;;
    appended)
      {
        [ -s "$path" ] && printf '\n'
        printf '%s\n' "$begin"
        cat "$content_file"
        printf '%s\n' "$end"
      } >>"$path"
      ;;
    created)
      {
        printf '%s\n' "$begin"
        cat "$content_file"
        printf '%s\n' "$end"
      } >"$path"
      ;;
    unchanged) : ;;
  esac
  rm -f "$content_file"

  jq -n --arg path "$path" --arg action "$action" --arg verb "$verb" '
    {ok:true, path:$path, action:$action,
     display:("[story] " + $verb + ": " + $action + " the Storyhook instructions in `" + $path + "`.")}'
}

cmd_scaffold_agents_md() { _cmd_scaffold_instructions agents-md AGENTS.md "$@"; }
cmd_scaffold_claude_md() { _cmd_scaffold_instructions claude-md CLAUDE.md "$@"; }

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
    [ -n "${TMUX:-}" ] || fail "story capture requires tmux — run $AGENT_LABEL inside a tmux session."
  fi
  local id="${1:-}"
  [ -n "$id" ] || fail "usage: story.sh capture <story-id>"
  shift
  [ "$#" -eq 0 ] || fail "usage: story.sh capture <story-id>"
  valid_story_id "$id" || fail "story id must be alphanumeric (hyphens/underscores allowed) (got: $id)."

  # capture makes no git calls at all — it looks for a tmux window — but it
  # still resolves the checkout first (best-effort) so the `story_cli show`
  # call below is disambiguated to the right project via the PROJECT_SLUG
  # resolve_checkout pins (SH-120).
  #
  # Tolerant throughout, on purpose. capture's job is to find a window, so a
  # store that cannot answer leaves the id as typed and lets the "no live
  # tmux window" message below do the reporting — rather than turning a
  # capture into a project lookup failure. The one read it still pays for is
  # the canonical id: the window it is hunting was named by `dispatch` from
  # the CANONICAL id, so `story.sh capture 5` would otherwise look for a
  # window no dispatch has ever created (SH-118).
  local wname
  if command -v "$STORY" >/dev/null 2>&1; then
    resolve_checkout || true
    id=$(canonical_story_id "$(story_cli show "$id" --json 2>/dev/null || printf '')" "$id")
  fi
  wname=$(resolve_wname "$id")

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

  local pane transcript
  pane=$(pane_for_window "$wname")
  [ -n "$pane" ] || fail "no live tmux window named \`$wname\` — dispatch it first with \`/story do $id\`."
  transcript=$(capture_pane_transcript "$pane") \
    || fail "failed to capture pane \`$pane\` (window \`$wname\`)."

  jq -n --arg id "$id" --arg wname "$wname" --arg pane "$pane" --arg tx "$transcript" '
    {
      ok: true, id: $id, window_name: $wname, pane: $pane, transcript: $tx,
      display: ("[story] capture " + $wname + " (" + $pane + ") — recent rendered rows:\n\n" + $tx)
    }'
}

# cmd_notify <story-id> <message> — resume the exact dispatched agent after
# centralized verification returns its PR for repair (SH-521).
cmd_notify() {
  local id="${1:-}" message="${2:-}"
  [ -n "$id" ] && [ -n "$message" ] && [ "$#" -eq 2 ] \
    || fail "usage: story.sh notify <story-id> <message>"
  valid_story_id "$id" \
    || fail "story id must be alphanumeric (hyphens/underscores allowed) (got: $id)."

  local wname pane buffer provider
  wname=$(resolve_wname "$id")
  pane=$(pane_for_window "$wname") || pane=""
  [ -n "$pane" ] \
    || refuse "pane-unavailable" "no live tmux window named \`$wname\`; verification diagnostics remain on $id, which has been blocked for manual recovery."
  provider=$(tmux show-options -w -v -t "$pane" @storyhook-agent 2>/dev/null || printf '')
  case "$provider" in
  claude | codex) configure_agent "$provider" ;;
  *) refuse "pane-provider-unknown" "tmux window \`$wname\` has no valid StoryHook provider identity; refusing to type into an unverified pane." ;;
  esac
  pane_runs "$pane" \
    || refuse "pane-changed" "tmux window \`$wname\` no longer runs the dispatched $AGENT_LABEL process; refusing to type into an unrelated pane."

  buffer="story-verify-$id"
  paste_prompt "$pane" "$message" "$buffer" \
    || refuse "delivery-failed" "could not paste the verification remediation into pane \`$pane\`."
  tmux send-keys -t "$pane" Enter 2>/dev/null \
    || refuse "delivery-failed" "the remediation reached pane \`$pane\`, but tmux refused the submit key."
  jq -n --arg id "$id" --arg window "$wname" --arg pane "$pane" \
    '{ok:true, id:$id, window_name:$window, pane:$pane,
      display:("[story] notified " + $id + " in window `" + $window + "` (" + $pane + ").")}'
}

# _project_integrity — run the CLI's own `story doctor` tolerantly.
#
# It exits 5 with an AppError::Integrity whenever it finds ANYTHING. So a
# non-zero exit here is an ordinary finding, not a failure of this probe, and
# must never abort it.
#
# Reads `.result` and `.error` only, and deliberately keeps doing so: since
# SH-244 the error envelope also carries `.findings[]` as structured data, but
# this probe wants one summary line for a human, which is exactly what `.error`
# already is — the findings' own sentences joined.
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
    [ -n "${TMUX:-}" ] || fail "story doctor requires tmux — run $AGENT_LABEL inside a tmux session."
    [ -n "${TMUX_PANE:-}" ] || fail "story doctor requires \$TMUX_PANE — run $AGENT_LABEL inside a tmux pane."
  fi

  local doctor_bin="${DOCTOR_LAUNCH_TPL%% *}"
  command -v "$doctor_bin" >/dev/null 2>&1 \
    || fail "launch binary '$doctor_bin' not found on PATH (set STORY_DOCTOR_LAUNCH_CMD)."

  if [ -n "$DRY_RUN" ]; then
    jq -n --arg launch "$DOCTOR_LAUNCH_TPL" --arg wname "$DOCTOR_WINDOW_NAME" \
      --arg agent "$AGENT" --arg agent_label "$AGENT_LABEL" --arg plan_key "$CODEX_PLAN_KEY" '
      {
        ok: true, dry_run: true, agent: $agent, agent_label: $agent_label, window_name: $wname,
        commands: [
          "story doctor --json",
          ("tmux new-window -d -n " + $wname + " -P -F #{pane_id}"),
          ("tmux send-keys -t <pane> -l " + $launch),
          "tmux send-keys -t <pane> Enter",
          (if $agent == "codex" then ("tmux send-keys -t <pane> " + $plan_key + " # if Plan footer is absent") else empty end),
          "printf %s <multi-line probe> | tmux load-buffer -b story-doctor -",
          "tmux paste-buffer -p -d -b story-doctor -t <pane>",
          "tmux capture-pane -p -t <pane>",
          "tmux kill-window -t <window>"
        ],
        display: ("[story] DRY RUN doctor: would run `story doctor` for project integrity, then spin up "
                  + $agent_label + " with `" + $launch + "` in window " + $wname + ", check readiness and Plan mode, paste a multi-line probe "
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

  # This probe launches DOCTOR_LAUNCH_TPL, which need not be LAUNCH_TPL — so the
  # binary pane_runs recognises by identity has to follow it, or doctor would
  # self-test the dispatch launcher's install while running a different one.
  READY_LAUNCH_BIN="$doctor_bin"

  local readiness_confirmed=false plan_mode_confirmed=false tier="none" tail_evidence
  if wait_ready "$pane" "$DOCTOR_LAUNCH_TPL"; then
    readiness_confirmed=true
    if ensure_provider_plan_mode "$pane"; then
      plan_mode_confirmed=true
    fi
  fi
  tier="$WAIT_READY_TIER"
  # SH-239: which of pane_runs' rules answered, and the identity behind it.
  # `pattern` means the occupant's NAME was recognised; anything else means a
  # name-only gate would have refused this build, which is the drift doctor
  # exists to surface BEFORE a dispatch discovers it.
  local occupant occupant_rule launch_resolved
  occupant="$WAIT_READY_COMMAND"
  occupant_rule="$PANE_RUNS_RULE"
  launch_resolved="$(resolve_exe "$doctor_bin" || printf '')"
  tail_evidence=$(pane_tail "$pane")

  # Live multi-line paste probe: paste a 3-line marker through the real
  # bracketed-paste path WITHOUT submitting, then read the input box back.
  # If delivery works, all three lines land as ONE block and the FIRST sits on
  # the `❯` row; had it split at a newline, the first would have submitted and
  # only the last would remain. Diagnostic only — never presses Enter.
  local probe_ran=false probe_first_held=false probe_seen=0 probe_total=3
  if [ "$readiness_confirmed" = true ] && [ "$plan_mode_confirmed" = true ]; then
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
    display="[story] doctor: $AGENT_LABEL readiness OK via the '$tier' tier."
    if [ "$occupant_rule" != "pattern" ]; then
      display="$display Occupant name \`$occupant\` does NOT match STORY_READY_PROCESS_PATTERN (\`$READY_PROCESS_PATTERN\`); it was recognised as the launch binary itself ($occupant_rule), which resolves to \`$launch_resolved\`. Dispatch works — but a name-only gate would refuse this build, so leave STORY_READY_PROCESS_PATTERN alone rather than pinning it to \`$occupant\`, which changes on every update."
    fi
  else
    display="[story] doctor: $AGENT_LABEL readiness NOT confirmed within the poll budget — the readiness marker may have drifted. See pane_tail."
    if [ -n "$occupant" ]; then
      display="$display The pane's occupant was \`$occupant\` and the launch binary resolves to \`${launch_resolved:-<unresolved>}\`."
    fi
  fi
  if [ "$plan_mode_confirmed" = true ]; then
    display="$display Plan mode: confirmed."
  else
    display="$display Plan mode: NOT confirmed (${PLAN_MODE_REASON:-readiness was not established})."
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
    --argjson plan "$plan_mode_confirmed" --arg agent "$AGENT" --arg agent_label "$AGENT_LABEL" \
    --arg tail "$tail_evidence" --arg display "$display" \
    --argjson probe_ran "$probe_ran" --argjson probe_first "$probe_first_held" \
    --argjson probe_seen "$probe_seen" --argjson probe_total "$probe_total" \
    --argjson integrity_ok "$_INTEGRITY_OK" --arg integrity "$integrity_summary" \
    --arg occupant "$occupant" --arg rule "$occupant_rule" \
    --arg launch_bin "$doctor_bin" --arg launch_resolved "$launch_resolved" \
    --arg pattern "$READY_PROCESS_PATTERN" '
    {
      ok: true,
      agent: $agent,
      agent_label: $agent_label,
      readiness_confirmed: $ready,
      plan_mode_confirmed: $plan,
      matched_tier: $tier,
      occupant: {
        name: $occupant,
        match_rule: $rule,
        name_pattern: $pattern,
        matches_name_pattern: ($rule == "pattern"),
        launch_binary: $launch_bin,
        launch_binary_resolved: $launch_resolved
      },
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

# _story_worktree_status <path> <caller-toplevel> — removable|current|locked|
# dirty|missing.
#
# <caller-toplevel> is the repository root the CALLER was standing in, captured
# before this verb changed directory. It is passed rather than measured here ON
# PURPOSE: the question `current` asks is literally "is the caller standing
# inside the worktree it is asking us to delete", which is a question about
# where the user is, not about where this script has since gone. Measuring it
# here would answer for the wrong directory, and `git worktree remove` would
# then succeed while the user's shell sat in a deleted one.
#
# A locked worktree is never removed and never unlocked — Claude Code's own
# worktree feature locks the ones it creates, and reclaiming those is not this
# verb's business.
_story_worktree_status() {
  local target="$1" cur="$2" locked
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

# _close_story_window <window-name> — close every matching window and prove
# absence. Returns 0 when at least one was removed, 1 when already absent, and
# 2 when a kill failed or the postcondition could not be established.
#
# pane_for_window / display-message / kill-window each swallow their own
# errors, so the only question worth asking is whether a pane was found. The
# window-id lookup is preferred over killing the pane directly because a window
# holding several panes must die whole; the pane fallback is what runs when a
# tmux too old (or a fixture too thin) does not answer `#{window_id}`.
#
# Extracted from cmd_reap for SH-484: `unclaim` and `reset` close the same
# window by the same rule, and three copies of a teardown step is the shape
# this project has paid for repeatedly (SH-136, SH-198, SH-258, SH-260/276,
# SH-360). What differs between the three verbs is WHEN they call it and
# whether they are allowed to, never how the window is found.
_close_story_window() {
  local wname="$1" pane window found=false attempts=0
  while :; do
    pane=$(pane_for_window "$wname") || return 2
    [ -n "$pane" ] || break
    found=true
    attempts=$((attempts + 1))
    [ "$attempts" -le 256 ] || return 2
    window=$(tmux display-message -p -t "$pane" '#{window_id}' 2>/dev/null || printf '')
    if [ -n "$window" ]; then
      tmux kill-window -t "$window" 2>/dev/null || return 2
    else
      tmux kill-window -t "$pane" 2>/dev/null || return 2
    fi
  done
  [ "$found" = true ] && return 0
  return 1
}

# _complete_prepare <id> — shared by plan and execute. Resolves the story,
# the target paths, and the classification of each, into caller-visible
# globals. Freshens origin/<default> exactly ONCE here so both verbs judge
# merged-ness against ground truth rather than a local <default> that a
# worktree-driven repo may never pull (a read, not a mutation — it runs even
# under dry-run, as issue.sh's cmd_complete_execute does).
_complete_prepare() {
  local id="$1"
  # FIRST, before this verb resolves or enters anything: the repository root
  # the CALLER is standing in. It is the sole input to _story_worktree_status's
  # `current` branch, and the only moment it can be read is before the working
  # directory moves.
  local caller_toplevel
  caller_toplevel=$(git rev-parse --show-toplevel 2>/dev/null || printf '')

  valid_story_id "$id" || fail "story id must be alphanumeric (hyphens/underscores allowed) (got: $id)."

  require_story
  # The same lookup dispatch performs, for the same reason: a worktree created
  # under one rule and cleaned up under another is SH-166 verbatim.
  resolve_checkout || fail "$CHECKOUT_ERROR"
  enter_checkout
  CMP_DIR="$PROJECT_ROOT"

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

  local wt_container wname
  wname=$(resolve_wname "$id")
  CMP_DEFAULT=$(default_branch)
  freshen_base_ref "$CMP_DEFAULT"

  wt_container="${WORKTREE_IGNORE_PATH%/}"

  CMP_WNAME="$wname"
  CMP_WT_PATH="$CMP_DIR/$wt_container/$CMP_WNAME"
  CMP_WT_BRANCH="worktree-$CMP_WNAME"
  CMP_WT_STATUS=$(_story_worktree_status "$CMP_WT_PATH" "$caller_toplevel")

  # Window classification (SH-308) — read-only, safe under `plan`. `complete`
  # never touched tmux before this: it could remove a worktree out from under a
  # live window, leaving a running Claude session pointed at a deleted
  # directory. `pane_for_window` is the SAME name-equality lookup `reap` and
  # `capture` already use (lib/session.sh) — CMP_WNAME is the one name dispatch,
  # complete and reap all agree on (resolve_wname).
  #
  # `self` is distinct from `open` on purpose: `complete` renders an answer a
  # human standing in THAT window reads, so killing it would destroy the
  # session before it could show the result. `reap` exists for exactly the
  # self-directed case and kills its own window LAST for that reason (see its
  # header comment) — `complete` defers to it rather than re-deriving the same
  # judgment call here.
  CMP_WINDOW_STATUS="none"
  CMP_WINDOW_PANE=""
  if [ -n "${TMUX:-}" ]; then
    CMP_WINDOW_PANE=$(pane_for_window "$CMP_WNAME") || CMP_WINDOW_PANE=""
    if [ -n "$CMP_WINDOW_PANE" ]; then
      if [ -n "${TMUX_PANE:-}" ] && [ "$CMP_WINDOW_PANE" = "$TMUX_PANE" ]; then
        CMP_WINDOW_STATUS="self"
      else
        CMP_WINDOW_STATUS="open"
      fi
    fi
  fi

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

  # How many of this branch's commits exist on NO remote (SH-484). A DIFFERENT
  # question from CMP_BR_STATUS, which is about merged-ness: a branch pushed to
  # `origin/worktree-<id>` and never merged anywhere scores 0 here and
  # `unmerged` above, and only this number answers "would deleting it destroy
  # something nothing else has". `reset` is the only verb that takes a verdict
  # on it — `complete` and `reap` refuse `unmerged` outright, which is strictly
  # stronger — but it is derived HERE with the other classifications so one
  # place decides what is true and each verb decides only what to do about it.
  #
  # A repository with no remotes at all counts every commit as unpushed, which
  # is the conservative reading and the correct one: there is nowhere else for
  # that work to be.
  CMP_BR_UNPUSHED=0
  if [ "$CMP_BR_STATUS" != "missing" ]; then
    CMP_BR_UNPUSHED=$(git rev-list --count "refs/heads/$CMP_WT_BRANCH" --not --remotes 2>/dev/null || printf '0')
  fi

  # Nothing at all under the project's checkout is REPORTED, never
  # reconstructed. There is deliberately no fallback to the caller's own
  # repository for a worktree dispatched before `project link checkout` recorded
  # a directory: it would only help when the caller happens to still be standing
  # in the old one, it would put a second CWD-dependent path through the very
  # verbs SH-120 takes off CWD, and this codebase's idiom for "recorded state
  # and reality disagree" is to say so.
  CMP_NOTE=""
  if [ "$CMP_WT_STATUS" = "missing" ] && [ "$CMP_BR_STATUS" = "missing" ]; then
    CMP_NOTE=" Nothing named \`$CMP_WNAME\` exists under $CMP_DIR — if $id was dispatched before \`project link checkout\` recorded that directory, its worktree is elsewhere and is not cleaned up here."
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
  # An `open` window is actionable ONLY when the worktree default `execute`
  # (no `--force`) will actually remove — i.e. `removable`. A window sitting
  # on a `dirty`/`current`/`locked` worktree is left alone by DEFAULT execute
  # too (see cmd_complete_execute), so counting it here would claim an action
  # this plan's own default run would not take; `--force` is an execute-only
  # override the plan does not assume. `self` is never counted — `complete`
  # cannot act on the window rendering its own answer; see the classification
  # comment above.
  if [ "$CMP_WINDOW_STATUS" = "open" ] && [ "$CMP_WT_STATUS" = "removable" ]; then
    CMP_ACTIONS=$((CMP_ACTIONS + 1))
  fi
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
    --arg wname "$CMP_WNAME" --arg wstatus "$CMP_WINDOW_STATUS" \
    --argjson actions "$CMP_ACTIONS" \
    --arg note "$CMP_NOTE" '
    {
      ok: true, id: $id, title: $title, state: $state, superstate: $super,
      default_branch: $default,
      plan: {
        close: (if $close then { to: $done_state } else null end),
        worktree: { path: $wtpath, status: $wtstatus },
        branch:   { name: $branch, status: $brstatus },
        window:   { name: $wname, status: $wstatus }
      },
      actions_count: $actions,
      display: (
        "[story] complete " + $id + " — plan (" + ($actions|tostring) + " action(s)):\n"
        + "  story:    " + $state + " (" + $super + ")"
          + (if $close then " -> would close as `" + $done_state + "`" else " -> already closed, nothing to do" end) + "\n"
        + "  window:   `" + $wname + "` [" + $wstatus + "]"
          + (if $wstatus == "open" and $wtstatus == "removable" then " -> would close before removing the worktree"
             elif $wstatus == "open" then " -> would close only if `--force` removes the worktree below"
             elif $wstatus == "self" then " -> PRESERVED (this is the window you are asking from — use `reap` for that)"
             else " -> nothing to close" end) + "\n"
        + "  worktree: " + $wtpath + " [" + $wtstatus + "]"
          + (if $wtstatus == "removable" then " -> would remove"
             elif $wtstatus == "missing" then " -> nothing to remove"
             elif ($wtstatus == "dirty" or $wtstatus == "current") then " -> PRESERVED (retry with `--force` to override)"
             else " -> PRESERVED" end) + "\n"
        + "  branch:   " + $branch + " [" + $brstatus + "]"
          + (if $brstatus == "deletable" then " -> would delete (merged into " + $default + ")"
             elif $brstatus == "missing" then " -> nothing to delete"
             elif $brstatus == "unmerged" then " -> PRESERVED (not merged into " + $default + ")"
             else " -> PRESERVED (" + $brstatus + ")" end)
        + (if $note == "" then "" else "\n " + $note end)
      )
    }'
}

cmd_complete_execute() {
  local id="${1:-}"
  [ -n "$id" ] || fail "usage: story.sh complete execute <story-id> [--no-close] [--no-clean] [--force]"
  shift
  local no_close="" no_clean="" force=""
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --no-close) no_close=1; shift ;;
      --no-clean) no_clean=1; shift ;;
      --force)    force=1; shift ;;
      *) fail "unknown argument \`$1\` — usage: story.sh complete execute <story-id> [--no-close] [--no-clean] [--force]" ;;
    esac
  done
  _complete_prepare "$id"
  id="$CMP_ID"

  local -a removed_wt=() removed_bl=() failed=() skipped=() commands=()
  local closed=false close_note="" closed_window=false

  # Close the story FIRST. A state-transition failure remains a note because
  # independent resource cleanup is still useful; every attempted resource
  # cleanup below, however, must prove its postcondition before `ok:true`.
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
    # Decide whether the worktree will actually be removed BEFORE touching
    # either it or its window: `--force` overrides `dirty`/`current` (a `git
    # worktree remove --force`, the same escalation dispatch's own rollback
    # already uses at four sites in this file), but never `locked` — Claude
    # Code's own worktree feature locks the ones it creates, and reclaiming
    # those has never been this verb's business, forced or not.
    local wt_action="skip"
    case "$CMP_WT_STATUS" in
      removable) wt_action="remove" ;;
      dirty|current) [ -n "$force" ] && wt_action="remove" ;;
      missing) wt_action="missing" ;;
      *) wt_action="skip" ;;  # locked, or anything future _story_worktree_status adds
    esac

    # Close an `open` window BEFORE removing the worktree it belongs to
    # (SH-308) — a bystander window must never be left running inside a
    # directory mid-deletion. Reuses the SAME pane `_complete_prepare` already
    # resolved, so this can't disagree with what `plan` just reported. `self`
    # is excluded here too (never killed by `complete`; see `reap`), and no
    # window is closed when the worktree ends up NOT being removed (`skip` —
    # e.g. `dirty` without `--force`), since nothing is actually being
    # reclaimed out from under it.
    if [ "$wt_action" = "remove" ] && [ "$CMP_WINDOW_STATUS" = "open" ]; then
      if [ -n "$DRY_RUN" ]; then
        commands+=("tmux kill-window -t <window of $CMP_WNAME>")
      else
        if _close_story_window "$CMP_WNAME"; then
          closed_window=true
        else
          local window_rc=$?
          if [ "$window_rc" -ne 1 ]; then
            failed+=("window:$CMP_WNAME(unverifiable)")
            wt_action="skip"
          fi
        fi
      fi
      [ -z "$DRY_RUN" ] || closed_window=true
    elif [ "$CMP_WINDOW_STATUS" = "self" ]; then
      # Named explicitly, whether or not the worktree ends up touched: a
      # human standing in this window reading `display` is the one fact
      # `complete` can never destroy out from under itself. `reap` exists for
      # exactly this self-directed case and kills its own window LAST.
      skipped+=("window:$CMP_WNAME(self)")
    fi

    case "$wt_action" in
      remove)
        if [ -n "$DRY_RUN" ]; then
          # NOT `$([ -n "$force" ] && printf ...)`: under `set -e`, a failing
          # LAST command inside a `$(...)` used in an assignment/append kills
          # this whole script silently (the classic errexit-in-command-
          # substitution gotcha) — `[ -n "$force" ]` failing when unset would
          # do exactly that. Computed as a plain `if` beforehand instead.
          local force_prefix=""
          if [ -n "$force" ]; then force_prefix="--force "; fi
          commands+=("git worktree remove ${force_prefix}$CMP_WT_PATH" "git worktree prune")
          removed_wt+=("$CMP_WT_PATH")
        else
          local -a rm_args=(worktree remove)
          [ -n "$force" ] && rm_args+=(--force)
          rm_args+=("$CMP_WT_PATH")
          if git "${rm_args[@]}" >/dev/null 2>&1; then
            git worktree prune >/dev/null 2>&1 || true
            if registered_worktree_branch "$CMP_WT_PATH" >/dev/null 2>&1 \
               || [ -e "$CMP_WT_PATH" ]; then
              failed+=("worktree:$CMP_WT_PATH(postcondition)")
            else
              removed_wt+=("$CMP_WT_PATH")
            fi
          else
            failed+=("worktree:$CMP_WT_PATH")
          fi
        fi ;;
      missing) : ;;  # already gone — not an error.
      skip) skipped+=("worktree:$CMP_WT_PATH($CMP_WT_STATUS)") ;;
    esac

    case "$CMP_BR_STATUS" in
      deletable)
        if [ -n "$DRY_RUN" ]; then
          commands+=("git branch -d $CMP_WT_BRANCH")
          removed_bl+=("$CMP_WT_BRANCH")
        elif delete_merged_local_branch "$CMP_WT_BRANCH" "$CMP_DEFAULT"; then
          if local_branch_exists "$CMP_WT_BRANCH"; then
            failed+=("branch:$CMP_WT_BRANCH(postcondition)")
          else
            removed_bl+=("$CMP_WT_BRANCH")
          fi
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

  local complete_ok=true
  [ "$(printf '%s' "$fail_json" | jq 'length')" -eq 0 ] || complete_ok=false
  jq -n --argjson ok "$complete_ok" --arg id "$id" --argjson rwt "$rwt" --argjson rbl "$rbl" \
        --argjson failed "$fail_json" --argjson skipped "$skip_json" \
        --argjson cmds "$cmds_json" --argjson closed "$closed" \
        --arg done_state "$CMP_DONE_STATE" --arg note "$close_note" \
        --arg wtnote "$CMP_NOTE" --argjson closed_window "$closed_window" \
        --argjson forced "$([ -n "$force" ] && echo true || echo false)" \
        --argjson dry "$([ -n "$DRY_RUN" ] && echo true || echo false)" '
    {
      ok: $ok, id: $id, closed: $closed,
      removed: { worktrees: $rwt, branches: $rbl, window: $closed_window }
    }
    + (if $closed then {closed_as: $done_state} else {} end)
    + (if $dry then {dry_run:true, commands:$cmds} else {} end)
    + (if $forced then {forced:true} else {} end)
    + (if ($failed|length) > 0 then {failed:$failed} else {} end)
    + (if ($skipped|length) > 0 then {skipped:$skipped} else {} end)
    + { display: (
        "[story] " + (if $dry then "DRY RUN for " else "" end) + "complete " + $id + ": "
        + (if $closed then (if $dry then "would close as `" else "closed as `" end) + $done_state + "`, " else "" end)
        + (if $closed_window then (if $dry then "would close its tmux window, " else "closed its tmux window, " end) else "" end)
        + (if $dry then "would remove " else "removed " end) + ($rwt|length|tostring) + " worktree(s), "
        + ($rbl|length|tostring) + " branch(es)."
        + (if $forced then " (--force)" else "" end)
        + (if ($failed|length) > 0 then " Could not: " + ($failed|join(", ")) + "." else "" end)
        + (if ($skipped|length) > 0 then " Preserved: " + ($skipped|join(", ")) + "." else "" end)
        + $note + $wtnote
      ) }'
  [ "$complete_ok" = true ]
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

# leased_story_windows <lease-json> <story-id> — publish the unique window ids
# whose names exactly equal the completed story id on the leased tmux server.
# A missing socket proves absence. An existing but unreadable socket fails loud
# because cleanup cannot prove that every matching window is gone.
LEASE_TMUX_WINDOWS=""
leased_story_windows() {
  local lease="$1" story="$2" socket listing window_id window_name seen=""
  socket=$(printf '%s' "$lease" | jq -r '.tmux.socket_path')
  LEASE_TMUX_WINDOWS=""
  if [ ! -e "$socket" ]; then
    return 0
  fi
  if ! listing=$(tmux -S "$socket" list-windows -a -F '#{window_id} #{window_name}' 2>/dev/null); then
    [ ! -e "$socket" ] && return 0
    return 1
  fi
  [ -z "$listing" ] && return 0
  while read -r window_id window_name; do
    [[ "$window_id" =~ ^@[0-9]+$ ]] || return 1
    [ "$window_name" = "$story" ] || continue
    case "$seen" in *"|$window_id|"*) continue ;; esac
    seen="${seen}|$window_id|"
    LEASE_TMUX_WINDOWS="${LEASE_TMUX_WINDOWS:+$LEASE_TMUX_WINDOWS$'\n'}$window_id"
  done <<< "$listing"
  return 0
}

# cmd_unclaim_leased <story-id> <lease-json> — release an engine-owned claim
# and close only exact-name windows on the creation-time tmux server. A retry
# after the state release already succeeded is valid: cleanup is independently
# idempotent, while an ordinary unleased conflict keeps the safety refusal.
cmd_unclaim_leased() {
  local typed_id="$1" lease="$2"
  if ! printf '%s' "$lease" | jq -e --argjson version "$CLEANUP_LEASE_VERSION" '
      type == "object" and .version == $version
      and (.project_slug | type == "string" and length > 0)
      and (.story_id | type == "string" and length > 0)
      and (.repository_path | type == "string" and startswith("/"))
      and (.worktree_path | type == "string" and startswith("/"))
      and (.branch | type == "string" and length > 0)
      and (.tmux | type == "object")
      and (.tmux.socket_path | type == "string" and startswith("/"))' >/dev/null 2>&1; then
    refuse "invalid-cleanup-lease" "story.sh unclaim: the cleanup lease is malformed or uses an unsupported version."
  fi
  require_story
  resolve_project || fail "$CHECKOUT_ERROR"
  local lease_project lease_story
  lease_project=$(printf '%s' "$lease" | jq -r '.project_slug')
  lease_story=$(printf '%s' "$lease" | jq -r '.story_id')
  [ "$lease_project" = "$PROJECT_SLUG" ] \
    || refuse "cleanup-lease-project-mismatch" "story.sh unclaim: lease project `$lease_project` does not match selected project `$PROJECT_SLUG`."
  [ "$typed_id" = "$lease_story" ] \
    || refuse "cleanup-lease-story-mismatch" "story.sh unclaim: lease story `$lease_story` does not match requested story `$typed_id`."

  _release_story unclaim "$lease_story"
  local unclaimed=false conflict=""
  case "$REL_RESULT" in
    ok) unclaimed=true ;;
    conflict) conflict="$REL_ACTUAL" ;;
    *) fail "story unclaim $lease_story failed: $REL_ERROR" ;;
  esac

  leased_story_windows "$lease" "$lease_story" \
    || refuse "cleanup-lease-tmux-unverifiable" "story.sh unclaim: the leased tmux server exists but its story windows cannot be enumerated."
  local initial="$LEASE_TMUX_WINDOWS" removed=false failure=""
  if [ -n "$initial" ]; then
    local socket window_id
    socket=$(printf '%s' "$lease" | jq -r '.tmux.socket_path')
    while IFS= read -r window_id; do
      if tmux -S "$socket" kill-window -t "$window_id" >/dev/null 2>&1; then
        removed=true
      else
        failure="${failure:+$failure; }tmux refused to kill $window_id"
      fi
    done <<< "$initial"
  fi
  local absent=false
  if leased_story_windows "$lease" "$lease_story"; then
    [ -n "$LEASE_TMUX_WINDOWS" ] || absent=true
  else
    failure="${failure:+$failure; }tmux postcondition is unverifiable"
  fi
  [ "$absent" = true ] \
    || failure="${failure:+$failure; }story tmux windows remain: $(printf '%s' "$LEASE_TMUX_WINDOWS" | tr '\n' ' ')"

  local ok=false display
  if [ "$absent" = true ] && [ -z "$failure" ]; then
    ok=true
    display="[story] unclaim $lease_story: exact story windows are absent on the leased tmux server."
  else
    display="[story] unclaim $lease_story released its claim but cleanup failed: ${failure:-the tmux absence postcondition is false}."
  fi
  jq -n --argjson ok "$ok" --arg id "$lease_story" --argjson lease "$lease" \
    --argjson unclaimed "$unclaimed" --arg conflict "$conflict" \
    --arg from "$REL_FROM" --arg to "$REL_TO" --arg fallback "$REL_FALLBACK" \
    --argjson removed "$removed" --argjson absent "$absent" --arg display "$display" '
      {ok:$ok,id:$id,unclaimed:$unclaimed,cleanup:{lease:$lease,
       removed:{tmux:$removed},postconditions:{tmux_story_windows_absent:$absent}},
       display:$display}
      + (if $conflict == "" then {} else {unclaim_conflict:$conflict} end)
      + (if $unclaimed then {unclaimed_from:$from,restored_to:$to} else {} end)
      + (if $fallback == "" then {} else {restore_fallback:$fallback} end)'
  [ "$ok" = true ]
}

# branch_worktree_path <branch> — print the registered worktree holding the
# exact local branch, if any. Porcelain records are parsed without splitting
# paths on spaces.
branch_worktree_path() {
  local wanted="$1" line path="" branch=""
  while IFS= read -r line; do
    case "$line" in
      worktree\ *)
        if [ "$branch" = "$wanted" ]; then printf '%s' "$path"; return 0; fi
        path="${line#worktree }"; branch="" ;;
      branch\ refs/heads/*) branch="${line#branch refs/heads/}" ;;
      '')
        if [ "$branch" = "$wanted" ]; then printf '%s' "$path"; return 0; fi
        path=""; branch="" ;;
    esac
  done < <(git worktree list --porcelain 2>/dev/null)
  [ "$branch" != "$wanted" ] || { printf '%s' "$path"; return 0; }
  return 1
}

# leased_reap_receipt <lease-json> <ok> <worktree-registration-absent>
# <worktree-path-absent> <branch-absent> <tmux-absent> <display> — emit the
# typed protocol receipt the daemon verifies field for field.
leased_reap_receipt() {
  local lease="$1" ok="$2" registration_absent="$3" path_absent="$4"
  local branch_absent="$5" tmux_absent="$6" removed_worktree="$7"
  local removed_branch="$8" removed_tmux="$9" display="${10}"
  jq -n --argjson ok "$ok" --argjson version "$CLEANUP_LEASE_VERSION" \
    --arg story "$(printf '%s' "$lease" | jq -r '.story_id')" \
    --argjson lease "$lease" --argjson registration "$registration_absent" \
    --argjson path "$path_absent" --argjson branch "$branch_absent" \
    --argjson tmux "$tmux_absent" --argjson rwt "$removed_worktree" \
    --argjson rbr "$removed_branch" --argjson rtmux "$removed_tmux" \
    --arg display "$display" \
    '{ok:$ok, receipt_version:$version, story_id:$story, lease:$lease,
      removed:{worktree:$rwt,branch:$rbr,tmux:$rtmux},
      postconditions:{worktree_registration_absent:$registration,
                      worktree_path_absent:$path,
                      branch_absent:$branch,
                      tmux_story_windows_absent:$tmux},
      display:$display}'
}

# cmd_reap_leased <typed-story-id> <lease-json> — exact-target cleanup for the
# centralized verifier or the dispatched worktree itself. No current checkout
# or ambient provider participates.
cmd_reap_leased() {
  local typed_id="$1" lease="$2"
  if ! printf '%s' "$lease" | jq -e --argjson version "$CLEANUP_LEASE_VERSION" '
      type == "object" and .version == $version
      and (.project_slug | type == "string" and length > 0)
      and (.story_id | type == "string" and length > 0)
      and (.repository_path | type == "string" and startswith("/"))
      and (.worktree_path | type == "string" and startswith("/"))
      and (.branch | type == "string" and length > 0)
      and (.tmux | type == "object")
      and (.tmux.socket_path | type == "string" and startswith("/"))' >/dev/null 2>&1; then
    refuse "invalid-cleanup-lease" "story.sh reap: the cleanup lease is malformed or uses an unsupported version."
  fi

  local lease_project lease_story leased_repo leased_worktree leased_branch
  lease_project=$(printf '%s' "$lease" | jq -r '.project_slug')
  lease_story=$(printf '%s' "$lease" | jq -r '.story_id')
  leased_repo=$(printf '%s' "$lease" | jq -r '.repository_path')
  leased_worktree=$(printf '%s' "$lease" | jq -r '.worktree_path')
  leased_branch=$(printf '%s' "$lease" | jq -r '.branch')
  valid_story_id "$typed_id" || fail "story id must be alphanumeric (hyphens/underscores allowed) (got: $typed_id)."

  require_story
  resolve_project || fail "$CHECKOUT_ERROR"
  [ "$lease_project" = "$PROJECT_SLUG" ] \
    || refuse "cleanup-lease-project-mismatch" "story.sh reap: lease project \`$lease_project\` does not match selected project \`$PROJECT_SLUG\`."
  local show_json result canonical_id state super done_state
  show_json=$(story_cli show "$typed_id" --json 2>/dev/null) || true
  result=$(printf '%s' "$show_json" | jq -r '.result // ""' 2>/dev/null || printf '')
  [ "$result" = ok ] || fail "$(printf '%s' "$show_json" | jq -r --arg id "$typed_id" '.error // ("story `" + $id + "` not found")')"
  canonical_id=$(canonical_story_id "$show_json" "$typed_id")
  [ "$lease_story" = "$canonical_id" ] \
    || refuse "cleanup-lease-story-mismatch" "story.sh reap: lease story \`$lease_story\` does not match requested story \`$canonical_id\`."
  state=$(printf '%s' "$show_json" | jq -r '.story.story.state // ""')
  super=$(printf '%s' "$show_json" | jq -r '.story.story.superstate // ""')
  done_state=$(story_closed_state)
  [ "$super" = CLOSED ] \
    || refuse "not-closed" "story.sh reap: $canonical_id is not closed (state \`$state\`) -- refusing to reclaim a worktree for a story that isn't done."
  [ "$state" = "$done_state" ] \
    || refuse "not-completion-state" "story.sh reap: $canonical_id is in CLOSED state \`$state\`, not completion state \`$done_state\`."

  local repository_real root worktree_real registered_branch branch_holder
  repository_real=$(cd_resolve / "$leased_repo") \
    || refuse "cleanup-lease-repository-missing" "story.sh reap: leased repository \`$leased_repo\` is unreachable."
  [ "$repository_real" = "$leased_repo" ] \
    || refuse "cleanup-lease-repository-mismatch" "story.sh reap: leased repository path is not canonical (\`$leased_repo\` resolves to \`$repository_real\`)."
  root=$(repo_root "$leased_repo") \
    || refuse "cleanup-lease-repository-invalid" "story.sh reap: leased repository \`$leased_repo\` is not a Git worktree."
  [ "$root" = "$leased_repo" ] && ! is_linked_worktree "$leased_repo" \
    || refuse "cleanup-lease-repository-mismatch" "story.sh reap: leased repository \`$leased_repo\` is not its repository's main worktree."
  CDPATH= cd -- "$leased_repo" || refuse "cleanup-lease-repository-missing" "story.sh reap: cannot enter leased repository \`$leased_repo\`."
  git check-ref-format --branch "$leased_branch" >/dev/null 2>&1 \
    || refuse "cleanup-lease-branch-invalid" "story.sh reap: leased branch \`$leased_branch\` is not a valid local branch name."
  [ "$leased_worktree" != "$leased_repo" ] \
    || refuse "cleanup-lease-worktree-mismatch" "story.sh reap: leased worktree and repository paths are identical."
  if [ -e "$leased_worktree" ]; then
    worktree_real=$(cd_resolve / "$leased_worktree") \
      || refuse "cleanup-lease-worktree-unreachable" "story.sh reap: leased worktree \`$leased_worktree\` cannot be resolved."
    [ "$worktree_real" = "$leased_worktree" ] \
      || refuse "cleanup-lease-worktree-mismatch" "story.sh reap: leased worktree path is not canonical (\`$leased_worktree\` resolves to \`$worktree_real\`)."
    [ "$(repo_root "$leased_worktree" 2>/dev/null || printf '')" = "$leased_repo" ] \
      || refuse "cleanup-lease-worktree-mismatch" "story.sh reap: \`$leased_worktree\` does not belong to leased repository \`$leased_repo\`."
  fi
  if registered_branch=$(registered_worktree_branch "$leased_worktree"); then
    [ "$registered_branch" = "$leased_branch" ] \
      || refuse "cleanup-lease-worktree-mismatch" "story.sh reap: \`$leased_worktree\` is registered on \`$registered_branch\`, not leased branch \`$leased_branch\`."
  elif [ -e "$leased_worktree" ]; then
    refuse "cleanup-lease-worktree-unregistered" "story.sh reap: leased path \`$leased_worktree\` exists but is not a registered worktree."
  fi
  branch_holder=$(branch_worktree_path "$leased_branch" || printf '')
  [ -z "$branch_holder" ] || [ "$branch_holder" = "$leased_worktree" ] \
    || refuse "cleanup-lease-branch-reused" "story.sh reap: leased branch \`$leased_branch\` is checked out at unexpected path \`$branch_holder\`."

  local default wt_status branch_status
  default=$(default_branch)
  freshen_base_ref "$default"
  wt_status=$(_story_worktree_status "$leased_worktree" "")
  case "$wt_status" in
    dirty) refuse "dirty-worktree" "story.sh reap: $canonical_id's leased worktree ($leased_worktree) has uncommitted changes." ;;
    locked) refuse "locked-worktree" "story.sh reap: $canonical_id's leased worktree ($leased_worktree) is locked." ;;
  esac
  if ! local_branch_exists "$leased_branch"; then
    branch_status=missing
  elif is_protected_branch "$leased_branch"; then
    branch_status=protected
  elif branch_is_merged "$leased_branch" "$default"; then
    branch_status=deletable
  else
    branch_status=unmerged
  fi
  [ "$branch_status" != protected ] \
    || refuse "protected-branch" "story.sh reap: $canonical_id's leased branch ($leased_branch) is protected."
  [ "$branch_status" != unmerged ] \
    || refuse "unmerged-branch" "story.sh reap: $canonical_id's leased branch ($leased_branch) is not merged into \`$default\`."
  leased_story_windows "$lease" "$canonical_id" \
    || refuse "cleanup-lease-tmux-unverifiable" "story.sh reap: the leased tmux server exists but its story windows cannot be enumerated."
  local initial_tmux_windows="$LEASE_TMUX_WINDOWS"

  local reaped_wt=false reaped_br=false reaped_tmux=false failure=""
  if [ "$wt_status" != missing ]; then
    if git worktree remove "$leased_worktree" >/dev/null 2>&1; then
      git worktree prune >/dev/null 2>&1 || true
      reaped_wt=true
    else
      failure="git worktree remove refused for $leased_worktree"
    fi
  fi
  if [ "$branch_status" = deletable ]; then
    if delete_merged_local_branch "$leased_branch" "$default"; then
      reaped_br=true
    else
      failure="${failure:+$failure; }could not delete branch $leased_branch"
    fi
  fi
  if [ -n "$initial_tmux_windows" ]; then
    local socket window_id
    socket=$(printf '%s' "$lease" | jq -r '.tmux.socket_path')
    while IFS= read -r window_id; do
      if tmux -S "$socket" kill-window -t "$window_id" >/dev/null 2>&1; then
        reaped_tmux=true
      fi
    done <<< "$initial_tmux_windows"
  fi

  local registration_absent=false path_absent=false branch_absent=false tmux_absent=false
  registered_worktree_branch "$leased_worktree" >/dev/null 2>&1 || registration_absent=true
  [ -e "$leased_worktree" ] || path_absent=true
  local_branch_exists "$leased_branch" || branch_absent=true
  if leased_story_windows "$lease" "$canonical_id"; then
    if [ -z "$LEASE_TMUX_WINDOWS" ]; then
      tmux_absent=true
    else
      failure="${failure:+$failure; }story tmux windows remain: $(printf '%s' "$LEASE_TMUX_WINDOWS" | tr '\n' ' ')"
    fi
  else
    failure="${failure:+$failure; }tmux postcondition is unverifiable"
  fi
  local ok=false display
  if [ "$registration_absent" = true ] && [ "$path_absent" = true ] \
     && [ "$branch_absent" = true ] && [ "$tmux_absent" = true ] \
     && [ -z "$failure" ]; then
    ok=true
    display="[story] leased reap $canonical_id: exact worktree and branch are absent; no tmux window is named for the story."
  else
    display="[story] leased reap $canonical_id failed: ${failure:-one or more exact postconditions remain false}."
  fi
  leased_reap_receipt "$lease" "$ok" "$registration_absent" "$path_absent" \
    "$branch_absent" "$tmux_absent" "$reaped_wt" "$reaped_br" "$reaped_tmux" "$display"
  [ "$ok" = true ]
}

# cmd_reap <story-id> — reclaim a CLOSED story's worktree and branch, then
# close its tmux window (SH-208). The autonomous charter's own last act:
# unlike `story.sh complete`, which is a QUESTION a human answers (plan
# previews, execute acts), this is a COMMAND with nothing left to negotiate
# -- the charter has already closed the story and merged its PR by the time
# it runs this, so the only question left is whether it is SAFE to discard
# the workspace, never whether to.
#
# There is deliberately no "record what happened" step. The original reason
# was that a closed story could not be commented on at all -- SH-261 removed
# that, so `story comment` would now be accepted here. What has not changed
# is that no such step has been designed: it would have to report on its own
# destruction, from a worktree that is being deleted underneath it, and that
# is a question for whoever wants the record, not a line to add in passing.
# Its exit JSON remains the only record, which is why
# the tmux window -- the one thing that could still observe it -- is killed
# LAST: destructive git work happens first, so nothing is ever left pending
# when the window dies out from under this process.
#
# Preflight is a HARD, ALL-OR-NOTHING refusal, not partial best-effort like
# `complete`: unless the story is CLOSED and the worktree/branch are BOTH
# safe to discard, NOTHING is touched -- no worktree, no branch, no window.
# `complete` is human-supervised interactive cleanup, where a partial result
# ("removed the worktree, left the unmerged branch") is useful information
# for someone still there to read it. `reap` is machine-triggered
# self-destruction with nobody watching; silently discarding a worktree
# while leaving unmerged work orphaned would be a defect no one would see
# happen. This is also what makes an autonomous hard stop (`story block`,
# which leaves the story open) structurally unable to reach the destructive
# half of this verb -- the charter's own prose telling it "never reap past a
# hard stop" is backed by a gate that does not trust the prose alone.
#
# `current` is deliberately NOT a veto here, unlike `_story_worktree_status`'s
# use in `complete`. There it protects an interactive human's shell from
# having its cwd vanish out from under it. Here, by the time this runs,
# `_complete_prepare`'s own `enter_checkout` has already `cd`d this PROCESS
# to the main checkout -- `git worktree remove` sees an ordinary linked
# worktree, not its own cwd, regardless of where the invoking shell (the
# agent's own tmux pane) still stands. Killing that pane's window in the
# last step is what makes it safe for the agent's shell to have been
# standing there when this was called.
cmd_reap() {
  local id="${1:-}"
  [ -n "$id" ] || fail "usage: story.sh reap <story-id>"
  shift
  [ "$#" -eq 0 ] || fail "usage: story.sh reap <story-id>"

  if [ -n "${STORYHOOK_REAP_LEASE_V1:-}" ]; then
    cmd_reap_leased "$id" "$STORYHOOK_REAP_LEASE_V1"
    return
  fi

  # A dispatched worktree owns an exact private marker written before handoff.
  # Read it before _complete_prepare changes directory and before project
  # checkout/provider discovery can redirect cleanup. The leased path validates
  # every field and proves every postcondition; a present but unreadable marker
  # is therefore a refusal, never permission to fall back to mutable discovery.
  local caller_git_dir caller_marker caller_lease
  caller_git_dir=$(git rev-parse --absolute-git-dir 2>/dev/null || printf '')
  if [ -n "$caller_git_dir" ]; then
    caller_marker="$caller_git_dir/$CLEANUP_LEASE_MARKER"
    if [ -e "$caller_marker" ] || [ -L "$caller_marker" ]; then
      [ -f "$caller_marker" ] && [ ! -L "$caller_marker" ] \
        || refuse "invalid-cleanup-lease-marker" "story.sh reap: the calling worktree's cleanup lease marker is not a regular private file."
      caller_lease=$(command cat -- "$caller_marker" 2>/dev/null) \
        || refuse "unreadable-cleanup-lease-marker" "story.sh reap: could not read the calling worktree's cleanup lease marker."
      cmd_reap_leased "$id" "$caller_lease"
      return
    fi
  fi

  _complete_prepare "$id"
  id="$CMP_ID"

  if [ "$CMP_SUPER" != "CLOSED" ]; then
    refuse "not-closed" "story.sh reap: $id is not closed (state \`$CMP_STATE\`) -- refusing to reclaim a worktree for a story that isn't done."
  fi
  if [ "$CMP_STATE" != "$CMP_DONE_STATE" ]; then
    local state_details
    state_details=$(jq -n --arg state "$CMP_STATE" --arg completion_state "$CMP_DONE_STATE" \
      '{state:$state, completion_state:$completion_state}')
    refuse_with "not-completion-state" \
      "story.sh reap: $id is in CLOSED state \`$CMP_STATE\`, but this project's completion state is \`$CMP_DONE_STATE\` -- refusing to reclaim work completed as abandonment. Reopen it, move it to \`$CMP_DONE_STATE\`, then reap again." \
      "$state_details"
  fi
  case "$CMP_WT_STATUS" in
    dirty)  refuse "dirty-worktree" "story.sh reap: $id's worktree ($CMP_WT_PATH) has uncommitted changes -- refusing to discard them. Clean or commit them, then reap again." ;;
    locked) refuse "locked-worktree" "story.sh reap: $id's worktree ($CMP_WT_PATH) is locked -- refusing to reclaim it." ;;
  esac
  case "$CMP_BR_STATUS" in
    protected) refuse "protected-branch" "story.sh reap: $id's branch ($CMP_WT_BRANCH) is protected -- refusing to delete it." ;;
    unmerged)  refuse "unmerged-branch" "story.sh reap: $id's branch ($CMP_WT_BRANCH) is not merged into \`$CMP_DEFAULT\` -- refusing to discard unmerged work." ;;
  esac

  if [ -n "$DRY_RUN" ]; then
    local -a commands=()
    if [ "$CMP_WT_STATUS" != "missing" ]; then
      commands+=("git worktree remove $CMP_WT_PATH" "git worktree prune")
    fi
    [ "$CMP_BR_STATUS" = "deletable" ] && commands+=("git branch -d $CMP_WT_BRANCH")
    commands+=("tmux kill-window -t <window of $CMP_WNAME>")
    local cmds_json
    cmds_json=$(printf '%s\n' "${commands[@]}" | jq -R -s 'split("\n")|map(select(length>0))')
    jq -n --arg id "$id" --argjson cmds "$cmds_json" '
      {
        ok: true, dry_run: true, id: $id, commands: $cmds,
        display: ("[story] DRY RUN reap: would reclaim " + $id + "'"'"'s worktree and branch, and close its tmux window.")
      }'
    return 0
  fi

  # Scalar booleans, deliberately NOT named removed_wt/removed_bl -- those
  # names are cmd_complete_execute's own ARRAYS, and reusing them (even as
  # properly `local`-scoped scalars) reads as a collision to static analysis
  # and to the next person grepping this file.
  local reaped_wt=false reaped_br=false wt_fail="" br_fail=""

  if [ "$CMP_WT_STATUS" != "missing" ]; then
    if git worktree remove "$CMP_WT_PATH" >/dev/null 2>&1; then
      # No --force: git itself refuses a dirty or current worktree (moot
      # here -- dirty already refused above, and this process has already
      # left "current" behind), and that veto is a feature, not an
      # obstacle to route around.
      git worktree prune >/dev/null 2>&1 || true
      reaped_wt=true
    else
      wt_fail="git worktree remove refused for $CMP_WT_PATH"
    fi
  fi
  if [ "$CMP_BR_STATUS" = "deletable" ]; then
    if delete_merged_local_branch "$CMP_WT_BRANCH" "$CMP_DEFAULT"; then
      reaped_br=true
    else
      br_fail="could not delete branch $CMP_WT_BRANCH"
    fi
  fi

  local display="[story] reap $id: " reap_ok=true
  if [ "$reaped_wt" = true ]; then
    display="${display}removed worktree \`$CMP_WT_PATH\`, "
  elif [ -n "$wt_fail" ]; then
    display="${display}could not remove worktree \`$CMP_WT_PATH\`, "
    reap_ok=false
  else
    display="${display}worktree already gone, "
  fi
  if [ "$reaped_br" = true ]; then
    display="${display}deleted branch \`$CMP_WT_BRANCH\`."
  elif [ -n "$br_fail" ]; then
    display="${display}could not delete branch \`$CMP_WT_BRANCH\`."
    reap_ok=false
  else
    display="${display}branch already gone."
  fi
  [ -n "$wt_fail" ] && display="$display $wt_fail."
  [ -n "$br_fail" ] && display="$display $br_fail."

  # Best-effort, LAST -- see _close_story_window for why nothing here needs a
  # guard past "was a pane found at all".
  _close_story_window "$CMP_WNAME" || true

  jq -n --arg id "$id" --argjson ok "$reap_ok" \
        --argjson rwt "$reaped_wt" --argjson rbr "$reaped_br" \
        --arg wtfail "$wt_fail" --arg brfail "$br_fail" --arg display "$display" '
    {
      ok: $ok, id: $id,
      removed: { worktree: $rwt, branch: $rbr }
    }
    + (if $wtfail == "" then {} else {worktree_error: $wtfail} end)
    + (if $brfail == "" then {} else {branch_error: $brfail} end)
    + { display: $display }'
  [ "$reap_ok" = true ]
}

# ---- release: unclaim and reset ---------------------------------------------
#
# `unclaim` and `reset` are the two halves of handing a story back, split by
# what they are allowed to destroy (SH-484):
#
#   unclaim  release the claim, comment, close the window. NOTHING on disk.
#   reset    the same, then delete the worktree AND the branch.
#
# Both are the inverse of `story claim`, and both delegate the state change
# itself to `story unclaim` (SH-483) rather than composing a `story move`: the
# state a story was claimed FROM is derived from its own event log inside the
# release transaction, so no caller has to carry that answer around, and the
# `todo` fallback is reported rather than substituted silently.
#
# SELF-TERMINATION resolves differently for the two, and the asymmetry is
# deliberate. `unclaim` touches nothing on disk, so when the caller's own pane
# is the story's window it does everything it was asked to do and SKIPS ONLY
# the window kill, naming the skip. Killing that pane would destroy the very
# answer SH-483 requires be said out loud — which state the story went back
# to, and whether that was a fallback. `reset` cannot skip its destructive
# step, because that step is the verb, and skipping the window kill instead
# would leave a live shell pointed at a deleted directory; so `reset` refuses,
# and `--force` does not override it. That is why reap's answer (kill last,
# after flushing) fits neither: reap is charter-only, its prose says nothing
# after it can be observed, and its result answers nothing a caller needs.

UNCLAIM_USAGE="usage: story.sh unclaim <story-id> [--comment <text> | --no-comment]"
RESET_USAGE="usage: story.sh reset <story-id> [--force] [--comment <text> | --no-comment]"

# _parse_release_args <usage> <allow-force> <args...> — the flag loop `unclaim`
# and `reset` share, into REL_ID / REL_COMMENT_MODE / REL_COMMENT_TEXT /
# REL_FORCE. `--comment` and `--no-comment` together are refused here rather
# than deferred to the CLI, so the refusal arrives in the helper's own JSON
# shape — the one the router reads — instead of as a passed-through CLI error.
REL_ID=""
REL_COMMENT_MODE=""
REL_COMMENT_TEXT=""
REL_FORCE=false
_parse_release_args() {
  local usage="$1" allow_force="$2"
  shift 2
  REL_ID=""; REL_COMMENT_MODE=""; REL_COMMENT_TEXT=""; REL_FORCE=false
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --comment)
        [ "$#" -ge 2 ] || fail "--comment needs the text to post. $usage"
        [ "$REL_COMMENT_MODE" != "none" ] \
          || fail "--comment and --no-comment cannot be combined — pick one. $usage"
        REL_COMMENT_MODE="text"
        REL_COMMENT_TEXT="$2"
        shift 2
        ;;
      --no-comment)
        [ "$REL_COMMENT_MODE" != "text" ] \
          || fail "--comment and --no-comment cannot be combined — pick one. $usage"
        REL_COMMENT_MODE="none"
        shift
        ;;
      --force)
        [ "$allow_force" = true ] || fail "$usage"
        REL_FORCE=true
        shift
        ;;
      -*) fail "unknown option \`$1\`. $usage" ;;
      *)
        [ -z "$REL_ID" ] || fail "$usage"
        REL_ID="$1"
        shift
        ;;
    esac
  done
  [ -n "$REL_ID" ] || fail "$usage"
}

# _release_story <actor> <id> — run `story unclaim` for the caller and publish
# its answer as REL_RESULT (ok|conflict|error) plus the fields that answer go
# with. Under STORY_DRY_RUN the CLI's own --dry-run is used, which reads for
# REAL: a release that would conflict conflicts in the preview too, rather than
# previewing something that could not happen.
REL_RESULT=""
REL_FROM=""
REL_TO=""
REL_FALLBACK=""
REL_EXPECTED=""
REL_ACTUAL=""
REL_ERROR=""
REL_MESSAGE=""
_release_story() {
  local actor="$1" id="$2" json
  local -a args=(unclaim "$id" --json)
  case "$REL_COMMENT_MODE" in
    text) args+=(--comment "$REL_COMMENT_TEXT") ;;
    none) args+=(--no-comment) ;;
  esac
  [ -z "$DRY_RUN" ] || args+=(--dry-run)

  json=$(story_cli --actor "$actor" "${args[@]}" 2>/dev/null) || true
  REL_RESULT=$(printf '%s' "$json" | jq -r '.result // "error"' 2>/dev/null || printf 'error')
  REL_FROM=$(printf '%s' "$json" | jq -r '.unclaimed_from // ""' 2>/dev/null || printf '')
  REL_TO=$(printf '%s' "$json" | jq -r '.story.story.state // ""' 2>/dev/null || printf '')
  REL_FALLBACK=$(printf '%s' "$json" | jq -r '.restore_fallback // ""' 2>/dev/null || printf '')
  REL_EXPECTED=$(printf '%s' "$json" | jq -r '.expected // ""' 2>/dev/null || printf '')
  REL_ACTUAL=$(printf '%s' "$json" | jq -r '.actual // ""' 2>/dev/null || printf '')
  REL_ERROR=$(printf '%s' "$json" | jq -r '.error // "no answer from the story CLI"' 2>/dev/null \
    || printf 'no answer from the story CLI')
  REL_MESSAGE=$(printf '%s' "$json" | jq -r '.message // ""' 2>/dev/null || printf '')
}

# _release_fallback_clause — one sentence naming why the story did not go back
# where it came from, or nothing at all. SH-483 requires the substitution be
# said out loud; this is the plugin's half of saying it.
_release_fallback_clause() {
  case "$REL_FALLBACK" in
    "") : ;;
    no-prior-state)
      printf ' It was created directly in that state, so there was no earlier state to restore (no-prior-state).' ;;
    prior-state-removed)
      printf ' The state it was claimed from has since been removed from this project (prior-state-removed).' ;;
    prior-state-closed)
      printf ' The state it was claimed from is no longer OPEN, so restoring it would have closed the story (prior-state-closed).' ;;
    *)
      printf ' Its claimed-from state could not be restored (%s).' "$REL_FALLBACK" ;;
  esac
}

# _close_release_window <window-name> — close the story's window if this verb
# may, and publish what happened as RELEASE_WINDOW / RELEASE_CLOSED.
#
# `CMP_WINDOW_STATUS` alone is the wrong input here. `_complete_prepare`
# classifies it only when the CALLER is inside tmux, which is right for the
# question SH-308 added it for — "is this window MINE, which `complete` must
# not close" — but it cannot answer "does a window exist" for a shell that is
# not attached to the server. From there a live story window classifies as
# `none`, so `unclaim` would leave it open while reporting nothing, and `reset`
# would delete a worktree out from under a shell still running in it. `reap`
# has never had this gap because it kills unconditionally and reports nothing.
#
# So the `none` case asks `_close_story_window` itself and takes its RETURN
# VALUE as the classifier — the same source of truth reap uses, rather than a
# second copy of the lookup. `self` is unreachable from outside tmux by
# construction (it requires $TMUX_PANE), so nothing is being skipped: a caller
# with no pane of their own cannot be standing in the window being closed.
#
# `_complete_prepare` is deliberately NOT widened to drop its $TMUX gate:
# `complete` counts an `open` window in CMP_ACTIONS and closes one in
# `execute`, so lifting the gate would change what `complete` does from outside
# tmux, which is a different verb's behaviour and a different story's decision.
RELEASE_WINDOW=""
RELEASE_CLOSED=false
RELEASE_WINDOW_ERROR=""
_close_release_window() {
  local wname="$1"
  RELEASE_WINDOW="$CMP_WINDOW_STATUS"
  RELEASE_CLOSED=false
  RELEASE_WINDOW_ERROR=""
  case "$CMP_WINDOW_STATUS" in
    self) return 0 ;;
    open)
      if _close_story_window "$wname"; then
        RELEASE_CLOSED=true
      else
        local rc=$?
        [ "$rc" -ne 1 ] \
          && RELEASE_WINDOW_ERROR="could not prove tmux window absence for $wname" \
          || RELEASE_WINDOW="none"
      fi ;;
    *)
      if _close_story_window "$wname"; then
        RELEASE_WINDOW="open"; RELEASE_CLOSED=true
      else
        local rc=$?
        [ "$rc" -eq 1 ] \
          || RELEASE_WINDOW_ERROR="could not prove tmux window absence for $wname"
      fi
      ;;
  esac
  return 0
}

# cmd_unclaim <story-id> [--comment <text> | --no-comment] — hand a claim back
# and close the story's tmux window. The worktree and branch are REPORTED and
# left exactly as found; `reset` is the verb that removes them.
cmd_unclaim() {
  _parse_release_args "$UNCLAIM_USAGE" false "$@"
  if [ -n "${STORYHOOK_REAP_LEASE_V1:-}" ]; then
    cmd_unclaim_leased "$REL_ID" "$STORYHOOK_REAP_LEASE_V1"
    return
  fi
  _complete_prepare "$REL_ID"
  local id="$CMP_ID"

  _release_story unclaim "$id"
  case "$REL_RESULT" in
    ok) : ;;
    conflict)
      # A lost CAS means no claim of ours was released, so no window of anyone
      # else's is closed over it either.
      refuse "unclaim-conflict" \
        "story.sh unclaim: $id is not claimed (expected \`$REL_EXPECTED\`, found \`$REL_ACTUAL\`) — another session may have moved it. Nothing was released and its tmux window was left alone." ;;
    *) fail "story unclaim $id failed: $REL_ERROR" ;;
  esac

  if [ -n "$DRY_RUN" ]; then
    # The window line is previewed unless it is the caller's own, which is
    # reap's own convention: without looking, nothing here can know whether a
    # window exists, and a preview that under-promises is as misleading as one
    # that over-promises.
    local -a dry_cmds=("story unclaim $id")
    [ "$CMP_WINDOW_STATUS" != "self" ] && dry_cmds+=("tmux kill-window -t <window of $CMP_WNAME>")
    local dry_json
    dry_json=$(printf '%s\n' "${dry_cmds[@]}" | jq -R -s 'split("\n")|map(select(length>0))')
    jq -n --arg id "$id" --argjson cmds "$dry_json" --arg msg "$REL_MESSAGE" '
      {
        ok: true, dry_run: true, id: $id, commands: $cmds,
        display: ("[story] DRY RUN unclaim " + $id + ": " + $msg)
      }'
    return 0
  fi

  _close_release_window "$CMP_WNAME"
  local closed="$RELEASE_CLOSED"
  local cleanup_ok=true
  [ -z "$RELEASE_WINDOW_ERROR" ] || cleanup_ok=false

  local display="[story] unclaim $id: released from \`$REL_FROM\` back to \`$REL_TO\`."
  display="$display$(_release_fallback_clause)"
  case "$RELEASE_WINDOW" in
    open) [ "$closed" = true ] \
            && display="$display Closed its tmux window \`$CMP_WNAME\`." \
            || display="$display Its tmux window \`$CMP_WNAME\` could not be closed." ;;
    self) display="$display Its tmux window \`$CMP_WNAME\` was left open — it is the one you are in, and closing it would have destroyed this answer. Close it yourself when you are done." ;;
    *)    : ;;
  esac
  if [ "$CMP_WT_STATUS" != "missing" ]; then
    display="$display Its worktree \`$CMP_WT_PATH\` ($CMP_WT_STATUS) was left in place — \`story.sh reset $id\` is the verb that removes it."
  fi
  [ -z "$RELEASE_WINDOW_ERROR" ] || display="$display $RELEASE_WINDOW_ERROR."

  jq -n --argjson ok "$cleanup_ok" --arg id "$id" --arg from "$REL_FROM" --arg to "$REL_TO" \
        --arg fallback "$REL_FALLBACK" --arg window "$RELEASE_WINDOW" \
        --argjson closed "$closed" --arg wtpath "$CMP_WT_PATH" \
        --arg wtstatus "$CMP_WT_STATUS" --arg display "$display" '
    {
      ok: $ok, id: $id,
      unclaimed_from: $from, restored_to: $to
    }
    + (if $fallback == "" then {} else {restore_fallback: $fallback} end)
    + {
      window: $window, closed_window: $closed,
      worktree_path: $wtpath, worktree_status: $wtstatus,
      display: $display
    }'
  [ "$cleanup_ok" = true ]
}

# cmd_reset <story-id> [--force] — unclaim, then delete the worktree AND the
# branch. For a story inadvertently abandoned (a crash, a reboot) where
# restarting beats inheriting somebody else's half-finished scratch directory.
#
# Every `--force`-able refusal below asks ONE question — "am I about to destroy
# something that is not recoverable elsewhere?" — which is why there is one
# flag and not three. The two refusals `--force` does NOT cover are the two
# that ask something else: a protected branch is a repository-level statement
# rather than a scratch artifact, and self-termination is about destroying the
# caller's own ground mid-command.
#
# UNPUSHED, not UNMERGED, is the branch question here — see CMP_BR_UNPUSHED in
# _complete_prepare. reap refuses `unmerged`, which is strictly stronger and
# would refuse the case this verb exists for; `deletable`/`unmerged` therefore
# does not gate anything here, and the deletion is `git branch -D` rather than
# delete_merged_local_branch for exactly that reason.
cmd_reset() {
  _parse_release_args "$RESET_USAGE" true "$@"
  _complete_prepare "$REL_ID"
  local id="$CMP_ID"

  # Refusals first, most-absolute first, before anything at all is mutated.
  if [ "$CMP_WINDOW_STATUS" = "self" ]; then
    refuse "self-window" \
      "story.sh reset: $id's tmux window \`$CMP_WNAME\` is the one you are in — resetting it would delete this shell's own ground mid-command. Run it from another window (\`--force\` does not override this)."
  fi
  if [ "$CMP_WT_STATUS" = "current" ]; then
    refuse "current-worktree" \
      "story.sh reset: you are standing in $id's worktree ($CMP_WT_PATH) — removing it would leave this shell in a deleted directory. \`cd\` out of it first (\`--force\` does not override this)."
  fi
  if [ "$CMP_BR_STATUS" = "protected" ]; then
    refuse "protected-branch" \
      "story.sh reset: $id's branch ($CMP_WT_BRANCH) is protected — refusing to delete it, and \`--force\` does not override this."
  fi
  if [ "$REL_FORCE" != true ]; then
    case "$CMP_WT_STATUS" in
      locked)
        refuse "locked-worktree" \
          "story.sh reset: $id's worktree ($CMP_WT_PATH) is locked — somebody took that lock deliberately. Unlock it, or pass \`--force\`." ;;
      dirty)
        refuse "dirty-worktree" \
          "story.sh reset: $id's worktree ($CMP_WT_PATH) has uncommitted changes, which exist nowhere else — refusing to discard them. Commit them, or pass \`--force\` to destroy them." ;;
    esac
    if [ "$CMP_BR_UNPUSHED" -gt 0 ]; then
      # refuse_with, not refuse: the COUNT is the evidence for this verdict,
      # and a refusal that only says "some" makes the operator run git
      # themselves to decide whether to override it.
      refuse_with "unpushed-commits" \
        "story.sh reset: $id's branch ($CMP_WT_BRANCH) has $CMP_BR_UNPUSHED commit(s) on no remote — refusing to discard work nothing else has. Push the branch, or pass \`--force\`." \
        "$(jq -n --argjson n "$CMP_BR_UNPUSHED" '{unpushed: $n}')"
    fi
  fi

  _release_story reset "$id"
  local unclaimed=false conflict=""
  case "$REL_RESULT" in
    ok) unclaimed=true ;;
    # A conflict PROVES no claim is held: the story is not in the active state,
    # so nobody is working it through the state machine. The worktree is still
    # litter `reap` refuses to touch (the story is open), and this is the verb
    # for that, so the teardown proceeds and the conflict is reported.
    conflict) conflict="$REL_ACTUAL" ;;
    *) fail "story unclaim $id failed: $REL_ERROR — nothing was removed." ;;
  esac

  if [ -n "$DRY_RUN" ]; then
    local -a dry_cmds=()
    [ "$REL_RESULT" = "ok" ] && dry_cmds+=("story unclaim $id")
    if [ "$CMP_WT_STATUS" != "missing" ]; then
      dry_cmds+=("git worktree remove --force $CMP_WT_PATH" "git worktree prune")
    fi
    [ "$CMP_BR_STATUS" != "missing" ] && dry_cmds+=("git branch -D $CMP_WT_BRANCH")
    [ "$CMP_WINDOW_STATUS" != "self" ] && dry_cmds+=("tmux kill-window -t <window of $CMP_WNAME>")
    local dry_json
    dry_json=$(printf '%s\n' "${dry_cmds[@]:-}" | jq -R -s 'split("\n")|map(select(length>0))')
    # The sentence is a PROJECTION of the command list, never a fixed one
    # written beside it. "would delete its worktree, its branch and its tmux
    # window" is a false statement whenever there is no worktree or no branch,
    # and a preview is the one place the reader has nothing else to check it
    # against. Written as a jq projection rather than as shell string-building
    # so the two structurally cannot drift.
    jq -n --arg id "$id" --argjson cmds "$dry_json" --argjson forced "$REL_FORCE" \
          --argjson unclaimed "$([ "$REL_RESULT" = "ok" ] && printf true || printf false)" '
      {
        ok: true, dry_run: true, id: $id, forced: $forced, commands: $cmds,
        display: ("[story] DRY RUN reset " + $id + ": would "
          + (if $unclaimed then "release it" else "release nothing (it is not claimed)" end)
          + ", then run: "
          + ($cmds | map(select(startswith("story unclaim") | not)) | join("; "))
          + ".")
      }'
    return 0
  fi

  local removed_wt=false removed_br=false wt_fail="" br_fail=""
  if [ "$CMP_WT_STATUS" != "missing" ]; then
    # --force unconditionally: a dirty worktree that got this far was either
    # clean or explicitly acknowledged above, and git's own veto would only
    # re-litigate a decision already taken. A LOCKED one needs --force twice,
    # which is git's own spelling for "yes, the lock too".
    local -a rm_args=(worktree remove --force "$CMP_WT_PATH")
    if [ "$CMP_WT_STATUS" = "locked" ]; then
      rm_args=(worktree remove --force --force "$CMP_WT_PATH")
    fi
    if git "${rm_args[@]}" >/dev/null 2>&1; then
      git worktree prune >/dev/null 2>&1 || true
      removed_wt=true
    else
      wt_fail="git worktree remove refused for $CMP_WT_PATH"
    fi
  fi
  if [ "$CMP_BR_STATUS" != "missing" ]; then
    if [ -n "$wt_fail" ]; then
      br_fail="branch $CMP_WT_BRANCH left in place: its worktree could not be removed"
    elif git branch -D "$CMP_WT_BRANCH" >/dev/null 2>&1; then
      removed_br=true
    else
      br_fail="could not delete branch $CMP_WT_BRANCH"
    fi
  fi

  if registered_worktree_branch "$CMP_WT_PATH" >/dev/null 2>&1 || [ -e "$CMP_WT_PATH" ]; then
    wt_fail="${wt_fail:-worktree cleanup postcondition failed for $CMP_WT_PATH}"
    removed_wt=false
  fi
  if local_branch_exists "$CMP_WT_BRANCH"; then
    br_fail="${br_fail:-branch cleanup postcondition failed for $CMP_WT_BRANCH}"
    removed_br=false
  fi

  # `self` already refused above, so this can only find somebody else's window
  # — or one this caller could not see from outside tmux.
  _close_release_window "$CMP_WNAME"
  local closed="$RELEASE_CLOSED"
  local reset_ok=true
  if [ -n "$wt_fail" ] || [ -n "$br_fail" ] || [ -n "$RELEASE_WINDOW_ERROR" ]; then
    reset_ok=false
  fi

  local display="[story] reset $id: "
  if [ "$unclaimed" = true ]; then
    display="${display}released from \`$REL_FROM\` back to \`$REL_TO\`$(_release_fallback_clause)"
    display="${display%.} — "
  else
    display="${display}the story was not claimed (it is \`$conflict\`), so nothing was released — "
  fi
  if [ "$removed_wt" = true ]; then
    display="${display}removed worktree \`$CMP_WT_PATH\`, "
  elif [ -n "$wt_fail" ]; then
    display="${display}left worktree \`$CMP_WT_PATH\` in place, "
  else
    display="${display}worktree already gone, "
  fi
  if [ "$removed_br" = true ]; then
    display="${display}deleted branch \`$CMP_WT_BRANCH\`."
  elif [ -n "$br_fail" ]; then
    display="${display}left branch \`$CMP_WT_BRANCH\` in place."
  else
    display="${display}branch already gone."
  fi
  [ "$closed" = true ] && display="$display Closed its tmux window \`$CMP_WNAME\`."
  [ -n "$wt_fail" ] && display="$display $wt_fail."
  [ -n "$br_fail" ] && display="$display $br_fail."
  [ -z "$RELEASE_WINDOW_ERROR" ] || display="$display $RELEASE_WINDOW_ERROR."
  [ "$REL_FORCE" = true ] && display="$display (\`--force\`)"

  jq -n --argjson ok "$reset_ok" --arg id "$id" --argjson unclaimed "$unclaimed" --arg conflict "$conflict" \
        --arg from "$REL_FROM" --arg to "$REL_TO" --arg fallback "$REL_FALLBACK" \
        --argjson forced "$REL_FORCE" --argjson rwt "$removed_wt" --argjson rbr "$removed_br" \
        --arg wtfail "$wt_fail" --arg brfail "$br_fail" \
        --arg window "$RELEASE_WINDOW" --argjson closed "$closed" \
        --argjson unpushed "$CMP_BR_UNPUSHED" --arg display "$display" '
    {
      ok: $ok, id: $id, forced: $forced,
      unclaimed: $unclaimed
    }
    + (if $conflict == "" then {} else {unclaim_conflict: $conflict} end)
    + (if $unclaimed then {unclaimed_from: $from, restored_to: $to} else {} end)
    + (if $fallback == "" then {} else {restore_fallback: $fallback} end)
    + {
      removed: { worktree: $rwt, branch: $rbr },
      unpushed: $unpushed,
      window: $window, closed_window: $closed
    }
    + (if $wtfail == "" then {} else {worktree_error: $wtfail} end)
    + (if $brfail == "" then {} else {branch_error: $brfail} end)
    + { display: $display }'
  [ "$reset_ok" = true ]
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

# Every command except dispatch and capabilities reads the provider from the
# environment. Both resolve it only after parsing their own explicit
# --agent override.
if [ "${1:-}" != "dispatch" ] && [ "${1:-}" != "capabilities" ]; then
  configure_agent "${STORY_AGENT:-claude}"
fi

case "${1:-}" in
  dispatch)      shift; cmd_dispatch "$@" ;;
  capabilities)  shift; cmd_capabilities "$@" ;;
  complete)   shift; cmd_complete "$@" ;;
  reap)       shift; cmd_reap "$@" ;;
  unclaim)    shift; cmd_unclaim "$@" ;;
  reset)      shift; cmd_reset "$@" ;;
  view)       shift; cmd_view "$@" ;;
  list)       shift; cmd_list "$@" ;;
  create)     shift; cmd_create "$@" ;;
  doctor)     shift; cmd_doctor "$@" ;;
  capture)    shift; cmd_capture "$@" ;;
  notify)     shift; cmd_notify "$@" ;;
  ensure-cli) shift; cmd_ensure_cli "$@" ;;
  context)    shift; cmd_context "$@" ;;
  sync)       shift; cmd_sync "$@" ;;
  handoff)    shift; cmd_handoff "$@" ;;
  triage)     shift; cmd_triage "$@" ;;
  scaffold-claude-md) shift; cmd_scaffold_claude_md "$@" ;;
  scaffold-agents-md) shift; cmd_scaffold_agents_md "$@" ;;
  *)          fail "usage: story.sh <list | view <story-id> | dispatch (<story-id> | --next) [--auto] [--full-auto] [--force] [--resume] [--agent=claude|codex] [--model=<id>] [--effort=<id>] [--speed=standard|fast] | capabilities [--agent=claude|codex] | create --title <t> [--description-file <p>] | complete <plan|execute> <story-id> | reap <story-id> | unclaim <story-id> [--comment <t> | --no-comment] | reset <story-id> [--force] [--comment <t> | --no-comment] | doctor | capture <story-id> | notify <story-id> <message> | ensure-cli | context [--full] | sync [--since <d>] | handoff [--since <d>] | triage | scaffold-agents-md [--path <file>] | scaffold-claude-md [--path <file>]>" ;;
esac
