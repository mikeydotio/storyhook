#!/usr/bin/env bash
# Administrative handoffs use a trusted native hook, not the agent's tool budget.
set -uo pipefail
if [ -z "${STORYHOOK_AUTO:-}${STORYHOOK_FULL_AUTO:-}" ]; then
  cat >/dev/null
  printf '{}'
  exit 0
fi
if ! command -v python3 >/dev/null 2>&1; then
  cat >/dev/null
  printf 'StoryHook continuation unavailable: python3 is missing.\n' >&2
  printf '{}'
  exit 0
fi
export PYTHONDONTWRITEBYTECODE=1
case "${1:-}" in
  stop) exec python3 "$(dirname "${BASH_SOURCE[0]}")/session_handoff.py" ;;
  compact) exec python3 "$(dirname "${BASH_SOURCE[0]}")/compact_receipt.py" ;;
  *) cat >/dev/null; printf '{}'; exit 0 ;;
esac
