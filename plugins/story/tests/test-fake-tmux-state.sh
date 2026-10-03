#!/usr/bin/env bash
# SH-263: the fake tmux's state directory is NAMED BY THE CALLER, always.
#
# The fake holds every byte of its model -- the input buffer, the `launched`
# flag, the derived `pane_current_command`, the pane pid, the absorb counter --
# in files under one directory, because it is re-exec'd per tmux call and can
# keep nothing in process memory. That directory used to fall back to a FIXED,
# world-writable path (`/tmp/issue-faketmux`) whenever $FAKE_TMUX_STATE was
# unset, and five of this suite's own test files left it unset. They therefore
# shared one directory with each other, with every concurrent run of this suite,
# and with the `issue` plugin's fake of the same name that this one was forked
# from -- a directory that also persisted between runs (its `new_session_calls`
# was found seven hours older than its neighbours, under a 71 KB
# `new_window_args.log` accumulated across runs).
#
# The failure that produced this file: test-dispatch-auto.sh's one real
# fake-tmux dispatch refused at the readiness gate -- "that pane is running
# `zsh`, not a process matching `^(claude|node)$`" -- for a pane it had itself
# launched `claude` into moments earlier. A second user of the shared directory
# is all it takes: that user's `new-window` clears `launched` and `input`, and
# its next Enter is then read as a launch line of nothing at all, which derives
# the fallback shell name and writes it over the first user's occupant. The
# SH-226 gate then correctly refused a pane it correctly observed to hold a
# shell. The gate was right; the fixture lied to it.
#
# Note what does NOT reproduce it: seeding the shared directory with a stale
# `launched=true` before a run. `new-window`'s exec form resets that flag and
# re-derives the occupant unconditionally, so stale state alone is harmless.
# It takes a CONCURRENT second writer, which is exactly what a fixed shared
# path invites and a private one makes impossible.
#
# This file deliberately does not set $FAKE_TMUX_STATE. It is the same shape as
# the five that forgot to, and it inherits whatever lib.sh gives every test --
# which is the fix, and which is what the first case below measures.
source "$(dirname "$0")/lib.sh"

FAKE_TMUX="$TESTS_DIR/fakes/tmux"
LEGACY_SHARED_STATE=/tmp/issue-faketmux

# Use lib.sh's graced lifetime: this proves explicit termination, not expiry.

pane_cwd="$(mktemp -d /tmp/story-test-tmux-cwd.XXXXXX)"
_TMP_REPOS+=("$pane_cwd")

occupant() { "$FAKE_TMUX" display-message -p '#{pane_current_command}'; }
pane_pid() { "$FAKE_TMUX" display-message -p '#{pane_pid}'; }
engine_probe() {
  "$FAKE_TMUX" display-message -p -t %1 \
    '#{pane_pid}\t#{pane_current_command}\t#{pane_dead}\t#{window_activity}'
}

# --- the field failure, reproduced ----------------------------------------
#
# A dispatch's own launch, then a second fake-tmux user that never named a
# state directory. Whatever that second user does, it must not be able to
# reach this pane: it did not say where this pane's state lives, so it cannot
# have been handed it.
"$FAKE_TMUX" new-window -d -P -F '#{pane_id}' -n TST-1 -c "$pane_cwd" \
  'claude --permission-mode plan' ';' set-window-option -t @1 remain-on-exit on >/dev/null
assert_eq "$(occupant)" "claude" "the launch's own occupant is claude"
launched_pid="$(pane_pid)"
[ -n "$launched_pid" ] || fail_test "the launch recorded no pane pid"
# A delayed observer must still see a live pane for the explicit kill proof.
# This outlives the retired local five-second lifetime (SH-819).
retired_lifetime=5
sleep "$((retired_lifetime + 1))"
# The fourth field is the window's last-output stamp (SH-657): a freshly
# opened window answers "now" unless a test planted an older time.
printf '1789066115' > "$FAKE_TMUX_STATE/window_activity"
expected_engine_probe="$(printf '%s\tclaude\t0\t1789066115' "$launched_pid")"
assert_eq "$(engine_probe)" "$expected_engine_probe" \
  "the engine liveness probe receives every requested pane field"
rm -f "$FAKE_TMUX_STATE/window_activity"
case "$(engine_probe)" in
  *$'\t'0$'\t'[0-9]*) ;;
  *) fail_test "without a planted stamp the fourth field is the current unix time: $(engine_probe)" ;;
esac

