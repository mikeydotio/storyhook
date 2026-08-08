#!/usr/bin/env bash
# SH-62: `dispatch <id> --auto` swaps in the autonomous charter (plan
# approval stays the ONE human interaction; the child resolves the rest
# itself) while leaving the attended path byte-identical. Mostly dry-run
# (no tmux needed — every case here either refuses before the DRY_RUN branch
# point or IS the dry-run branch), plus one real fake-tmux dispatch mirroring
# test-dispatch-happy.sh for the confirmed-path `display`.
source "$(dirname "$0")/lib.sh"

FAKE_TMUX_DIR="$TESTS_DIR/fakes"

repo=$(mk_story_repo)
repo_phys=$(cd "$repo" && pwd -P)  # symlink-resolved, matches repo_root()'s `pwd -P`
id=$(new_story "$repo" "Ready story for auto dispatch")

dry() {
  (cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$id" "$@" 2>&1)
}

# --- attended path is untouched: byte-identical prompt, auto:false ---
# A literal copy of PROMPT_TPL's default (story.sh) with <n> substituted —
# this is the byte-identical regression pin SH-62's own acceptance criteria
# demand for the no-flag path.
expected_attended_prompt="Investigate and plan a fix for story $id in this repo. Begin by reading it with \`story show $id --json\` (its comments carry the discussion history). When your plan is finalized and approved, post it as a comment on $id via \`story comment $id \"<plan>\"\` before you start implementing. Ensure every pull request you open references story $id in its body, and comment a link to each PR on $id after you push it. Do not bump the version or deploy from this worktree: do not run semver bump, deployit deploy, or any release/version step, and do not plan for them -- versioning and deployment happen later from the main branch, not here."

out=$(dry)
assert_eq "$(jqf "$out" .ok)" "true" "attended: ok:true"
assert_eq "$(jqf "$out" .auto)" "false" "attended: auto:false"
assert_eq "$(jqf "$out" .prompt)" "$expected_attended_prompt" "attended: prompt is byte-identical to today's"
for marker in "AUTONOMOUS" "council-vote" "gh pr merge" "story block" "story move $id" "reap"; do
  case "$(jqf "$out" .prompt)" in
    *"$marker"*) fail_test "attended: prompt leaked auto-charter marker [$marker]" ;;
  esac
done
case "$(jqf "$out" .display)" in
  *utonomous*) fail_test "attended: display mentions autonomy without --auto" ;;
esac

# --- --auto dry run: charter content, auto:true, display warns about
#     auto-accept-edits ---
out=$(dry --auto)
assert_eq "$(jqf "$out" .ok)" "true" "auto: ok:true"
assert_eq "$(jqf "$out" .auto)" "true" "auto: auto:true"
prompt=$(jqf "$out" .prompt)
for marker in \
  "AUTONOMOUS" \
  "council-vote" \
  "make test" \
  "gh pr merge --merge" \
  "story block $id" \
  "story move $id done" \
  "semver bump"; do
  case "$prompt" in
    *"$marker"*) : ;;
    *) fail_test "auto: prompt missing charter obligation [$marker]" ;;
  esac
done

# SH-208: the charter's own last act is a fully-resolved `reap` command, not
# instructions the child must reconstruct -- this project's own slug, this
# script's own path, and the story id are all templated in.
reap_slug=$(slug_for "$repo")
expected_reap="bash \"$SCRIPT\" --project \"$reap_slug\" reap \"$id\""
assert_contains "$prompt" "$expected_reap" "auto: prompt carries the exact reap command"
assert_contains "$prompt" "run \`$expected_reap\` as your absolute last action" \
  "auto: reap is framed as the session's final act"
assert_contains "$prompt" "never run \`$expected_reap\` after a hard stop" \
  "auto: reap is explicitly forbidden past a hard stop"

# The charter used to require every `story` write to be made from the main
# checkout, and templated its absolute path in to say where that was. It said
# so because a worktree carried its own copy of the tracker and a write made
# against it was silently lost (SH-46). One store ended that: the worktree and
# the main checkout are the same project. The clause is gone, and these pin it
# gone — reintroducing it would send an autonomous agent `cd`-ing between
# directories for no reason at all.
for absent in "$repo_phys" "cd $repo_phys" "separate copy"; do
  case "$prompt" in
    *"$absent"*) fail_test "auto: charter still anchors story writes elsewhere [$absent]" ;;
  esac
