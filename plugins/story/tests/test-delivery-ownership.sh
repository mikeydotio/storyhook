#!/usr/bin/env bash
# Pure authority, process identity and inherited-lock regressions; no provider.
source "$(dirname "$0")/lib.sh"
python3 "$TESTS_DIR/test_delivery_ownership.py" || exit 1
finish
