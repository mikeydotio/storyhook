#!/usr/bin/env bash
# Codex's provider contract beside the existing Claude dispatch suite: launch,
# screen readiness, Plan-mode transition, Tab submission, provider worktree,
# refusal rollback, doctor, and safe reap.
source "$(dirname "$0")/lib.sh"

FAKE_TMUX_DIR="$TESTS_DIR/fakes"
FAKE_BIN=$(mktemp -d /tmp/story-test-codex-bin.XXXXXX)
_TMP_REPOS+=("$FAKE_BIN")
printf '#!/bin/sh\nexit 0\n' >"$FAKE_BIN/codex"
chmod +x "$FAKE_BIN/codex"

fresh_tmux() {
  export FAKE_TMUX_STATE
  FAKE_TMUX_STATE=$(mktemp -d /tmp/story-test-codex-tmux.XXXXXX)
  _TMP_REPOS+=("$FAKE_TMUX_STATE")
  unset FAKE_TMUX_LAUNCH_MANGLE FAKE_TMUX_FAIL_SEND_KEYS \
    FAKE_TMUX_FAIL_RUN_SHELL FAKE_TMUX_CAPTURE
}

run_codex() {
  local repo="$1"; shift
  (
    cd "$repo" &&
      PATH="$FAKE_BIN:$FAKE_TMUX_DIR:$PATH" \
      TMUX=fake TMUX_PANE=%0 STORY_AGENT=codex \
      STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
      STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 \
      FAKE_TMUX_CAPTURE=marker \
      FAKE_TMUX_CODEX_SENTINEL_MODE=identity \
      FAKE_TMUX_CODEX_PLUGIN_ROOT="$PLUGIN_ROOT" \
      bash "$SCRIPT" "$@" 2>&1
  )
}

# Happy dispatch: Codex has its own launch, worktree, Plan key and submit key.
fresh_tmux
repo=$(mk_story_repo CDX)
id=$(new_story "$repo" "Codex dispatch happy path")
out=$(run_codex "$repo" dispatch "$id")
assert_eq "$(jqf "$out" .ok)" "true" "dispatch: ok"
assert_eq "$(jqf "$out" .agent)" "codex" "dispatch: selected provider"
assert_eq "$(jqf "$out" .readiness_confirmed)" "true" "dispatch: readiness"
assert_eq "$(jqf "$out" .plan_mode_confirmed)" "true" "dispatch: Plan footer"
assert_eq "$(jqf "$out" .submit_key)" "Tab" "dispatch: reports submit key"
assert_eq "$(jqf "$out" .prompt_confirmed)" "true" \
  "dispatch: Codex's empty placeholder confirms Tab submission"
assert_eq "$(tail -1 "$FAKE_TMUX_STATE/submit_keys.log")" "Tab" "dispatch: submitted with Tab"
[ -f "$FAKE_TMUX_STATE/plan_mode" ] || fail_test "dispatch: never sent Shift+Tab/BTab"
[ -d "$repo/.codex/worktrees/$id" ] || fail_test "dispatch: Codex worktree missing"
[ ! -d "$repo/.claude/worktrees/$id" ] || fail_test "dispatch: created a Claude worktree"
codex_private_git=$(git -C "$repo/.codex/worktrees/$id" rev-parse --absolute-git-dir)
codex_lease="$codex_private_git/storyhook-cleanup-lease-v1.json"
[ -f "$codex_lease" ] || fail_test "dispatch: Codex cleanup lease marker missing"
assert_eq "$(jq -r .story_id "$codex_lease")" "$id" \
  "dispatch: Codex marker carries canonical story"
[ ! -e "$repo/.codex/worktrees/$id/.claude/dispatch-sentinel.json" ] \
  || fail_test "dispatch: fake published a Claude-only sentinel in Codex"
grep -q '^\.codex/worktrees/$' "$repo/.gitignore" \
  || fail_test "dispatch: Codex worktree container was not ignored"
assert_contains "$(cat "$FAKE_TMUX_STATE/submitted")" "story show $id" \
  "dispatch: charter reached Codex as one submitted block"
assert_contains "$(cat "$FAKE_TMUX_STATE/submitted")" \
  "story comment $id your-exact-approved-plan’ the first implementation step" \
  "dispatch: attended Codex plan persists itself as step one"
assert_contains "$(cat "$FAKE_TMUX_STATE/submitted")" \
  "post the plan verbatim rather than summarizing it" \
  "dispatch: attended Codex persists the exact approved plan"
