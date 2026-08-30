#!/usr/bin/env bash
# `story.sh capabilities --agent=<p>` (SH-517) -- the provider-knowledge
# catalog the daemon relays at GET /api/dispatch-options and the web dispatch
# dialog builds its Model/Effort/Speed selects from. Sourced from the SAME
# per-provider block `configure_agent` already reads its launch templates
# from, so this suite is what pins that catalog's shape rather than duplicating
# it in a second source of truth.
source "$(dirname "$0")/lib.sh"

run_capabilities() {
  bash "$SCRIPT" capabilities "$@" 2>&1
}

# Claude's catalog: the exact set the story asks for -- Fable, Opus,
# Opus+Sonnet (mapped to the real `opusplan` alias), Sonnet, Haiku -- with
# opusplan marked as the default model, since that is what an unconfigured
# dispatch already launches.
out=$(run_capabilities --agent=claude)
assert_eq "$(jqf "$out" .ok)" "true" "claude: ok"
assert_eq "$(jqf "$out" .agent)" "claude" "claude: agent echoed"
assert_eq "$(jqf "$out" '.models | map(.id) | sort | join(",")')" \
  "fable,haiku,opus,opusplan,sonnet" "claude: model id set"
assert_eq "$(jqf "$out" '.models[] | select(.id=="opusplan") | .label')" \
  "Opus+Sonnet" "claude: opusplan labelled Opus+Sonnet"
assert_eq "$(jqf "$out" '.models[] | select(.id=="opusplan") | .default')" \
  "true" "claude: opusplan is the default model"
assert_eq "$(jqf "$out" '[.models[] | select(.default==true)] | length')" \
  "1" "claude: exactly one default model"
assert_eq "$(jqf "$out" '.efforts | map(.id) | sort | join(",")')" \
  "high,low,max,medium,xhigh" "claude: effort id set"
assert_eq "$(jqf "$out" '.speeds | map(.id) | join(",")')" \
  "fast" "claude: speed offers only the one alternative to Default -- no redundant standard entry"

# Codex's catalog: the GPT-5.6 trio the story names, no forced default model
# (its own config.toml decides, matching dispatch's existing no-flag
# behavior), and an extra "none" effort level Codex's own CLI accepts.
out=$(run_capabilities --agent=codex)
assert_eq "$(jqf "$out" .ok)" "true" "codex: ok"
assert_eq "$(jqf "$out" .agent)" "codex" "codex: agent echoed"
assert_eq "$(jqf "$out" '.models | map(.id) | sort | join(",")')" \
  "gpt-5.6-luna,gpt-5.6-sol,gpt-5.6-terra" "codex: model id set"
assert_eq "$(jqf "$out" '[.models[] | select(.default==true)] | length')" \
  "0" "codex: no forced default model"
assert_eq "$(jqf "$out" '.efforts | map(.id) | sort | join(",")')" \
  "high,low,max,medium,none,xhigh" "codex: effort id set (includes none)"
assert_eq "$(jqf "$out" '.speeds | map(.id) | join(",")')" \
  "fast" "codex: speed offers only fast, same as claude"

# STORY_AGENT is the fallback when --agent is omitted, exactly like every
# other verb.
out=$(STORY_AGENT=codex run_capabilities)
assert_eq "$(jqf "$out" .agent)" "codex" "STORY_AGENT fallback: codex"

# Absent both, capabilities defaults to Claude -- matching cmd_dispatch's own
# "${STORY_AGENT:-claude}" default.
out=$(run_capabilities)
assert_eq "$(jqf "$out" .agent)" "claude" "no agent given: defaults to claude"

# Unknown agent refuses the same way every other agent-accepting verb does.
out=$(run_capabilities --agent=gpt4)
assert_eq "$(jqf "$out" .ok)" "false" "unknown agent: ok:false"
assert_contains "$out" "unknown agent" "unknown agent: names the problem"

# A stray extra argument is a hard usage failure, not a silent ignore --
# matching dispatch/view/list's own convention for trailing tokens.
out=$(run_capabilities --agent=claude extra)
assert_eq "$(jqf "$out" .ok)" "false" "trailing argument: ok:false"
assert_contains "$out" "usage" "trailing argument: usage message"

finish