env -u FAKE_TMUX_STATE "$FAKE_TMUX" new-window -d -P -F '#{pane_id}' \
  -n probe -c "$pane_cwd" >/dev/null 2>&1
env -u FAKE_TMUX_STATE "$FAKE_TMUX" send-keys -t %1 Enter >/dev/null 2>&1

assert_eq "$(occupant)" "claude" \
  "a second fake-tmux user cannot rewrite this pane's occupant"
assert_eq "$(pane_pid)" "$launched_pid" \
  "a second fake-tmux user cannot kill this pane's process"

# --- kill-window ends the pane process ------------------------------------
#
# Real tmux tears down a window's panes. This fake's process model must do the
# same: callers use kill-window as the ownership boundary before deleting the
# fixture paths that a dispatched pane can still touch.
kill -0 "$launched_pid" 2>/dev/null \
  || fail_test "the pane placeholder exited before kill-window exercised it"
"$FAKE_TMUX" kill-window -t @1 >/dev/null
for _attempt in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
  kill -0 "$launched_pid" 2>/dev/null || break
  sleep 0.05
done
if kill -0 "$launched_pid" 2>/dev/null; then
  fail_test "kill-window left pane placeholder $launched_pid alive"
fi
assert_eq "$(pane_pid)" "" "kill-window clears the dead pane's pid record"

# --- the fake refuses to invent a state directory -------------------------
#
# Refusal rather than a default is the whole repair: a fake that silently
# invents a shared path where a private one was meant fails the way this
# harness exists to prevent -- quietly, in another test's assertions, hours
# later, on a change that touched none of it.
out="$(env -u FAKE_TMUX_STATE "$FAKE_TMUX" display-message -p '#{window_id}' 2>&1)"
rc=$?
[ "$rc" -ne 0 ] || fail_test "the fake ran with no \$FAKE_TMUX_STATE (exit $rc)"
assert_contains "$out" "FAKE_TMUX_STATE" \
  "the refusal names the variable that was not set"

# ...and it stores state, it does not own it: a directory the caller never
# created is a mistake worth failing on, not one to paper over with `mkdir -p`.
missing="$pane_cwd/never-created"
out="$(FAKE_TMUX_STATE="$missing" "$FAKE_TMUX" display-message -p '#{window_id}' 2>&1)"
rc=$?
[ "$rc" -ne 0 ] || fail_test "the fake ran against a state directory that does not exist (exit $rc)"
[ ! -d "$missing" ] || fail_test "the fake created the state directory it was refusing"

