#!/usr/bin/env bash
# SH-570 -- Claude plan approval survives bounded tmux/TUI races without
# typing into a pane whose original process is no longer known to be alive.
set -uo pipefail

TESTS_DIR="$(cd "$(dirname "$0")" && pwd)"
HOOK="$TESTS_DIR/../hooks/full-auto.sh"
TMUX_FIXTURE="$(mktemp -d /tmp/story-test-claude-plan-resilience.XXXXXX)"
trap 'rm -rf -- "$TMUX_FIXTURE"' EXIT

cat >"$TMUX_FIXTURE/tmux" <<'FAKE_TMUX'
#!/usr/bin/env bash
set -uo pipefail

target_after_t() {
  local previous="" argument
  for argument in "$@"; do
    if [ "$previous" = -t ]; then
      printf '%s' "$argument"
      return 0
    fi
    previous="$argument"
  done
  return 1
}

next_count() {
  local pane="$1" name="$2" key file count=0
  key="${pane#%}"
  file="$FULL_AUTO_TMUX_STATE/$key-$name-count"
  [ ! -f "$file" ] || read -r count <"$file"
  count=$((count + 1))
  printf '%s' "$count" >"$file"
  printf '%s' "$count"
}

read_count() {
  local pane="$1" name="$2" file
  file="$FULL_AUTO_TMUX_STATE/${pane#%}-$name-count"
  REPLY=0
  [ ! -f "$file" ] || read -r REPLY <"$file"
}

pane=$(target_after_t "$@") || exit 1
scenario="${FULL_AUTO_TMUX_SCENARIO:-normal}"

case "${1:-}" in
  display-message)
    count=$(next_count "$pane" identity)
    read_count "$pane" send
    send_count="$REPLY"
    case "$scenario" in
      transient-identity) [ "$count" -gt 1 ] || exit 1 ;;
      transient-post-send-identity)
        [ "$send_count" -eq 0 ] || [ "$count" -ne 3 ] || exit 1
        ;;
      pre-send-identity-exhaustion) [ $((count % 2)) -ne 0 ] || exit 1 ;;
      identity-exhaustion) exit 1 ;;
      unknown-identity) printf 'unknown\n'; exit 0 ;;
      initial-replaced) printf '778:0\n'; exit 0 ;;
      initial-dead) printf '777:1\n'; exit 0 ;;
      replace-before-send) [ "$count" -lt 2 ] || { printf '778:0\n'; exit 0; } ;;
      die-before-send) [ "$count" -lt 2 ] || { printf '777:1\n'; exit 0; } ;;
      replace-after-send) [ "$send_count" -eq 0 ] || { printf '778:0\n'; exit 0; } ;;
    esac
    printf '777:0\n'
    ;;
  capture-pane)
    count=$(next_count "$pane" capture)
    read_count "$pane" send
    send_count="$REPLY"
    case "$scenario" in
      transient-capture) [ "$count" -gt 1 ] || exit 1 ;;
      transient-post-send-capture)
        [ "$send_count" -eq 0 ] || [ "$count" -ne 2 ] || exit 1
        ;;
      capture-exhaustion) exit 1 ;;
    esac
    if [ "$scenario" = changed ] || { [ "$scenario" = changed-then-exact ] && [ "$count" -eq 1 ]; }; then
      printf 'Provider changed this prompt\n1. Continue\n'
    elif [ -f "$FULL_AUTO_TMUX_STATE/${pane#%}-accepted" ]; then
      printf 'Implementation started\n'
    else
      printf '%s\n' \
        'Ready to code?' \
        '❯ 1. Yes, and use auto mode' \
        '  2. Yes, manually approve edits'
    fi
    ;;
  send-keys)
    count=$(next_count "$pane" send)
    printf '%s\n' "$*" >>"$FULL_AUTO_TMUX_STATE/${pane#%}-send-keys.log"
    case "$scenario" in
      transient-send)
        [ "$count" -gt 1 ] || exit 1
        ;;
      send-exhaustion)
        exit 1
        ;;
      absorbed-send)
        [ "$count" -gt 1 ] || exit 0
        ;;
    esac
    : >"$FULL_AUTO_TMUX_STATE/${pane#%}-accepted"
    ;;
  *) exit 1 ;;
esac
FAKE_TMUX
chmod +x "$TMUX_FIXTURE/tmux"

cat >"$TMUX_FIXTURE/sleep" <<'FAKE_SLEEP'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$FULL_AUTO_TMUX_STATE/sleep.log"
FAKE_SLEEP
chmod +x "$TMUX_FIXTURE/sleep"

failures=0

value() {
  local pane="$1" name="$2" file
  file="$TMUX_FIXTURE/${pane#%}-$name-count"
  [ ! -f "$file" ] || cat "$file"
}

send_log() {
  local pane="$1" file
  file="$TMUX_FIXTURE/${pane#%}-send-keys.log"
  [ ! -f "$file" ] || cat "$file"
}

