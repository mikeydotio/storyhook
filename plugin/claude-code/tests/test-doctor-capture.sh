#!/usr/bin/env bash
# `story.sh doctor` and `story.sh capture` — the two diagnostic verbs.
#
# doctor deviates from /issue's: the story CLI has its OWN `story doctor`
# (project data integrity), so this verb runs BOTH that and the tmux
# readiness probe. The integrity half is the tricky one -- `story doctor`
# exits 5 with an error whenever it finds anything, and emits its `.issues[]`
# array only when that array is EMPTY, so a finding must read as a finding and
# never as a failure of the probe itself.
source "$(dirname "$0")/lib.sh"

export PATH="$TESTS_DIR/fakes:$PATH"
export FAKE_TMUX_STATE
FAKE_TMUX_STATE=$(mktemp -d /tmp/story-test-tmux.XXXXXX)
_TMP_REPOS+=("$FAKE_TMUX_STATE")

repo=$(mk_story_repo)
id=$(new_story "$repo" "Dispatched story")
w=$(wname_for "$repo" "$id")

# --- capture: dry run names the window and runs nothing ---
out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" capture "$id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "capture dry: ok"
assert_eq "$(jqf "$out" .window_name)" "$w" "capture dry: window name matches dispatch's"
assert_contains "$(jqf "$out" '.commands|join(" ")')" "capture-pane" "capture dry: previews the read"

