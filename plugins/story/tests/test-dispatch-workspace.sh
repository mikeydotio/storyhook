#!/usr/bin/env bash
# Detached terminal servers must not retain dispatch workspace authority.
source "$(dirname "$0")/lib.sh"
# These cases own private real servers. Keep lib.sh's store/binary isolation,
# then remove only its fake terminal from PATH, as the native cleanup tests do.
export PATH="${PATH#"$TESTS_DIR/fakes:"}"
python3 -B "$TESTS_DIR/test-dispatch-workspace.py" "$@"
