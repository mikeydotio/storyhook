#!/usr/bin/env bash
# SH-786: schedule real tracker mutations at the dispatcher handoff boundaries.
source "$(dirname "$0")/lib.sh"

# Both dispatch and the real daemon delivery worker use this checkout's helper.
export STORYHOOK_DISPATCH_SCRIPT="$SCRIPT"

fixture=$(mktemp -d /tmp/story-test-block-race.XXXXXX)
_TMP_REPOS+=("$fixture")
export SH786_REAL_STORY
SH786_REAL_STORY=$(command -v story)
cat > "$fixture/story" <<'PY'
#!/usr/bin/env python3
import json, os, subprocess, sys
from pathlib import Path

args = sys.argv[1:]
real = os.environ['SH786_REAL_STORY']
if not any(verb in args for verb in ('supersede-block-deliveries', 'session-eligibility')):
    os.execv(real, [real, *args])
state = Path(os.environ['FAKE_TMUX_STATE'])
phase = os.environ['SH786_PHASE']
action = os.environ.get('SH786_ACTION', 'awaiting')
project = args[args.index('--project') + 1]
story = os.environ['SH786_ID']

def mutate():
    commands = {
        'awaiting': ['block', story, 'SH-786 boundary hold'],
        'blocked': ['move', story, 'blocked'],
        'relation': ['relate', story, 'blocked-by', os.environ.get('SH786_BLOCKER', '')],
        'block-on': ['block', story, '--on', os.environ.get('SH786_BLOCKER', '')],
        'lift': ['block', story, 'transient hold'],
    }
    subprocess.run([real, '--project', project, *commands[action]], check=True, stdout=subprocess.DEVNULL)
    if action == 'lift':
        subprocess.run([real, '--project', project, 'unblock', story], check=True, stdout=subprocess.DEVNULL)
    (state / 'mutation').write_text(action)

answer = subprocess.run([real, *args], capture_output=True)
if answer.returncode:
    sys.stdout.buffer.write(answer.stdout)
    sys.stderr.buffer.write(answer.stderr)
    sys.exit(answer.returncode)
if 'supersede-block-deliveries' in args:
    (state / 'revoked').touch()
    if phase == 'after-revocation':
        mutate()
if 'session-eligibility' in args:
    final = (state / 'pane_identity').exists()
    with (state / 'eligibility-calls').open('a') as log:
        log.write('final\n' if final else 'early\n')
    if final and phase == 'after-query':
        mutate()
    if phase in ('malformed', 'wrong-story', 'wrong-version', 'wrong-type', 'error'):
        if phase == 'error':
            print('SH-786 eligibility transport failure', file=sys.stderr)
            sys.exit(2)
        value = json.loads(answer.stdout)
        if phase == 'malformed':
            print('{')
            sys.exit(0)
        key, bad = {'wrong-story': ('story_id', 'OTHER-1'),
                    'wrong-version': ('schema_version', 99),
                    'wrong-type': ('eligible', 'true')}[phase]
        value['session_eligibility'][key] = bad
        print(json.dumps(value))
        sys.exit(0)
sys.stdout.buffer.write(answer.stdout)
PY
chmod +x "$fixture/story"
printf '#!/bin/sh\nexit 0\n' > "$fixture/codex"
chmod +x "$fixture/codex"

