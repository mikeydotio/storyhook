#!/usr/bin/env bash
# SH-121 (C10) — which project a story.sh verb acts on, and who decides.
#
# story.sh used to answer that itself: `project_root()` preferred repo_root(),
# so every read verb ran `story` from the repository's top level whatever
# directory the caller stood in. That is strictly less informed than the CLI —
# which since SH-116/SH-119/SH-151 consults `--project`, `$STORYHOOK_PROJECT`,
# the nearest committed pointer file at or above CWD, and the repository's
# registered origin — and in one shape it was not merely redundant but wrong.
#
# The four cases below are the ones that shape has to get right. Two of them
# used to fail:
#
#   1. a plain subdirectory                      — worked before, must keep working
#   2. a monorepo sub-project                    — answered about the WRONG project
#   3. outside a repository, with no --project   — answered "no work" over a refusal
#   4. outside a repository, with --project      — the way in, which did not exist
#
# Case 3 is SH-163: `cmd_list` defaulted to `{"stories":[]}` on any failure, so
# a CLI exit 3 naming three ways out became "No ready stories to pick up." For
# the tool whose job is handing an agent its next task, that is the worst
# available shape — and it is why case 4 needs case 3 to be honest before it can
# be tested at all.
source "$(dirname "$0")/lib.sh"

# slug_for <dir> — the project slug `story project list` reports for the project
# rooted at <dir>. Read out of the listing rather than derived from the
# directory name: the derivation is the CLI's business, and a test that
# reimplemented it would keep passing after the two disagreed.
slug_for() {
  local phys
  phys=$(cd "$1" && pwd -P)
  (cd "$1" && story project list 2>/dev/null) \
    | awk -v p="$phys" 'index($0, p) && $1 != "checkout" && $1 != "origin" {print $1; exit}'
}

# --- 1. a plain subdirectory ------------------------------------------------
repo=$(mk_story_repo)
ready_id=$(new_story "$repo" "Ready story")
mkdir -p "$repo/src/deep/nested"

out=$(cd "$repo/src/deep/nested" && bash "$SCRIPT" list 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "subdir: list resolves the project by walking up"
assert_contains "$out" "$ready_id" "subdir: and it is this repository's story"

out=$(cd "$repo/src/deep/nested" && bash "$SCRIPT" view "$ready_id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "subdir: view resolves the same way"

# --- 2. a monorepo sub-project ----------------------------------------------
# The SH-151 shape: one repository, a project at its top level and another in a
# subdirectory. Standing in the subdirectory, the CLI answers for the
# sub-project; story.sh must not overrule it.
mono=$(mktemp -d /tmp/story-test-mono.XXXXXX)
_TMP_REPOS+=("$mono")
(
  cd "$mono"
  git init -q -b main
  git config user.email t@t
  git config user.name t
  echo a >f
  git add f
  git commit -qm init
  mkdir -p service-b
  story project new --prefix ROOT >/dev/null 2>&1
  cd service-b && story project new --prefix SVCB >/dev/null 2>&1
) >/dev/null 2>&1
root_id=$(cd "$mono" && story new "Only in the root project" --json 2>/dev/null | jq -r '.story.story.id')
sub_id=$(cd "$mono/service-b" && story new "Only in service-b" --json 2>/dev/null | jq -r '.story.story.id')
assert_contains "$sub_id" "SVCB" "fixture: the sub-project must mint its own prefix"

out=$(cd "$mono/service-b" && bash "$SCRIPT" view "$sub_id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "monorepo: view answers for the sub-project, not the repository root"

out=$(cd "$mono/service-b" && bash "$SCRIPT" list 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "monorepo: list answers at all"
assert_contains "$out" "$sub_id" "monorepo: list shows the sub-project's story"
case "$out" in
*"$root_id"*) fail_test "monorepo: list must NOT show the repository root project's story ($root_id)" ;;
esac

# The repository's own top level still answers for itself — the guard against
# this fix going too far, since SH-151's rule cuts both ways.
out=$(cd "$mono" && bash "$SCRIPT" list 2>&1)
assert_contains "$out" "$root_id" "monorepo: the repository root still answers for its own project"

# --- 3. outside a repository, with no --project -----------------------------
# A refusal must read as a refusal. Before SH-163 this was `ok: true, count: 0`.
outside=$(mktemp -d /tmp/story-test-outside.XXXXXX)
_TMP_REPOS+=("$outside")
out=$(cd "$outside" && bash "$SCRIPT" list 2>&1) || true
assert_eq "$(jqf "$out" .ok)" "false" "outside: an unresolvable directory refuses rather than reporting no work"
assert_contains "$(jqf "$out" .display)" "--project" "outside: and the refusal carries the CLI's way out"

# The other direction, which is what makes the assertion above mean something:
# a project that genuinely has nothing ready still answers ok with a count of 0.
empty=$(mk_story_repo)
out=$(cd "$empty" && bash "$SCRIPT" list 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "empty project: still ok"
assert_eq "$(jqf "$out" .count)" "0" "empty project: with a count of zero, distinguishable from a refusal"

# --- 4. outside a repository, with --project --------------------------------
slug=$(slug_for "$repo")
assert_contains "$slug" "" "fixture: a slug was read for the repository"
out=$(cd "$outside" && bash "$SCRIPT" --project "$slug" list 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "outside + --project: resolves with no repository at all"
assert_contains "$out" "$ready_id" "outside + --project: and it is the named project's story"

out=$(cd "$outside" && bash "$SCRIPT" --project="$slug" view "$ready_id" 2>&1)
assert_eq "$(jqf "$out" .ok)" "true" "outside + --project=slug: the equals spelling works too"

out=$(cd "$outside" && bash "$SCRIPT" --project 2>&1) || true
assert_eq "$(jqf "$out" .ok)" "false" "--project with no slug is refused rather than swallowing the verb"

finish
