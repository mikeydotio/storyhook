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

# Keep the placeholder alive long enough to prove kill-window, rather than its
# natural timeout, ends the pane process recorded by this fake.
export FAKE_TMUX_PANE_LIFETIME=5

pane_cwd="$(mktemp -d /tmp/story-test-tmux-cwd.XXXXXX)"
_TMP_REPOS+=("$pane_cwd")

occupant() { "$FAKE_TMUX" display-message -p '#{pane_current_command}'; }
pane_pid() { "$FAKE_TMUX" display-message -p '#{pane_pid}'; }

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

finish
