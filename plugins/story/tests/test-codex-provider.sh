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
  unset FAKE_TMUX_LAUNCH_MANGLE FAKE_TMUX_FAIL_SEND_KEYS FAKE_TMUX_CAPTURE
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
[ ! -e "$repo/.codex/worktrees/$id/.claude/dispatch-sentinel.json" ] \
  || fail_test "dispatch: fake published a Claude-only sentinel in Codex"
grep -q '^\.codex/worktrees/$' "$repo/.gitignore" \
  || fail_test "dispatch: Codex worktree container was not ignored"
assert_contains "$(cat "$FAKE_TMUX_STATE/submitted")" "story show $id" \
  "dispatch: charter reached Codex as one submitted block"

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
  "codex --no-alt-screen -c check_for_update_on_startup=false" \
  "dry auto: Codex launch suppresses the managed child's update chooser"
assert_contains "$(jqf "$out" '.commands|join(" ")')" "send-keys -t <pane> Tab" \
  "dry auto: Tab submission"

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
