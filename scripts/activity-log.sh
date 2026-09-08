#!/usr/bin/env bash
# Source this without changing the caller's shell options. With no destination
# the command runs directly, preserving ordinary developer/test invocations.
activity_run() {
    local source="$1"
    shift
    if [ -z "${STORYHOOK_ACTIVITY_LOG_DIR:-}" ]; then
        "$@"
    elif command -v python3 >/dev/null 2>&1; then
        python3 "$(dirname "${BASH_SOURCE[0]}")/activity-run.py" "$source" -- "$@"
    else
        printf '%s\n' "warning: activity capture unavailable for $source: python3 not found" >&2
        "$@"
    fi
}