assert_contains "$(cat "$FAKE_TMUX_STATE/submitted")" \
  "every linked pull request title contains the exact story ID $id" \
  "dispatch: attended Codex requires the story ID in the PR title"
[ ! -e "$FAKE_TMUX_STATE/run_shell.log" ] \
  || fail_test "dispatch: attended Codex armed the autonomous plan watcher"

# Codex renders its prompt before the model is ready. A Shift+Tab sent during
# that loading footer is silently ignored, so dispatch must wait for the footer
# to clear rather than sending the mode key as soon as the input glyph appears.
fresh_tmux
repo_loading=$(mk_story_repo CDL)
id_loading=$(new_story "$repo_loading" "Codex model-loading Plan transition")
out=$(FAKE_TMUX_CODEX_LOADING_POLLS=8 run_codex "$repo_loading" dispatch "$id_loading")
assert_eq "$(jqf "$out" .ok)" "true" "model loading: dispatch waits and succeeds"
assert_eq "$(jqf "$out" .plan_mode_confirmed)" "true" "model loading: Plan confirmed"
[ ! -f "$FAKE_TMUX_STATE/plan_key_ignored.log" ] \
  || fail_test "model loading: Shift+Tab was sent before Codex finished loading"

# The real 0.148.0 TUI can discard one Shift+Tab during a later startup phase
# even after its model label stops saying "loading". The bounded handshake may
# retry an unconfirmed key, but must stop as soon as the literal Plan footer is
# visible so it never cycles a confirmed session away from Plan.
fresh_tmux
repo_retry=$(mk_story_repo CDR)
id_retry=$(new_story "$repo_retry" "Codex dropped first Plan key")
out=$(FAKE_TMUX_IGNORE_PLAN_KEYS=1 run_codex "$repo_retry" dispatch "$id_retry")
assert_eq "$(jqf "$out" .ok)" "true" "dropped Plan key: bounded retry succeeds"
assert_eq "$(jqf "$out" .plan_mode_confirmed)" "true" "dropped Plan key: Plan confirmed"
assert_contains "$(cat "$FAKE_TMUX_STATE/plan_key_ignored.log")" "late TUI startup" \
  "dropped Plan key: fixture exercised the retry"

# Safe reap uses the provider's path and removes only the closed, merged leaf.
(cd "$repo" && story move "$id" done >/dev/null)
export FAKE_TMUX_PANES
FAKE_TMUX_PANES=$(printf '%s\t1\t%%1' "$id")
out=$(run_codex "$repo" reap "$id")
assert_eq "$(jqf "$out" .ok)" "true" "reap: ok"
assert_eq "$(jqf "$out" .removed.worktree)" "true" "reap: removed Codex worktree"
assert_eq "$(jqf "$out" .removed.branch)" "true" "reap: removed merged branch"
[ ! -d "$repo/.codex/worktrees/$id" ] || fail_test "reap: Codex worktree survived"

# A launch that never becomes Codex refuses before typing and rolls everything back.
fresh_tmux
repo_bad=$(mk_story_repo CDB)
id_bad=$(new_story "$repo_bad" "Codex launch refusal")
out=$(FAKE_TMUX_LAUNCH_MANGLE=1 STORY_READY_ATTEMPTS=2 run_codex "$repo_bad" dispatch "$id_bad")
assert_eq "$(jqf "$out" .ok)" "false" "readiness refusal: ok:false"
assert_eq "$(jqf "$out" .reason)" "pane-not-ready" "readiness refusal: reason"
assert_eq "$(cd "$repo_bad" && story show "$id_bad" --json | jq -r '.story.story.state')" "todo" \
  "readiness refusal: claim rolled back"
[ ! -d "$repo_bad/.codex/worktrees/$id_bad" ] || fail_test "readiness refusal: worktree survived"

# A failed Plan-mode key is also a pre-handoff refusal with complete rollback.
fresh_tmux
repo_plan=$(mk_story_repo CDP)
id_plan=$(new_story "$repo_plan" "Codex plan refusal")
out=$(FAKE_TMUX_FAIL_SEND_KEYS=all run_codex "$repo_plan" dispatch "$id_plan")
assert_eq "$(jqf "$out" .ok)" "false" "plan refusal: ok:false"
assert_eq "$(jqf "$out" .reason)" "plan-mode-unconfirmed" "plan refusal: reason"
assert_eq "$(cd "$repo_plan" && story show "$id_plan" --json | jq -r '.story.story.state')" "todo" \
  "plan refusal: claim rolled back"

