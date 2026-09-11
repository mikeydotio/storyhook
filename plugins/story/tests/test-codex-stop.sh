#!/usr/bin/env bash
# Network-free regressions; all scratch files belong to the Python fixtures.
source "$(dirname "$0")/lib.sh"
set -euo pipefail
PYTHONDONTWRITEBYTECODE=1 python3 "$(dirname "$0")/test_codex_stop.py"
PYTHONDONTWRITEBYTECODE=1 python3 "$(dirname "$0")/test_plan_request.py"
PYTHONDONTWRITEBYTECODE=1 python3 "$(dirname "$0")/test_codex_classifier.py"
