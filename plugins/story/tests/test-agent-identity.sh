#!/usr/bin/env bash
# Native terminal/process regressions with isolated real story stores; no model is contacted.
source "$(dirname "$0")/lib.sh"
# These cases own private real servers. Keep lib.sh's store/binary isolation,
# then remove only its fake terminal from PATH, as the native cleanup tests do.
export PATH="${PATH#"$TESTS_DIR/fakes:"}"
python3 "$TESTS_DIR/test-agent-identity.py" "$@"
