#!/usr/bin/env bash
# SH-859: consent precedes hooks, initialization and exactly one story charter.
source "$(dirname "$0")/lib.sh"
fixture_bin=$(mktemp -d /tmp/story-test-trust-bin.XXXXXX)
_TMP_REPOS+=("$fixture_bin")
ln -s "$TESTS_DIR/fakes/workspace-trust-tmux" "$fixture_bin/tmux"
for provider in claude codex; do
  printf '#!/bin/sh\nexit 0\n' > "$fixture_bin/$provider"
  chmod +x "$fixture_bin/$provider"
done
export WORKSPACE_TRUST_BASE_TMUX="$TESTS_DIR/fakes/tmux"
export WORKSPACE_TRUST_PRETRUSTED=0
repo=$(mk_story_repo WTR)

# Pass mode through the public dispatch API; the provider boundary alone is fake.
launch() {
  (cd "$repo" && PATH="$fixture_bin:$PATH" \
    TMUX="$FAKE_TMUX_STATE/tmux.sock,0,0" TMUX_PANE=%0 STORY_AGENT="$provider" STORY_COUNCIL=off \
    STORY_READY_DELAY=0.1 STORY_READY_ATTEMPTS="$attempts" STORY_CONFIRM_DELAY=0 \
    STORY_PASTE_SETTLE_DELAY=0 FAKE_TMUX_CAPTURE=marker \
    FAKE_TMUX_SUPPRESS_SENTINEL="$suppress_hook" \
    FAKE_TMUX_CODEX_SENTINEL_MODE=identity \
    FAKE_TMUX_SESSION_ID="$session_id" \
    FAKE_TMUX_CODEX_PLUGIN_ROOT="$PLUGIN_ROOT" \
    bash "$SCRIPT" dispatch "$id" "$@")
}
providers=(claude codex)
modes=(auto full-auto resume attended missing-hook)
if [ "$#" = 2 ]; then providers=("$1"); modes=("$2"); fi
for provider in "${providers[@]}"; do
  for mode in "${modes[@]}"; do
    FAKE_TMUX_STATE=$(mktemp -d /tmp/story-test-trust-tmux.XXXXXX)
    export FAKE_TMUX_STATE
    _TMP_REPOS+=("$FAKE_TMUX_STATE")
    id=$(new_story "$repo" "$provider $mode workspace trust")
    session_id="$id-initial"
    flags=(--auto) attempts=18 suppress_hook=0
    case "$mode" in
      full-auto) flags+=(--full-auto) ;;
      attended) flags=(); attempts=3 ;;
      missing-hook) suppress_hook=1 ;;
    esac
    if [ "$mode" = attended ]; then out=$(launch); else out=$(launch "${flags[@]}"); fi
    if [ "$mode" = resume ]; then
      assert_eq "$(jqf "$out" .ok)" true "$provider: initial dispatch before resume"
      PATH="$fixture_bin:$PATH" tmux kill-window -t "$(jqf "$out" .window)"
      session_id="$id-replacement"
      out=$(launch --auto --resume)
      assert_eq "$(jqf "$out" .worktree_reused)" true "$provider: fresh resume retains worktree"
    fi
    case "$mode" in
      attended|missing-hook)
        assert_eq "$(jqf "$out" .ok)" false "$provider $mode: readiness must refuse"
        assert_eq "$(jqf "$out" .reason)" pane-not-ready "$provider $mode: one contextual result"
        expected_submits=0
        if [ "$mode" = attended ]; then
          [ ! -f "$FAKE_TMUX_STATE/trust_keys" ] || fail_test "$provider: attended consent is not automated"
          assert_eq "$(jqf "$out" .trust_phase)" unseen "$provider: attended trust scope stays off"
        else
          assert_eq "$(jqf "$out" .wait_ready_reason)" no-sentinel "$provider: consent cannot replace hook evidence"
          assert_eq "$(jqf "$out" .trust_phase)" complete "$provider: hook refusal preserves completed consent phase"
          [ "$provider" != codex ] || expected_submits=1
        fi
        ;;
      *)
        assert_eq "$(jqf "$out" .ok)" true "$provider $mode: consent unblocks ordinary readiness"
        expected_submits=1
        [ "$provider" != codex ] || expected_submits=2
        assert_contains "$(cat "$FAKE_TMUX_STATE/submitted" 2>/dev/null || true)" "$id" \
          "$provider $mode: last submission is the story charter"
        ;;
    esac
    if [ "$mode" != attended ]; then
      [ -f "$FAKE_TMUX_STATE/trust_accepted" ] || fail_test "$provider $mode: consent missing; $out"
      assert_eq "$(grep -c '^Enter$' "$FAKE_TMUX_STATE/trust_keys" 2>/dev/null || true)" 1 \
        "$provider $mode: confirmation is one-shot"
    fi
    for violation in trust_rejected trust_unsafe_paste trust_premature_key; do
      [ ! -f "$FAKE_TMUX_STATE/$violation" ] || fail_test "$provider $mode: $violation"
    done
    assert_eq "$(cat "$FAKE_TMUX_STATE/prompt_submits")" "$expected_submits" \
      "$provider $mode: exact initialization and charter count"
    PATH="$fixture_bin:$PATH" tmux kill-window -t @1
    printf 'CHECKED: %s %s\n' "$provider" "$mode"
  done
done
finish
