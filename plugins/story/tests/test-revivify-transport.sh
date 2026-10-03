#!/usr/bin/env bash
# SH-825: shared discovery plus real composed-helper/private-listener coverage.
# shellcheck source=plugins/story/tests/lib.sh
source "$(dirname "$0")/lib.sh"
export PATH="${PATH#"$TESTS_DIR/fakes:"}"
python3 -B -W error "$TESTS_DIR/test_tmux_target.py" || exit 1
python3 -B -W error "$TESTS_DIR/test_tmux_restore.py" || exit 1
python3 -B -W error "$TESTS_DIR/test_process_observation.py" || exit 1
python3 -B -W error "$TESTS_DIR/test_restored_dispatch.py" || exit 1
python3 -B -W error "$TESTS_DIR/test_revivify_transport.py" || exit 1
finish
