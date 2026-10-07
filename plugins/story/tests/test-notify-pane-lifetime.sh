#!/usr/bin/env bash
# SH-839: a notification scenario must retain its fixture process identity.
source "$(dirname "$0")/lib.sh"
python3 "$TESTS_DIR/test_notify_pane_lifetime.py" "$@"
