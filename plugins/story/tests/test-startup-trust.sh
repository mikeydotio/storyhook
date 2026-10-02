#!/usr/bin/env bash
# Discover the parser's boundary regressions through the plugin test runner.
set -euo pipefail
python3 -B "$(dirname "$0")/test_startup_trust.py"