# The general fake has separate resource and notify inventories. Publish the
# latter from the actual registration only after it exists, including in the
# daemon's inherited environment; otherwise a late interrupt sees no pane.
mkdir "$fixture/terminal"
cat > "$fixture/terminal/tmux" <<'SH'
#!/usr/bin/env bash
socket="${TMUX%%,*}"
args=("$@")
for ((i=0; i<${#args[@]}-1; i++)); do
  if [ "${args[$i]}" = -S ]; then
    socket="${args[$((i+1))]}"
    break
  fi
done
case "$socket" in
  /tmp/story-*/*|/private/tmp/story-*/*)
    export FAKE_TMUX_STATE="${socket%/*}" ;;
esac
if [ -s "$FAKE_TMUX_STATE/pane_identity" ]; then
  export FAKE_TMUX_PANES
  FAKE_TMUX_PANES=$(jq -r '"\(.story)\t1\t\(.pane)"' "$FAKE_TMUX_STATE/pane_identity")
fi
SH
printf 'exec %q "$@"\n' "$TESTS_DIR/fakes/tmux" >> "$fixture/terminal/tmux"
chmod +x "$fixture/terminal/tmux"
export PATH="$fixture/terminal:$PATH"

case_started=false
new_case() {
  # The daemon captures the fake terminal's directory at startup. Each case
  # owns a fresh terminal; finish the previous worker before switching it.
  if [ "$case_started" = true ]; then
    story daemon stop --force >/dev/null || { fail_test 'cannot stop fixture daemon'; finish; }
  fi
  case_started=true
  export FAKE_TMUX_STATE
  FAKE_TMUX_STATE=$(mktemp -d /tmp/story-test-block-race-tmux.XXXXXX)
  _TMP_REPOS+=("$FAKE_TMUX_STATE")
  repo=$(mk_story_repo)
  id=$(new_story "$repo" "Claim to launch block")
  blocker=$(new_story "$repo" "Dependency")
}

await_interrupt() {
  local attempts
  attempts=$(python3 "$TESTS_DIR/../../../scripts/tests/load_grace.py" patience 60)
  attempts=$(awk -v seconds="$attempts" 'BEGIN {print int(seconds * 10) + 1}')
  for ((attempt=0; attempt<attempts; attempt++)); do
    receipt=$(cd "$repo" && story show "$id" --json)
    if printf '%s' "$receipt" | jq -e '[.story.story.comments[]?.text | select(startswith("AGENT BLOCK DELIVERY") and contains("interrupt"))] | length > 0' >/dev/null; then return; fi
    sleep 0.1
  done
  fail_test "no interrupt receipt for $id: $receipt"
}

dispatch_case() {
  local phase="$1" provider="$2"
  shift 2
  out=$(cd "$repo" && PATH="$fixture:$PATH" STORY_BIN="$fixture/story" \
    SH786_ID="$id" SH786_BLOCKER="$blocker" SH786_PHASE="$phase" \
    TMUX=fake TMUX_PANE=%0 STORY_COUNCIL=off \
    STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 STORY_CONFIRM_DELAY=0 \
    STORY_PASTE_SETTLE_DELAY=0 FAKE_TMUX_CAPTURE=marker \
    FAKE_TMUX_CODEX_SENTINEL_MODE=identity FAKE_TMUX_CODEX_PLUGIN_ROOT="$PLUGIN_ROOT" \
    bash "$SCRIPT" dispatch "$@" --agent="$provider" 2>&1)
}

assert_no_charter() {
  if [ -f "$FAKE_TMUX_STATE/submitted" ] \
     && rg -q 'Investigate and plan|Work on story|AUTONOMOUS session' "$FAKE_TMUX_STATE/submitted"; then
    fail_test "blocked dispatch submitted its work charter: $out"
  fi
}

# The mutation happens while dispatch owns the workspace lock, after it has
# revoked the old session's deliveries. Each mode reaches production startup.
for mode in fresh next force resume; do
  new_case
  args=("$id")
  expected_claim=false
  case "$mode" in
    next) args=(--next) ;;
    force|resume)
      (cd "$repo" && story claim "$id" >/dev/null)
      args+=("--$mode")
      [ "$mode" != force ] || args+=(--auto --full-auto)
      expected_claim=true
      if [ "$mode" = resume ]; then
        mk_dispatched "$repo" "$id" >/dev/null
        printf 'retained work\n' > "$repo/.claude/worktrees/$id/retained.txt"
      fi ;;
  esac
  dispatch_case after-revocation claude "${args[@]}"
  assert_eq "$(jqf "$out" .reason)" dispatch-ineligible "$mode final refusal: $out"
  assert_eq "$(jqf "$out" .eligibility_reason)" awaiting "$mode tracker reason"
  [ -f "$FAKE_TMUX_STATE/mutation" ] || fail_test "$mode never reached boundary"
  assert_no_charter
  if [ "$mode" = resume ]; then
    assert_eq "$(cat "$repo/.claude/worktrees/$id/retained.txt")" 'retained work' 'resume preserves dirty work'
  else
    [ ! -e "$repo/.claude/worktrees/$id" ] || fail_test "$mode leaked startup worktree"
  fi
  assert_eq "$(jqf "$out" .claimed)" "$expected_claim" "$mode respects claim ownership"
