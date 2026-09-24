#!/usr/bin/env bash
# SH-758: what a storyhook-started tmux server retains, on private real servers.
source "$(dirname "$0")/lib.sh"
# These cases own private real servers. Keep lib.sh's store/binary isolation,
# then remove only its fake terminal from PATH, as the native cleanup tests do.
export PATH="${PATH#"$TESTS_DIR/fakes:"}"
python3 -B "$TESTS_DIR/test_tmux_server_env.py" || exit 1
finish
