#!/usr/bin/env bash
# SH-676: only an explicitly unattended Codex Stop may consider prose approval.
# The Python worker bounds each story --deadline 2 lookup (3s externally),
# the version lookup (2s), and classification (20s): at most 40s before output.
# This hook has 50s in hooks.json; it never inherits the handoff hook's budget.
set -uo pipefail
if [ -z "${STORYHOOK_AUTO:-}${STORYHOOK_FULL_AUTO:-}" ]; then
  cat >/dev/null
  printf '{}'
  exit 0
fi
if ! command -v python3 >/dev/null 2>&1; then
  cat >/dev/null
  printf '{"systemMessage":"StoryHook prose plan approval unavailable: python3 is missing. No approval was sent."}'
  exit 0
fi
export PYTHONDONTWRITEBYTECODE=1
exec python3 "$(dirname "${BASH_SOURCE[0]}")/codex_stop.py"