# Doctor reports and tests the selected provider contract.
fresh_tmux
out=$(run_codex "$repo_plan" doctor)
assert_eq "$(jqf "$out" .ok)" "true" "doctor: ok"
assert_eq "$(jqf "$out" .agent)" "codex" "doctor: selected provider"
assert_eq "$(jqf "$out" .readiness_confirmed)" "true" "doctor: readiness"
assert_eq "$(jqf "$out" .plan_mode_confirmed)" "true" "doctor: Plan footer"
assert_eq "$(jqf "$out" .multiline_probe.first_line_held)" "true" "doctor: bracketed paste"

# Codex auto mode never guesses a Claude skill inventory.
id_auto=$(new_story "$repo_plan" "Codex auto dry run")
out=$(cd "$repo_plan" && PATH="$FAKE_BIN:$PATH" STORY_AGENT=codex STORY_DRY_RUN=1 \
  bash "$SCRIPT" dispatch "$id_auto" --auto 2>&1)
assert_eq "$(jqf "$out" .agent)" "codex" "dry auto: selected provider"
assert_eq "$(jqf "$out" .council)" "false" "dry auto: safe solo fallback"
assert_contains "$(jqf "$out" '.commands|join(" ")')" \
  "codex --no-alt-screen -c check_for_update_on_startup=false --approve-for-me --dangerously-bypass-hook-trust" \
  "dry auto: Codex uses later automatic review and trusts the packaged hook"
assert_contains "$(jqf "$out" '.commands|join(" ")')" \
  "-e STORYHOOK_AUTO=$id_auto" "dry auto: Codex child receives the autonomous marker"
assert_contains "$(jqf "$out" '.commands|join(" ")')" \
  "-e STORYHOOK_FULL_AUTO=" "dry auto: Codex child contains the engine marker"
assert_eq "$(jqf "$out" .launch_source)" "builtin" "dry auto: builtin launch source"
assert_eq "$(jqf "$out" .launch_overridden)" "false" "dry auto: launch is not overridden"
assert_contains "$(jqf "$out" .display)" "approves the plan automatically" \
  "dry auto: display reports automatic plan approval"
assert_contains "$(jqf "$out" '.commands|join(" ")')" "send-keys -t <pane> Tab" \
  "dry auto: Tab submission"
assert_contains "$(jqf "$out" '.commands|join(" ")')" \
  "--approve-codex-plan <pane>" \
  "dry auto: exact-pane plan approval is armed"
assert_contains "$(jqf "$out" .prompt)" \
  "story comment $id_auto your-exact-approved-plan’ the first implementation step" \
  "dry auto: Codex plan persists itself as step one"
assert_contains "$(jqf "$out" .prompt)" \
  "before changing files or running tests" \
  "dry auto: persistence precedes implementation"
assert_contains "$(jqf "$out" .prompt)" \
  "every linked pull request title contains the exact story ID $id_auto" \
  "dry auto: Codex requires the story ID in the PR title"

# A real fake-tmux Auto dispatch arms the pane watcher after Plan mode is
# confirmed and before prompt submission. An arming failure is a pre-handoff
# refusal with complete claim/worktree rollback.
fresh_tmux
repo_auto=$(mk_story_repo CDA)
id_auto_real=$(new_story "$repo_auto" "Codex auto watcher")
out=$(run_codex "$repo_auto" dispatch "$id_auto_real" --auto)
assert_eq "$(jqf "$out" .ok)" "true" "real auto: dispatch succeeds"
assert_contains "$(cat "$FAKE_TMUX_STATE/run_shell.log")" \
  "STORYHOOK_AUTO=$id_auto_real" "real auto: watcher carries the story marker"
assert_contains "$(cat "$FAKE_TMUX_STATE/run_shell.log")" \
  "STORYHOOK_FULL_AUTO=" "real auto: watcher contains the engine marker"
assert_contains "$(cat "$FAKE_TMUX_STATE/run_shell.log")" \
  "--approve-codex-plan %1" "real auto: watcher targets the confirmed pane"

fresh_tmux
repo_auto_fail=$(mk_story_repo CDF)
id_auto_fail=$(new_story "$repo_auto_fail" "Codex auto watcher failure")
out=$(FAKE_TMUX_FAIL_RUN_SHELL=1 run_codex "$repo_auto_fail" dispatch "$id_auto_fail" --auto)
assert_eq "$(jqf "$out" .ok)" "false" "auto watcher failure: refused"
assert_eq "$(jqf "$out" .reason)" "plan-approval-unarmed" \
  "auto watcher failure: reason"
