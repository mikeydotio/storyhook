#!/usr/bin/env bash
# `story.sh sync [--since <duration>]` (SH-308) — wraps `story commit-sync`.
# `--since` is passed through ONLY when given; the CLI's own default (7d) is
# never restated here, so there is nothing in this script that can drift from
# it the way `skills/story-sync/SKILL.md`'s hand-copied "7d" could.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)
id=$(new_story "$repo" "Sync me")

# --- a commit referencing the story links it ---
(cd "$repo" && git commit --allow-empty -qm "fix: $id progress" ) >/dev/null 2>&1
out=$(cd "$repo" && bash "$SCRIPT" sync --since 1d 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "sync: ok"
assert_contains "$(jqf "$out" .display)" "linked" "sync: display reports what happened"
assert_contains "$(cd "$repo" && story show "$id" --json)" "$(cd "$repo" && git rev-parse HEAD)" \
  "sync: the commit really is linked to the real story"

# --- no --since given: no --since is passed to the CLI at all (its own
# default is the single source of truth, never restated here) ---
out=$(cd "$repo" && bash "$SCRIPT" sync 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "sync: works with no --since"

# --- idempotent: running it again finds nothing new to link ---
out=$(cd "$repo" && bash "$SCRIPT" sync --since 1d 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "sync: re-run is still ok"
assert_contains "$(jqf "$out" .display)" "linked 0" "sync: re-run links nothing new"

# --- errors ---
out=$(cd "$repo" && bash "$SCRIPT" sync --bogus 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "sync: rejects an unknown flag"
out=$(cd "$repo" && bash "$SCRIPT" sync --since 2>&1)
assert_eq "$(jqf "$out" .ok)" "false" "sync: --since with no value is rejected"

finish