# --- every test gets its own, from lib.sh ---------------------------------
#
# In lib.sh rather than in each test file, for the reason the data-home block
# above it is: five files forgot, and a fixture you can forget is one that will
# be forgotten again. Two independent sourcings must never land in one
# directory -- that IS the concurrent-runs case, minus the timing.
[ -n "${FAKE_TMUX_STATE:-}" ] || fail_test "lib.sh left \$FAKE_TMUX_STATE unset"
[ -d "${FAKE_TMUX_STATE:-}" ] || fail_test "lib.sh's \$FAKE_TMUX_STATE is not a directory"
case "${FAKE_TMUX_STATE:-}" in
  "$LEGACY_SHARED_STATE" | "$LEGACY_SHARED_STATE"/*)
    fail_test "lib.sh handed out the shared path this story retired" ;;
  /tmp/* | /private/tmp/*) : ;;
  *) fail_test "lib.sh's \$FAKE_TMUX_STATE [${FAKE_TMUX_STATE:-}] is not under /tmp" ;;
esac

mint() {
  env -u FAKE_TMUX_STATE bash -c \
    'source "$1/lib.sh"; printf "%s|%s" "$FAKE_TMUX_STATE" "$([ -d "$FAKE_TMUX_STATE" ] && printf dir)"' \
    _ "$TESTS_DIR"
}
first="$(mint)"
second="$(mint)"
assert_contains "$first" "|dir" "a sourced lib.sh mints a state directory that exists"
[ "${first%|*}" != "${second%|*}" ] \
  || fail_test "two independent test files were handed the same state directory [${first%|*}]"

# --- the placeholder's lifetime is graced, not an idle-machine 30 s ----------
#
# Two gates lost pane-probing tests to the fake's bare 30 s self-expiry: SH-760
# (test-dispatch-pane-readiness.sh) and SH-792 (test-notify.sh and
# test-notify-registered-session.sh read `pane-dead`, the exact signature a
# FAKE_TMUX_PANE_LIFETIME=1 run reproduces). lib.sh grants every test the
# contention-graced lifetime scripts/tests/load_grace.py computes, and leaves a
# lifetime a test chose for itself alone.
lifetime_after_lib() {
  env "$@" bash -c 'source "$1/lib.sh"; printf "%s" "${FAKE_TMUX_PANE_LIFETIME:-}"' _ "$TESTS_DIR" 2>/dev/null
}
graced="$(lifetime_after_lib -u FAKE_TMUX_PANE_LIFETIME)"
case "$graced" in
  '' | *[!0-9]*) fail_test "a sourced lib.sh exported no whole-second pane lifetime [$graced]" ;;
  *)
    [ "$graced" -ge 30 ] \
      || fail_test "a sourced lib.sh graced the pane lifetime below the fake's idle 30 s [$graced]"
    ;;
esac
assert_eq "$(lifetime_after_lib FAKE_TMUX_PANE_LIFETIME=7)" "7" \
  "a lifetime the test set itself survives sourcing lib.sh"

# --- the fixed default cannot come back -----------------------------------
#
# Structural, not behavioural: the refusal above proves today's fake has no
# default, and this proves no future edit can reintroduce one without saying so
# here first.
# Comment lines are stripped first: the fake's header records what the shared
# path WAS and why it went, which is history worth keeping and not a default
# anything can fall back into. What must never reappear is an executable line
# naming it, or any value substituted in for an unset $FAKE_TMUX_STATE.
code="$(grep -v '^[[:space:]]*#' "$FAKE_TMUX")"
case "$code" in
  *issue-faketmux*) fail_test "the fake names the shared path this story retired" ;;
esac
if printf '%s\n' "$code" | grep -Eq 'FAKE_TMUX_STATE:-[^}]'; then
  fail_test "the fake substitutes a default for \$FAKE_TMUX_STATE again"
fi

# --- nothing in this suite opts out of lib.sh -----------------------------
#
# Derived over the directory rather than listed here: a hand-maintained list of
# the files that must source lib.sh is a list that drifts, and every isolation
# this harness has -- the data home, the daemon address, and now the fake's
# state -- reaches a test file through that one source line.
for t in "$TESTS_DIR"/test-*.sh; do
  grep -Eq '^[[:space:]]*(source|\.)[[:space:]].*/lib\.sh' "$t" \
    || fail_test "$(basename "$t") does not source lib.sh, so it inherits no isolation"
done

# Client flags have no order requirement. Native and protected callers place
# -u after -S, while ordinary Python callers place it before -S.
selector_state=$(mktemp -d /tmp/story-test-selectors.XXXXXX)
_TMP_REPOS+=("$selector_state")
selector_state=$(cd "$selector_state" && pwd -P)
for order in before after protected; do
  case "$order" in
    before) flags=(-u -S "$selector_state/tmux.sock") ;;
    after) flags=(-S "$selector_state/tmux.sock" -u) ;;
    protected) flags=(-N -S "$selector_state/tmux.sock" -u) ;;
  esac
  observed=$(FAKE_TMUX_STATE="$selector_state" "$FAKE_TMUX" "${flags[@]}" display-message -p '#{socket_path}')
  assert_eq "$observed" "$selector_state/tmux.sock" "$order: client flags preserve selected socket"
done

# --- the default tmux server is always the test's own (SH-840) -------------
#
# A caller outside tmux is pointed at $TMUX_TMPDIR/tmux-<uid>/default, and an
# unset TMUX_TMPDIR means the operator's real server under /tmp. Every lib.sh
# instance keeps it inside the test home: the one that owns the home, and a
# nested one under a harness that cleared its environment, which is how
# tests/support/protect_domain.rs came to ask the machine's real server.
tmpdir_after_lib() {
  env "$@" bash -c 'source "$1/lib.sh"; printf "%s" "$TMUX_TMPDIR"' _ "$TESTS_DIR" 2>/dev/null
}
for inherited in unset outside; do
  case "$inherited" in
    unset) observed=$(tmpdir_after_lib -u TMUX_TMPDIR) ;;
    outside) observed=$(tmpdir_after_lib TMUX_TMPDIR=/tmp) ;;
  esac
  case "$observed" in
    "$STORYHOOK_TEST_HOME"/*) ;;
    *) fail_test "a nested lib.sh with TMUX_TMPDIR $inherited left the default tmux server outside the test home [$observed]" ;;
  esac
done
assert_eq "$(tmpdir_after_lib)" "$TMUX_TMPDIR" \
  "a nested lib.sh keeps the home-local TMUX_TMPDIR its caller chose"

finish
