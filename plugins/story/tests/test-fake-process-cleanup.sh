#!/usr/bin/env bash
# Actual lib.sh scopes and fake tmux only: no provider, real tmux or project.
source "$(dirname "$0")/lib.sh"
python3 -B "$TESTS_DIR/test_fake_process_cleanup.py" "$@"