done

# The force entry gate must work even when the worker already tried and failed
# to interrupt a pane that did not exist. Wait on its receipt, never a race sleep.
new_case
(cd "$repo" && story claim "$id" >/dev/null && story block "$id" 'pre-launch hold' >/dev/null)
await_interrupt
assert_contains "$receipt" 'interrupt unreached' 'pre-launch interrupt could not reach a pane'
dispatch_case none claude "$id" --force
assert_eq "$(jqf "$out" .reason)" dispatch-ineligible "force preflight refuses held claim: $out"
[ ! -e "$FAKE_TMUX_STATE/new_window_args.log" ] || fail_test 'force launched despite early hold'
assert_no_charter

for action in blocked relation block-on; do
  new_case
  SH786_ACTION="$action" dispatch_case after-revocation codex "$id" --auto
  assert_eq "$(jqf "$out" .reason)" dispatch-ineligible "$action final refusal: $out"
  assert_no_charter
  assert_eq "$(cat "$FAKE_TMUX_STATE/prompt_submits")" 1 'Codex gets only its task-free initialization'
done

for phase in malformed wrong-story wrong-version wrong-type error; do
  new_case
  dispatch_case "$phase" claude "$id"
  assert_eq "$(jqf "$out" .reason)" dispatch-eligibility-unavailable "$phase fails closed: $out"
  assert_no_charter
done

new_case
SH786_ACTION=lift dispatch_case after-revocation claude "$id"
assert_ok "$out" true "a hold lifted before handoff does not forbid work: $out"
assert_contains "$(cat "$FAKE_TMUX_STATE/eligibility-calls")" final 'successful handoff checked registered session'

new_case
dispatch_case after-query claude "$id"
assert_ok "$out" true "a later block uses delivery authority: $out"
[ -f "$FAKE_TMUX_STATE/mutation" ] || fail_test 'late block boundary was not exercised'
assert_contains "$(cat "$FAKE_TMUX_STATE/eligibility-calls")" final 'late block follows registered-session check'
await_interrupt
assert_contains "$receipt" 'interrupt delivered' 'late block reaches the registered session through the real worker'

new_case
FAKE_TMUX_FAIL_KILL_PANE=1 dispatch_case after-revocation claude "$id"
assert_eq "$(jqf "$out" .reason)" dispatch-ineligible 'uncertain cleanup still refuses the charter'
assert_eq "$(jqf "$out" .claimed)" true 'uncertain termination preserves the claim'
assert_contains "$(jqf "$out" .display)" 'startup cleanup could not be confirmed' 'uncertain cleanup is explicit'
[ -d "$repo/.claude/worktrees/$id" ] || fail_test 'uncertain cleanup removed the worktree'
assert_no_charter

new_case
(cd "$repo" && story claim "$id" >/dev/null)
STORY_DRY_RUN=1 dispatch_case none claude "$id" --force
assert_ok "$out" true 'eligible forced dry-run succeeds'
assert_eq "$(jqf "$out" .eligibility_phase)" preflight 'dry-run reports only preflight'
assert_eq "$(jqf "$out" .handoff_eligibility_checked)" false 'dry-run never claims future authorization'
[ ! -e "$FAKE_TMUX_STATE/revoked" ] || fail_test 'dry-run revoked delivery authority'
[ ! -e "$FAKE_TMUX_STATE/new_window_args.log" ] || fail_test 'dry-run created a provider'

finish
