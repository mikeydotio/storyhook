#!/usr/bin/env bash
# Source this without changing the caller's shell options. With no destination
# the command runs directly, preserving ordinary developer/test invocations.
activity_run() {
    local source="$1"
    shift
    if [ -z "${STORYHOOK_ACTIVITY_LOG_DIR:-}" ]; then
        "$@"
    elif . "$(dirname "${BASH_SOURCE[0]}")/python-runtime.sh" && storyhook_python_init; then
        "$STORYHOOK_PYTHON" "$(dirname "${BASH_SOURCE[0]}")/activity-run.py" "$source" -- "$@"
    else
        # Entry points enforce the runtime; optional observation preserves status.
        printf 'warning: activity capture unavailable for %s: python3 runtime unavailable: %s\n' \
            "$source" "${STORYHOOK_PYTHON_ERROR:-could not load runtime policy}" >&2
        "$@"
    fi
}
