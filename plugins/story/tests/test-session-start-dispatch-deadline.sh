#!/usr/bin/env bash
# SH-544: session-start.sh gives a DISPATCHED pane's own `story session-start`
# call a much larger --deadline than an ordinary human-launched session gets,
# because story.sh's dispatch readiness gate (READY_ATTEMPTS * READY_DELAY,
# tests/dispatch_ready_budget.rs) is already prepared to poll for tens of
# seconds while an ordinary `claude` launch should never make a person wait
# on a slow daemon.
#
# This deliberately stubs `story` on PATH rather than using the real binary
# (unlike most of this suite, lib.sh's own header explains why the real
# binary is normally preferred): the thing under test is session-start.sh's
# OWN branching on STORYHOOK_DISPATCH, not any dispatch/CAS semantics a real
# `story` would add — a stub that just records its own argv is the narrowest
# double that answers the one question this file asks.
source "$(dirname "$0")/lib.sh"

HOOK="$PLUGIN_ROOT/hooks/session-start.sh"

# stub_story <dir> — writes a fake `story` into <dir> that appends its own
# argv (one flag per captured line) to <dir>/calls.log and answers `{}`, so
# session-start.sh's own `case "$out" in "{"*)` branch is satisfied and the
# hook completes normally regardless of which deadline it chose.
stub_story() {
  local dir="$1"
  cat >"$dir/story" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$(dirname "$0")/calls.log"
cat >/dev/null # drain stdin, matching the real CLI's own stdin-passthrough seam
printf '{}'
EOF
  chmod +x "$dir/story"
}

STUB_DIR="$(mktemp -d /tmp/story-test-stub.XXXXXX)"
_TMP_REPOS+=("$STUB_DIR")
stub_story "$STUB_DIR"

run_hook() {
  (
    cd "$STUB_DIR" \
      && PATH="$STUB_DIR:$PATH" env -u STORYHOOK_DISPATCH "$@" bash "$HOOK" <<<'{"cwd":"'"$STUB_DIR"'"}' >/dev/null
  )
}

# ---- ordinary (non-dispatched) session: the small, human-facing deadline ---
rm -f "$STUB_DIR/calls.log"
run_hook
assert_contains "$(cat "$STUB_DIR/calls.log")" "--deadline 3 session-start" \
  "an ordinary session asks for the small deadline"
case "$(cat "$STUB_DIR/calls.log")" in
  *"--deadline 20"*) fail_test "an ordinary session must never get the dispatch-only deadline" ;;
  *) : ;;
esac

# ---- dispatched pane: the widened deadline ---------------------------------
rm -f "$STUB_DIR/calls.log"
run_hook STORYHOOK_DISPATCH=1
assert_contains "$(cat "$STUB_DIR/calls.log")" "--deadline 20 session-start" \
  "a dispatched pane asks for the widened deadline"
case "$(cat "$STUB_DIR/calls.log")" in
  *"--deadline 3 "*) fail_test "a dispatched pane must not fall back to the small deadline" ;;
  *) : ;;
esac

# ---- an empty STORYHOOK_DISPATCH is the same as unset ----------------------
rm -f "$STUB_DIR/calls.log"
run_hook STORYHOOK_DISPATCH=
assert_contains "$(cat "$STUB_DIR/calls.log")" "--deadline 3 session-start" \
  "an empty STORYHOOK_DISPATCH (the marker's own unset-dispatch spelling) stays small"

finish