assert_eq() {
  local actual="$1" expected="$2" label="$3"
  if [ "$actual" != "$expected" ]; then
    printf 'FAIL: %s\n  expected: [%s]\n  actual:   [%s]\n' \
      "$label" "$expected" "$actual" >&2
    failures=$((failures + 1))
  fi
}

reset_case() {
  rm -f -- "$TMUX_FIXTURE"/*-count "$TMUX_FIXTURE"/*-send-keys.log \
    "$TMUX_FIXTURE"/*-accepted "$TMUX_FIXTURE/sleep.log"
}

run_watcher() {
  local scenario="$1" pane="${2:-%4242}" expected_pid="${3:-777}" limit="${4:-0}"
  FULL_AUTO_TMUX_STATE="$TMUX_FIXTURE" \
  FULL_AUTO_TMUX_SCENARIO="$scenario" \
  STORYHOOK_AUTO=SH-570 \
  PATH="$TMUX_FIXTURE:$PATH" \
    bash "$HOOK" --approve-claude-plan "$pane" "$expected_pid" "$limit"
}

# Exact dialog: one accepted Return, followed by an observed transition.
reset_case
run_watcher normal
assert_eq "$(value %4242 send)" 1 "exact dialog sends Return once"
assert_eq "$(send_log %4242)" 'send-keys -t %4242 Enter' "Return targets the original pane"
assert_eq "$(value %4242 capture)" 2 "success is acknowledged by recapturing the transitioned UI"

# Unknown UI is fail-closed but remains eligible for a later exact dialog.
reset_case
run_watcher changed-then-exact %4242 777 2
assert_eq "$(value %4242 send)" 1 "an initially unknown screen is polled until the exact dialog appears"
reset_case
run_watcher changed %4242 777 2
assert_eq "$(send_log %4242)" "" "a changed dialog receives no input"

# A single failed observation is transport noise, not pane completion.
for scenario in transient-identity transient-capture; do
  reset_case
  run_watcher "$scenario"
  assert_eq "$(value %4242 send)" 1 "$scenario is retried and eventually approves"
done

# A post-Return observation failure retains the pending acknowledgement. Once
# observation recovers, the transitioned UI completes without another Return.
reset_case
run_watcher transient-post-send-identity %4242 777 2
assert_eq "$(value %4242 send)" 1 "post-send identity recovery does not resend after transition"
assert_eq "$(value %4242 identity)" 4 "post-send identity is retried against the original PID"
reset_case
run_watcher transient-post-send-capture %4242 777 2
assert_eq "$(value %4242 send)" 1 "post-send capture recovery does not resend after transition"
assert_eq "$(value %4242 capture)" 3 "post-send capture is retried before acknowledgement"

# Observation failures are bounded to three consecutive attempts.
reset_case
run_watcher identity-exhaustion
assert_eq "$(value %4242 identity)" 3 "identity failures stop after three attempts"
assert_eq "$(send_log %4242)" "" "identity failure exhaustion sends no input"
reset_case
run_watcher pre-send-identity-exhaustion
assert_eq "$(value %4242 identity)" 6 "pre-send identity failures stop after three complete observations"
assert_eq "$(send_log %4242)" "" "pre-send identity failure exhaustion sends no input"
reset_case
run_watcher capture-exhaustion
assert_eq "$(value %4242 capture)" 3 "capture failures stop after three attempts"
assert_eq "$(send_log %4242)" "" "capture failure exhaustion sends no input"

# Both a rejected Return and an accepted-but-absorbed Return are retried only
# while the exact dialog remains on the same original process.
for scenario in transient-send absorbed-send; do
  reset_case
  run_watcher "$scenario"
  assert_eq "$(value %4242 send)" 2 "$scenario retries Return after settling and re-observing"
  assert_eq "$(value %4242 capture)" 3 "$scenario confirms the dialog transition after retry"
done
reset_case
run_watcher send-exhaustion
assert_eq "$(value %4242 send)" 3 "Return invocations stop after three attempts including failures"
assert_eq "$(value %4242 capture)" 4 "each failed Return is followed by a settled re-observation"

# A known replacement or death is definitive. An unreadable identity is
# bounded transport failure. None can authorize input.
for scenario in initial-replaced initial-dead replace-before-send die-before-send; do
  reset_case
  run_watcher "$scenario"
  assert_eq "$(send_log %4242)" "" "$scenario receives no input"
done
reset_case
run_watcher unknown-identity
assert_eq "$(send_log %4242)" "" "unknown-identity receives no input"
assert_eq "$(value %4242 identity)" 1 "a successful unknown identity fails closed immediately"
reset_case
run_watcher replace-after-send
assert_eq "$(value %4242 send)" 1 "a replacement observed after Return receives no retry"

# Malformed internal arguments are inert at the tmux boundary.
reset_case
run_watcher normal not-a-pane 777
run_watcher normal %4242 not-a-pid
assert_eq "$(value %4242 identity)" "" "invalid pane/PID arguments never contact tmux"

if [ "$failures" -ne 0 ]; then
  printf 'FAIL: %s Claude plan approval resilience assertion(s) failed.\n' "$failures" >&2
  exit 1
fi

printf 'PASS\n'