# --- capture: no such window ---
out=$(cd "$repo" && TMUX=fake TMUX_PANE=%0 FAKE_TMUX_PANES="" bash "$SCRIPT" capture "$id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "capture: no live window is ok:false"
assert_contains "$(jqf "$out" .display)" "/story do $id" "capture: points at the dispatch verb"

# --- capture: a live window is read back ---
# FAKE_TMUX_PANES lines are "name<TAB>active<TAB>pane", matching the real
# `list-panes -a -F '#{window_name}\t#{pane_active}\t#{pane_id}'`.
out=$(cd "$repo" && TMUX=fake TMUX_PANE=%0 \
       FAKE_TMUX_PANES="$(printf 'other\t1\t%%3\n%s\t1\t%%7' "$w")" \
       FAKE_TMUX_TRANSCRIPT="hello from the session" \
       bash "$SCRIPT" capture "$id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "capture: ok"
assert_eq "$(jqf "$out" .pane)" "%7" "capture: resolves the pane of the named window"
assert_contains "$(jqf "$out" .transcript)" "hello from the session" "capture: returns the transcript"
assert_contains "$(jqf "$out" .display)" "hello from the session" "capture: display carries it"

# --- capture: preconditions and arg validation ---
out=$(cd "$repo" && env -u TMUX -u TMUX_PANE bash "$SCRIPT" capture "$id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "capture: refuses outside tmux"
assert_contains "$(jqf "$out" .display)" "tmux" "capture: says why"
out=$(cd "$repo" && bash "$SCRIPT" capture 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "capture: missing id is ok:false"
out=$(cd "$repo" && bash "$SCRIPT" capture "bad id!" 2>&1)
assert_contains "$(jqf "$out" .display)" "alphanumeric" "capture: invalid id rejected"

# --- doctor: dry run previews BOTH halves ---
out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" doctor 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "doctor dry: ok"
assert_contains "$(jqf "$out" '.commands|join(" ")')" "story doctor" "doctor dry: previews the integrity check"
assert_contains "$(jqf "$out" '.commands|join(" ")')" "new-window" "doctor dry: previews the readiness probe"
assert_contains "$(jqf "$out" '.commands|join(" ")')" "paste-buffer -p" \
  "doctor dry: probes via bracketed paste, the delivery path dispatch actually uses"

# --- doctor: a healthy project reports integrity OK ---
out=$(cd "$repo" && TMUX=fake TMUX_PANE=%0 FAKE_TMUX_CAPTURE=marker bash "$SCRIPT" doctor 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "doctor: ok"
assert_eq "$(jqf "$out" .readiness_confirmed)" "true" "doctor: readiness confirmed via the fake TUI"
assert_eq "$(jqf "$out" .matched_tier)" "marker" "doctor: reports which tier matched"
assert_eq "$(jqf "$out" .project_integrity.ok)" "true" "doctor: healthy project integrity"
assert_contains "$(jqf "$out" .display)" "project integrity: OK" "doctor: display carries both halves"
assert_eq "$(jqf "$out" .occupant.match_rule)" "pattern" "doctor: a plainly-named claude matches by NAME"
assert_eq "$(jqf "$out" .occupant.matches_name_pattern)" "true" "doctor: ...and says the name pattern covers it"

# --- doctor: SH-239 — it REPORTS a build the name pattern alone would refuse --
# doctor advertises that it checks "whether this Claude build's readiness/paste
# path is still recognised", and this is precisely the drift it missed: a native
# installer install whose binary is version-named. Dispatch works (pane_runs
# matches it by identity), but an operator must be told that the NAME check no
# longer covers their build -- and told NOT to pin the pattern to a version that
# changes on every update.
d_root=$(mk_versioned_claude 2.1.228)
out=$(cd "$repo" && PATH="$d_root/bin:$PATH" TMUX=fake TMUX_PANE=%0 \
       FAKE_TMUX_CAPTURE=marker FAKE_TMUX_PANE_COMMAND=2.1.228 \
       STORY_DOCTOR_LAUNCH_CMD="claude --permission-mode plan" \
       bash "$SCRIPT" doctor 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "doctor(SH-239): a version-named build is still ok"
assert_eq "$(jqf "$out" .readiness_confirmed)" "true" "doctor(SH-239): readiness IS confirmed"
assert_eq "$(jqf "$out" .occupant.match_rule)" "launch-binary" \
  "doctor(SH-239): recognised by identity, not by name"
assert_eq "$(jqf "$out" .occupant.matches_name_pattern)" "false" \
  "doctor(SH-239): and it says the name pattern does NOT cover this build"
assert_eq "$(jqf "$out" .occupant.name)" "2.1.228" "doctor(SH-239): reports the observed occupant"
assert_contains "$(jqf "$out" .occupant.launch_binary_resolved)" "versions/2.1.228" \
  "doctor(SH-239): reports what the launcher resolves THROUGH the symlink to"
assert_contains "$(jqf "$out" .display)" "does NOT match STORY_READY_PROCESS_PATTERN" \
  "doctor(SH-239): the display carries the warning, not just the JSON"
assert_contains "$(jqf "$out" .display)" "changes on every update" \
  "doctor(SH-239): and warns against pinning the pattern to a version"

# --- doctor: an integrity FINDING is a finding, not a failed probe ---
# The shell property under test is narrow and important: `story doctor` exits 5
# whenever it finds anything, and story.sh must read that as a finding rather
# than as a failure of its own readiness probe.
#
# The finding is simulated rather than fabricated, and deliberately. Under
# `.storyhook/` this test wrote a dangling relation straight into a story's
# JSONL; the store refuses that shape at the schema, so producing one now needs
# `storyhook::store::test_support` — a Rust API a shell test cannot reach.
# That the CLI really exits 5 on a real finding is pinned in Rust instead, by
# `tests/doctor.rs` and by `tests/error_contract.rs`'s Integrity row. See
# fakes/story-integrity/story.
export STORY_REAL_BIN
STORY_REAL_BIN=$(command -v story)
export STORY_FAKE_FINDING="TST-9999 is referenced by a relation nothing else records"

rc=0
(cd "$repo" && PATH="$TESTS_DIR/fakes/story-integrity:$PATH" story doctor --json >/dev/null 2>&1) || rc=$?
assert_eq "$rc" "5" "fixture sanity: a finding makes \`story doctor\` exit 5"

out=$(cd "$repo" && PATH="$TESTS_DIR/fakes/story-integrity:$PATH" \
       TMUX=fake TMUX_PANE=%0 FAKE_TMUX_CAPTURE=marker bash "$SCRIPT" doctor 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "doctor: a finding does NOT flip ok:false"
assert_eq "$(jqf "$out" .project_integrity.ok)" "false" "doctor: integrity reported as not-ok"
assert_contains "$(jqf "$out" .project_integrity.summary)" "TST-9999" "doctor: summary names the finding"
assert_eq "$(jqf "$out" .readiness_confirmed)" "true" \
  "doctor: the readiness probe still ran despite the integrity finding"

# --- doctor: preconditions and arg validation ---
out=$(cd "$repo" && env -u TMUX -u TMUX_PANE bash "$SCRIPT" doctor 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "doctor: refuses outside tmux"
out=$(cd "$repo" && bash "$SCRIPT" doctor extra 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "doctor: takes no arguments"
out=$(cd "$repo" && STORY_DRY_RUN=1 STORY_DOCTOR_LAUNCH_CMD=definitely-not-a-real-binary \
       bash "$SCRIPT" doctor 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "doctor: missing launch binary is ok:false"

finish