done
display=$(jqf "$out" .display)
case "$display" in
  *utonomous*) : ;;
  *) fail_test "auto: display does not mention autonomy" ;;
esac
case "$display" in
  *"auto-accept edits"*) : ;;
  *) fail_test "auto: display does not warn about choosing auto-accept edits" ;;
esac

# --- --auto is accepted before the id too ---
out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch --auto "$id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "auto-first: ok:true"
assert_eq "$(jqf "$out" .auto)" "true" "auto-first: auto:true"

# --- a stray trailing token or unknown flag is now a hard fail, naming
#     --auto in the usage string (dispatch used to silently ignore extras) ---
out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$id" junk 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "trailing token: ok:false"
assert_contains "$(jqf "$out" .display)" "usage" "trailing token: usage message"
assert_contains "$(jqf "$out" .display)" "--auto" "trailing token: usage names --auto"

out=$(cd "$repo" && STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$id" --bogus 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "unknown flag: ok:false"
assert_contains "$(jqf "$out" .display)" "--auto" "unknown flag: usage names --auto"

# --- STORY_AUTO_PROMPT overrides the charter, same seam as STORY_PROMPT ---
out=$(cd "$repo" && STORY_DRY_RUN=1 STORY_AUTO_PROMPT="custom auto <n> <dir>" \
  bash "$SCRIPT" dispatch "$id" --auto 2>&1)
assert_eq "$(jqf "$out" .prompt)" "custom auto $id $repo_phys" \
  "STORY_AUTO_PROMPT: overrides the auto charter and renders <n>/<dir>"

# STORY_PROMPT (the attended override) must not leak into an --auto run.
out=$(cd "$repo" && STORY_DRY_RUN=1 STORY_PROMPT="attended override <n>" \
  bash "$SCRIPT" dispatch "$id" --auto 2>&1)
case "$(jqf "$out" .prompt)" in
  "attended override"*) fail_test "STORY_PROMPT leaked into an --auto dispatch" ;;
esac

# STORY_PROMPT_EXTRA still appends after the auto charter.
out=$(cd "$repo" && STORY_DRY_RUN=1 STORY_PROMPT_EXTRA="EXTRA-CLAUSE" \
  bash "$SCRIPT" dispatch "$id" --auto 2>&1)
case "$(jqf "$out" .prompt)" in
  *"never run \`$expected_reap\` after a hard stop. EXTRA-CLAUSE") : ;;
  *) fail_test "STORY_PROMPT_EXTRA did not append after the auto charter" ;;
esac

# --- no side effects from any refused/dry-run attempt above ---
[ -d "$repo/.claude/worktrees" ] && fail_test "auto: a worktree was created by a dry run or a refused dispatch"
state=$(cd "$repo" && story show "$id" --json | jq -r '.story.story.state')
assert_eq "$state" "todo" "auto: story state untouched by any dry run or refused dispatch"

# --- one real --auto dispatch, fake tmux (mirrors test-dispatch-happy.sh) ---
out=$(
  cd "$repo" \
    && PATH="$FAKE_TMUX_DIR:$PATH" \
      TMUX="fake,0,0" TMUX_PANE="%0" \
      STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0 \
      STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0 \
      FAKE_TMUX_CAPTURE=marker \
      bash "$SCRIPT" dispatch "$id" --auto 2>&1
)
assert_eq "$(jqf "$out" .ok)" "true" "real auto dispatch: ok:true"
assert_eq "$(jqf "$out" .auto)" "true" "real auto dispatch: auto:true"
assert_eq "$(jqf "$out" .claimed)" "true" "real auto dispatch: claimed:true"
assert_contains "$(jqf "$out" .display)" "utonomous" "real auto dispatch: display names the session autonomous"
assert_contains "$(jqf "$out" .display)" "auto-accept edits" "real auto dispatch: display warns about auto-accept edits"

finish
