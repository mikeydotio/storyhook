#!/usr/bin/env bash
# Real Git credential resolution; all configuration and endpoints are fixtures.
source "$(dirname "$0")/lib.sh"
# Keep the shell alive so lib.sh releases its fixture home and binary lease.
python3 "$TESTS_DIR/test_submission_git.py"
