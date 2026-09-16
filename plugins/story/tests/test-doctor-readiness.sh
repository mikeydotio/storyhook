#!/usr/bin/env bash
# SH-736: a healthy screen cannot certify an absent SessionStart hook.
source "$(dirname "$0")/lib.sh"
bins=$(mktemp -d /tmp/story-test-doctor-bins.XXXXXX)
_TMP_REPOS+=("$bins")
printf '#!/bin/sh\nexit 0\n' > "$bins/claude"
cp "$bins/claude" "$bins/codex"
chmod +x "$bins/claude" "$bins/codex"
repo=$(mk_story_repo)
mkdir -p "$repo/.claude"
printf '%s' '{"session_id":"old-session","protocol_version":2}' > "$repo/.claude/dispatch-sentinel.json"
original=$(cat "$repo/.claude/dispatch-sentinel.json")

doctor_case() {
  export FAKE_TMUX_STATE
  FAKE_TMUX_STATE=$(mktemp -d /tmp/story-test-doctor-state.XXXXXX)
  _TMP_REPOS+=("$FAKE_TMUX_STATE")
  out=$(cd "$repo" && PATH="$bins:$TESTS_DIR/fakes:$PATH" \
    TMUX=fake TMUX_PANE=%0 STORY_READY_ATTEMPTS=3 STORY_READY_DELAY=0 \
    STORY_READY_FALLBACK_DELAY=0 STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 \
    FAKE_TMUX_CAPTURE=marker env "$@" bash "$SCRIPT" doctor)
  assert_eq "$(cat "$repo/.claude/dispatch-sentinel.json")" "$original" "doctor preserves caller evidence"
}

doctor_case FAKE_TMUX_SUPPRESS_SENTINEL=1
assert_eq "$(jqf "$out" .ok)" false "marker-only doctor must fail"
assert_eq "$(jqf "$out" .terminal_readiness_confirmed)" true "terminal still works"
assert_eq "$(jqf "$out" .readiness_confirmed)" false "missing hook cannot certify dispatch"
assert_eq "$(jqf "$out" .wait_ready_reason)" no-sentinel "doctor names missing hook"

doctor_case
assert_eq "$(jqf "$out" .ok)" true "fresh hook and context certify readiness"
assert_eq "$(jqf "$out" .sentinel_confirmed)" true "doctor checked the sentinel"
assert_eq "$(jqf "$out" .context_status)" loaded "context was loaded"
assert_eq "$(cat "$FAKE_TMUX_STATE/prompt_submits")" 0 "Claude doctor submits no prompt"
probe_cwd=$(cat "$FAKE_TMUX_STATE/window_cwd")
[ ! -e "$probe_cwd" ] || fail_test "doctor leaked its scratch checkout"

doctor_case FAKE_TMUX_CLAUDE_SENTINEL_ROOT="$bins"
assert_eq "$(jqf "$out" .ok)" false "foreign package cannot certify dispatch"
assert_eq "$(jqf "$out" .wait_ready_reason)" hook-identity-mismatch "doctor names foreign package"

doctor_case FAKE_TMUX_FAIL_KILL_PANE=1
assert_eq "$(jqf "$out" .ok)" false "cleanup failure cannot produce all-green"
assert_eq "$(jqf "$out" .cleanup.ok)" false "cleanup failure is explicit"
retained=$(jqf "$out" .preserved_path)
_TMP_REPOS+=("$retained")
[ -d "$retained" ] || fail_test "uncertain cleanup must retain scratch checkout"

doctor_case FAKE_TMUX_EXIT_ON_REPROBE=1
assert_eq "$(jqf "$out" .readiness_confirmed)" false "exited provider is never ready"
assert_eq "$(jqf "$out" .wait_ready_reason)" pid-exited "doctor names exited provider"
assert_eq "$(jqf "$out" .cleanup.ok)" false "unverifiable process does not grant cleanup authority"
retained=$(jqf "$out" .preserved_path)
_TMP_REPOS+=("$retained")
[ -d "$retained" ] || fail_test "lost process authority must retain scratch checkout"

doctor_case STORY_AGENT=codex FAKE_TMUX_CODEX_SENTINEL_MODE=identity FAKE_TMUX_CODEX_PLUGIN_ROOT="$PLUGIN_ROOT"
assert_eq "$(jqf "$out" .ok)" true "Codex doctor completes production bootstrap"
assert_eq "$(cat "$FAKE_TMUX_STATE/prompt_submits")" 1 "Codex doctor submits only initialization"

doctor_case STORY_AGENT=codex FAKE_TMUX_CODEX_SENTINEL_MODE=identity FAKE_TMUX_CODEX_PLUGIN_ROOT="$PLUGIN_ROOT" FAKE_TMUX_BOOTSTRAP_INCOMPLETE=1
assert_eq "$(jqf "$out" .ok)" false "unfinished bootstrap is not ready"
assert_eq "$(jqf "$out" .wait_ready_reason)" bootstrap-incomplete "doctor reports bootstrap failure"

# Force the actual CLI pre-RPC boundary only for the hook, preserving all
# other project/integrity operations and the production fallback writer.
export STORY_REAL_BIN
STORY_REAL_BIN=$(command -v story)
export SH736_FAIL_STATE="$bins/state-is-a-file"
printf blocked > "$SH736_FAIL_STATE"
cat > "$bins/story" <<'WRAPPER'
#!/usr/bin/env bash
case " $* " in
  *' session-start '*) XDG_STATE_HOME="$SH736_FAIL_STATE" exec "$STORY_REAL_BIN" "$@" ;;
  *) exec "$STORY_REAL_BIN" "$@" ;;
esac
WRAPPER
chmod +x "$bins/story"
doctor_case
assert_eq "$(jqf "$out" .readiness_confirmed)" true "degraded hook proves execution"
assert_eq "$(jqf "$out" .context_status)" unavailable "doctor reports degraded context"
assert_eq "$(jqf "$out" .ok)" false "degraded context cannot produce an all-green report"
mkdir -p "$repo/.storyhook"
ln -s absent "$repo/.storyhook/plugin-config.toml"
doctor_case
assert_eq "$(jqf "$out" .ok)" false "broken legacy settings cannot become enabled defaults"
assert_contains "$out" 'cannot copy plugin configuration' "doctor preserves configuration failure"
[ ! -f "$FAKE_TMUX_STATE/window_cwd" ] || fail_test "unreadable settings must refuse before launching a probe"
finish
