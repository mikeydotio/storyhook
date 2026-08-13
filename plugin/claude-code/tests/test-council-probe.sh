#!/usr/bin/env bash
# SH-219: council_vote_available (bin/story.sh) — whether --auto's charter
# gets the COUNCIL clause (convene '/council-vote') or the SOLO clause
# (research, decide, record, no council mentioned). Exercised through the
# real dispatch --dry-run path (`.council` in its JSON), the same render
# path test-dispatch-auto.sh and test-charter-inert.sh both trust, rather
# than by re-implementing the probe's own jq logic here.
#
# Every case overrides HOME for ONE invocation only (never the suite's own
# isolated HOME lib.sh already exported) — STORYHOOK_DATA_DIR stays fixed by
# lib.sh regardless, so the story CLI keeps resolving the real isolated
# store even while the probe reads a fixture ~/.claude built just for that
# one call.
source "$(dirname "$0")/lib.sh"

repo=$(mk_story_repo)
id=$(new_story "$repo" "Council probe fixture")

# probe <home> — dispatch --auto --dry-run with HOME overridden, echo the
# result's .council (true/false) and .ok.
probe() {
  local home="$1"
  (cd "$repo" && HOME="$home" STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$id" --auto 2>&1)
}

# mk_home — a fresh, otherwise-empty HOME with no ~/.claude at all.
mk_home() {
  local h
  h="$(mktemp -d /tmp/story-test-council-home.XXXXXX)"
  _TMP_REPOS+=("$h")
  printf '%s' "$h"
}

# mk_registry_entry <home> <key> <skill:yes|no> — one installed_plugins.json
# entry whose installPath optionally ships skills/council-vote/SKILL.md.
mk_registry_entry() {
  local home="$1" key="$2" skill="$3" install
  install="$home/.claude/plugins/cache/fake/$key/1.0.0"
  mkdir -p "$install"
  if [ "$skill" = "yes" ]; then
    mkdir -p "$install/skills/council-vote"
    printf '# council-vote\n' >"$install/skills/council-vote/SKILL.md"
  fi
  mkdir -p "$home/.claude/plugins"
  jq -n --arg k "$key" --arg p "$install" \
    '{version: 2, plugins: {($k): [{scope: "user", installPath: $p}]}}' \
    >"$home/.claude/plugins/installed_plugins.json"
}

# --- no ~/.claude at all: no council, dispatch still succeeds ---
home=$(mk_home)
out=$(probe "$home")
assert_eq "$(jqf "$out" .ok)" "true" "no-claude-dir: ok:true"
assert_eq "$(jqf "$out" .council)" "false" "no-claude-dir: council:false"

# --- registry entry ships the skill, not explicitly disabled: available ---
home=$(mk_home)
mk_registry_entry "$home" "council@agentics" "yes"
out=$(probe "$home")
assert_eq "$(jqf "$out" .ok)" "true" "registry-with-skill: ok:true"
assert_eq "$(jqf "$out" .council)" "true" "registry-with-skill: council:true"

# --- registry entry exists but ships no council-vote skill: not available ---
home=$(mk_home)
mk_registry_entry "$home" "council@agentics" "no"
out=$(probe "$home")
assert_eq "$(jqf "$out" .council)" "false" "registry-no-skill: council:false"

# --- registry + skill, but the plugin is explicitly disabled in the user's
#     own settings.json: not available ---
home=$(mk_home)
mk_registry_entry "$home" "council@agentics" "yes"
jq -n '{enabledPlugins: {"council@agentics": false}}' >"$home/.claude/settings.json"
out=$(probe "$home")
assert_eq "$(jqf "$out" .council)" "false" "user-disabled: council:false"

# --- registry + skill, disabled at the PROJECT level (.claude/settings.json
#     in the checkout) rather than the user's own: not available. The
#     project-local file wins over the plain project one, which wins over
#     the user's — this asserts the project one alone is already enough. ---
home=$(mk_home)
mk_registry_entry "$home" "council@agentics" "yes"
mkdir -p "$repo/.claude"
jq -n '{enabledPlugins: {"council@agentics": false}}' >"$repo/.claude/settings.json"
out=$(probe "$home")
assert_eq "$(jqf "$out" .council)" "false" "project-disabled: council:false"
rm -f "$repo/.claude/settings.json"

# --- a bare skill directly under ~/.claude/skills, no plugin involved:
#     available ---
home=$(mk_home)
mkdir -p "$home/.claude/skills/council-vote"
printf '# council-vote\n' >"$home/.claude/skills/council-vote/SKILL.md"
out=$(probe "$home")
assert_eq "$(jqf "$out" .council)" "true" "bare-user-skill: council:true"

# --- a bare skill directly under the PROJECT's own .claude/skills, no
#     plugin, no user-level anything: available ---
home=$(mk_home)
mkdir -p "$repo/.claude/skills/council-vote"
printf '# council-vote\n' >"$repo/.claude/skills/council-vote/SKILL.md"
out=$(probe "$home")
assert_eq "$(jqf "$out" .council)" "true" "bare-project-skill: council:true"
rm -rf "$repo/.claude/skills"

# --- malformed registry JSON: never crashes the dispatch, reads as no
#     council ---
home=$(mk_home)
mkdir -p "$home/.claude/plugins"
printf '{not valid json' >"$home/.claude/plugins/installed_plugins.json"
out=$(probe "$home")
assert_eq "$(jqf "$out" .ok)" "true" "malformed-registry: ok:true (dispatch survives)"
assert_eq "$(jqf "$out" .council)" "false" "malformed-registry: council:false"

# --- COUNCIL_MODE escape hatch: STORY_COUNCIL=on/off force the answer
#     without touching disk at all, even against a registry that says
#     the opposite ---
home=$(mk_home)
mk_registry_entry "$home" "council@agentics" "yes"
out=$(cd "$repo" && HOME="$home" STORY_DRY_RUN=1 STORY_COUNCIL=off bash "$SCRIPT" dispatch "$id" --auto 2>&1)
assert_eq "$(jqf "$out" .council)" "false" "STORY_COUNCIL=off overrides a real skill: council:false"

home=$(mk_home)
out=$(cd "$repo" && HOME="$home" STORY_DRY_RUN=1 STORY_COUNCIL=on bash "$SCRIPT" dispatch "$id" --auto 2>&1)
assert_eq "$(jqf "$out" .council)" "true" "STORY_COUNCIL=on overrides no skill at all: council:true"

# --- an attended (non---auto) dispatch never probes at all: council:false
#     regardless of what's on disk ---
home=$(mk_home)
mk_registry_entry "$home" "council@agentics" "yes"
out=$(cd "$repo" && HOME="$home" STORY_DRY_RUN=1 bash "$SCRIPT" dispatch "$id" 2>&1)
assert_eq "$(jqf "$out" .auto)" "false" "attended: auto:false"
assert_eq "$(jqf "$out" .council)" "false" "attended: council:false even with a real skill installed"

finish