assert_eq "$(cd "$repo_auto_fail" && story show "$id_auto_fail" --json | jq -r '.story.story.state')" \
  "todo" "auto watcher failure: claim rolled back"
assert_eq "$(jqf "$out" .plan_approval_armed)" "false" \
  "auto watcher failure: JSON reports the missing gate"

# The provider clause belongs only to Storyhook's built-in prompts. Every
# custom prompt remains a wholesale override, while PROMPT_EXTRA still follows
# the complete built-in prompt (including the provider clause).
out=$(cd "$repo_plan" && PATH="$FAKE_BIN:$PATH" STORY_AGENT=codex STORY_DRY_RUN=1 \
  STORY_PROMPT="custom attended <n>" bash "$SCRIPT" dispatch "$id_auto" 2>&1)
assert_eq "$(jqf "$out" .prompt)" "custom attended $id_auto" \
  "Codex attended custom prompt: remains wholesale"

out=$(cd "$repo_plan" && PATH="$FAKE_BIN:$PATH" STORY_AGENT=codex STORY_DRY_RUN=1 \
  STORY_COUNCIL=off STORY_AUTO_PROMPT_SOLO="custom solo <n>" \
  bash "$SCRIPT" dispatch "$id_auto" --auto 2>&1)
assert_eq "$(jqf "$out" .prompt)" "custom solo $id_auto" \
  "Codex auto custom prompt: remains wholesale"

out=$(cd "$repo_plan" && PATH="$FAKE_BIN:$PATH" STORY_AGENT=codex STORY_DRY_RUN=1 \
  STORY_COUNCIL=off STORY_AUTO_PROMPT="custom auto <n>" \
  bash "$SCRIPT" dispatch "$id_auto" --auto 2>&1)
assert_eq "$(jqf "$out" .prompt)" "custom auto $id_auto" \
  "Codex council-capable custom prompt: remains wholesale"

out=$(cd "$repo_plan" && PATH="$FAKE_BIN:$PATH" STORY_AGENT=codex STORY_DRY_RUN=1 \
  STORY_PROMPT_EXTRA="EXTRA-CLAUSE" bash "$SCRIPT" dispatch "$id_auto" --auto 2>&1)
case "$(jqf "$out" .prompt)" in
  *"every linked pull request title contains the exact story ID $id_auto. EXTRA-CLAUSE") : ;;
  *) fail_test "Codex built-in prompt: STORY_PROMPT_EXTRA is not last" ;;
esac

# The public dispatch flag selects the same provider without relying on the
# environment seam, may appear before the id, and outranks that seam when both
# are supplied.
id_flag=$(new_story "$repo_plan" "Codex explicit agent dry run")
out=$(cd "$repo_plan" && PATH="$FAKE_BIN:$PATH" STORY_AGENT=unknown STORY_DRY_RUN=1 \
  bash "$SCRIPT" dispatch --agent=codex "$id_flag" 2>&1)
assert_eq "$(jqf "$out" .agent)" "codex" "agent flag: selected provider"
assert_contains "$(jqf "$out" .worktree_path)" "/.codex/worktrees/" \
  "agent flag: provider worktree"
assert_contains "$(jqf "$out" '.commands|join(" ")')" "send-keys -t <pane> Tab" \
  "agent flag: provider submit key"

# The old environment token remains accepted, but canonical JSON and an
# actionable stderr warning keep every new surface on the shorter token.
id_alias=$(new_story "$repo_plan" "Claude compatibility alias dry run")
alias_err=$(mktemp)
_TMP_REPOS+=("$alias_err")
out=$(cd "$repo_plan" && STORY_AGENT=claude-code STORY_DRY_RUN=1 \
  bash "$SCRIPT" dispatch "$id_alias" 2>"$alias_err")
assert_eq "$(jqf "$out" .agent)" "claude" "legacy env alias: canonical provider"
assert_contains "$(cat "$alias_err")" "deprecated" "legacy env alias: warning"
assert_contains "$(cat "$alias_err")" "STORY_AGENT=claude" "legacy env alias: canonical remedy"

out=$(cd "$repo_plan" && STORY_AGENT=unknown bash "$SCRIPT" list 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "unknown provider: refused"
assert_contains "$(jqf "$out" .display)" "supported agents" "unknown provider: names choices"

finish
