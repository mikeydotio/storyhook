#!/usr/bin/env bash
# Prove the real fixture CLI stays out of inherited fallback data homes.
#
# Comparing every pathname in the developer's live data tree attributed any
# concurrent writer to this fixture. A browser screencast made that assertion
# fail even though this probe starts only local Git and story commands. Those
# snapshots could not identify the writer, or detect changed database bytes.
# Use wholly owned surrogate fallback homes instead: actual shared isolation
# must redirect away from them, and names AND bytes must remain unchanged.
# This tests the environment routing contract, not a sandbox for hard-coded
# paths or an attribution claim about arbitrary live developer activity.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

assert_isolated_paths() {
  local var value
  for var in HOME XDG_DATA_HOME XDG_CONFIG_HOME XDG_STATE_HOME STORYHOOK_DATA_DIR STORYHOOK_STORE_PATH; do
    value="${!var:-}"
    case "$value" in
      "$STORYHOOK_TEST_HOME"/*) : ;;
      *) fail_test "\$$var must remain in this fixture's owned home" ;;
    esac
  done
  case "$STORYHOOK_TEST_HOME" in
    /tmp/storyhook-plugin-home.* | /private/tmp/storyhook-plugin-home.*) : ;;
    *) fail_test "the fixture home must be a private /tmp plugin home" ;;
  esac
  [ "${_STORYHOOK_OWNS_TEST_HOME:-0}" = 1 ] \
    || fail_test "this probe must own its home and daemon, not inherit outer ownership"
  [ "${STORYHOOK_PARENT_PID:-}" = "$$" ] \
    || fail_test "the probe must own its daemon parent contract"
  [ -n "${STORYHOOK_REAL_HOME:-}" ] && [ "$HOME" != "$STORYHOOK_REAL_HOME" ] \
    || fail_test "the fixture HOME must differ from the inherited fallback HOME"
}

if [ "${1:-}" = --owned-probe ]; then
  receipt="${2:?owned receipt directory required}"
  # If stop cannot establish settlement, do not let lib.sh remove possible
  # live roots. The parent retains its receipt/fallback root on child failure.
  settle_probe_daemon() {
    local status="$1"
    if ! story daemon stop --force >/dev/null 2>&1 \
      || ! python3 -B "$TESTS_DIR/data-home-snapshot.py" settled "$STORYHOOK_TEST_HOME"; then
      trap - EXIT
      printf 'cannot settle owned probe daemon; retaining home %s and receipt %s\n' \
        "$STORYHOOK_TEST_HOME" "$receipt" >&2
      exit 1
    fi
    printf '%s\n' settled >"$receipt/settled" || { trap - EXIT; exit 1; }
    _STORYHOOK_OWNS_TEST_HOME=0
    return "$status"
  }
  trap 'settle_probe_daemon "$?"; _cleanup' EXIT
  assert_isolated_paths
  # Do not start native fixture work after an isolation assertion failed.
  [ "$_FAILED" -eq 0 ] || finish
  repo="$(mk_story_repo)"
  id="$(new_story "$repo" "owned isolation probe")"
  assert_contains "$id" "TST-" "the real CLI must mint the fixture story"
  shown="$(cd "$repo" && story show "$id" --json)"
  assert_eq "$(printf '%s' "$shown" | jq -r '.story.story.title')" \
    "owned isolation probe" "the real CLI must read its created story"
  [ -f "$STORYHOOK_STORE_PATH" ] || fail_test "the isolated store must exist"
  case "$repo" in
    /tmp/story-test.* | /private/tmp/story-test.*) : ;;
    *) fail_test "the real project must be in its owned scratch repository" ;;
  esac
  jq -n --arg id "$id" --arg home "$STORYHOOK_TEST_HOME" \
    --arg store "$STORYHOOK_STORE_PATH" \
    '{id:$id, home:$home, store:$store}' >"$receipt/probe.json"
  finish
fi

assert_isolated_paths
[ "$_FAILED" -eq 0 ] || finish
# Not registered for deletion until the child confirms settlement. Interruption
# or uncertainty leaves a named owned root instead of deleting borrowed paths.
guard="$(mktemp -d /tmp/story-test.data-home.XXXXXX)" || exit 1
fallback="$guard/inherited"
mkdir -p "$guard/receipt"
for directory in home/.local/share/storyhook xdg-data/storyhook xdg-config xdg-state explicit-data explicit-store; do
  mkdir -p "$fallback/$directory"
  printf '%s\n' 'unchanged fallback sentinel' >"$fallback/$directory/canary"
done
snapshot() {
  python3 -B "$TESTS_DIR/data-home-snapshot.py" snapshot "$fallback" >"$1"
}
snapshot "$guard/before.json" || exit 1
# Only this child's inherited inputs are contaminated. It must establish fresh
# lib.sh ownership; no outer test-home, daemon PID or fake tmux state is adopted.
if ! env -u STORYHOOK_TEST_HOME -u _STORYHOOK_OWNS_TEST_HOME \
  -u STORYHOOK_PARENT_PID -u STORYHOOK_PARENT_START_TIME -u FAKE_TMUX_STATE \
  -u TMUX -u TMUX_PANE -u TMUX_TMPDIR \
  HOME="$fallback/home" STORYHOOK_REAL_HOME="$fallback/home" \
  XDG_DATA_HOME="$fallback/xdg-data" XDG_CONFIG_HOME="$fallback/xdg-config" \
  XDG_STATE_HOME="$fallback/xdg-state" STORYHOOK_DATA_DIR="$fallback/explicit-data" \
  STORYHOOK_STORE_PATH="$fallback/explicit-store/store.db" \
  bash "$TESTS_DIR/test-data-home-isolation.sh" --owned-probe "$guard/receipt"; then
  fail_test "owned CLI probe failed; retained evidence at $guard"
  finish
fi
[ "$(cat "$guard/receipt/settled" 2>/dev/null)" = settled ] \
  || { fail_test "probe settlement receipt missing; retained evidence at $guard"; finish; }
assert_contains "$(jq -r .id "$guard/receipt/probe.json")" "TST-" "owned probe receipt"
probe_home="$(jq -r .home "$guard/receipt/probe.json")"
[ ! -e "$probe_home" ] || fail_test "settled child fixture home was not cleaned"
snapshot "$guard/after.json" || exit 1
if ! cmp -s "$guard/before.json" "$guard/after.json"; then
  fail_test "the real fixture CLI changed an owned inherited fallback tree; evidence at $guard"
fi

# Negative controls use this exact comparator on the same owned tree. A new
# path AND an in-place byte edit must be observable; no real-home mutation.
printf '%s\n' added >"$fallback/explicit-data/added-by-control"
snapshot "$guard/added.json" || exit 1
if cmp -s "$guard/after.json" "$guard/added.json"; then
  fail_test "fallback detector missed a new path"
fi
rm "$fallback/explicit-data/added-by-control"
printf '%s\n' 'corrupted fallback sentinel' >"$fallback/explicit-data/canary"
snapshot "$guard/modified.json" || exit 1
if cmp -s "$guard/after.json" "$guard/modified.json"; then
  fail_test "fallback detector missed a content-only mutation"
fi
printf '%s\n' 'unchanged fallback sentinel' >"$fallback/explicit-data/canary"
snapshot "$guard/restored.json" || exit 1
cmp -s "$guard/after.json" "$guard/restored.json" \
  || fail_test "negative controls did not restore the owned fallback tree"
if [ "$_FAILED" -eq 0 ]; then
  _TMP_REPOS+=("$guard")
else
  printf 'owned fallback evidence retained at %s\n' "$guard" >&2
fi
finish
