#!/usr/bin/env bash
# Native terminal/process regressions with isolated real story stores; no model is contacted.
source "$(dirname "$0")/lib.sh"
# These cases own private real servers. Keep lib.sh's store/binary isolation,
# then remove only its fake terminal from PATH, as the native cleanup tests do.
export PATH="${PATH#"$TESTS_DIR/fakes:"}"
# Notify commands get production's own bound, never a stricter one (SH-766).
notify_timeout=$(rust_duration_secs src/daemon/block_delivery.rs NOTIFY_TIMEOUT) \
  && notify_grace=$(rust_duration_secs src/daemon/block_delivery.rs NOTIFY_TERM_GRACE) \
  || { fail_test "cannot derive the notify bound from src/daemon/block_delivery.rs"; finish; }
export STORYHOOK_TEST_NOTIFY_BOUND_SECS=$((notify_timeout + notify_grace))
python3 "$TESTS_DIR/test-agent-identity.py" "$@"
