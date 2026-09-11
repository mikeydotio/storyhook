#!/usr/bin/env bash
# Native terminal/process regressions; no story store or model is contacted.
set -euo pipefail
exec python3 "$(dirname "$0")/test-agent-identity.py" "$@"
