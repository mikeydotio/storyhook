#!/usr/bin/env bash
#
# Project verification views (SH-662), alongside the SH-590 activity journal.
# Attach to storyhook-verifier on the default tmux server. Each canonical Git
# common directory owns a verification window; no phase replaces the journal.
# Contract and decisions: docs/spec/verifier-windows.md and story SH-662.
#
# SH-545's council established the stable session, default server, independent
# log readers, literal argv and best-effort behavior. SH-662 changes window
# ownership because SH-648 permits different projects to verify concurrently.
# Windows never use story IDs or dispatch sockets, so leased dispatch cleanup
# cannot reach them. Panes READ independently written logs, never pipe the
# gate's stdout/stderr (the descendant-held-pipe hazard in spawn_inventory).
#
# CONTRACT: sourced by verify-pr.sh from its own directory -- the bundle the
# daemon projects out of its binary (SH-654) -- so it is always present beside
# its caller; a missing copy is a packaging defect verify-pr.sh refuses by
# name, never a reason to degrade to no-op stubs.
#
# The kill switch: STORYHOOK_VERIFIER_MIRROR=0 prohibits every tmux call.
# Banners still enter the activity journal. The switch ships
# in storyhook::env::test_environment::TEST_ENVIRONMENT (`story help
# test-environment`) alongside every other variable that stops a storyhook
# process reaching a developer's own real state, so `scripts/test-env.sh`'s
# `storyhook_isolate` sets it for every harness that isolates a run the
# same way it already does for the rest of that table.

VERIFIER_WINDOW_SESSION="storyhook-verifier"

# True (0) unless explicitly disabled. Checked first in every entry point
# below so a disabled mirror never even probes for tmux.
verifier_window_enabled() {
    [ "${STORYHOOK_VERIFIER_MIRROR:-1}" != "0" ]
}

# Match the daemon's own environment boundary for standalone callers too.
# The subshell preserves the caller's dispatch context after this command.
verifier_window_tmux() (
    unset TMUX TMUX_PANE
    command tmux "$@"
)

# Resolve the same identity as machine-lock.sh's project key. Derivation stays
# in the caller's repository; the pane itself always starts in stable HOME.
# A failed identity is not permission to share some other project's window.
verifier_window_project() {
    local common project hash label
    common="$(git rev-parse --git-common-dir 2>/dev/null)" || return 1
    project="$(cd "$common" 2>/dev/null && pwd -P)" || return 1
    hash="$(printf '%s' "$project" | git hash-object --stdin 2>/dev/null)" || return 1
    [ -n "$hash" ] || return 1
    label="$project"
    [ "${project##*/}" != .git ] || label="${project%/*}"
    label="$(printf '%s' "${label##*/}" | LC_ALL=C tr -c 'A-Za-z0-9_-' '-')" || return 1
    printf 'verification-%s-%s\n' "$label" "$hash"
}

# verifier_window_ensure <window-name>
#
# The server serializes new-window -S, eliminating check-then-create races.
# A session-creation loser must confirm the winning session before continuing.
# The trailing colon requests a free index so -S reuses by NAME, not index.
verifier_window_ensure() {
    local name="$1" target="=${VERIFIER_WINDOW_SESSION}:=$1"
    verifier_window_enabled || return 1
    command -v tmux >/dev/null 2>&1 || return 1
    if ! verifier_window_tmux has-session -t "=$VERIFIER_WINDOW_SESSION" 2>/dev/null; then
        verifier_window_tmux new-session -d -c "$HOME" -s "$VERIFIER_WINDOW_SESSION" -n "$name" \
            sleep 2147483647 2>/dev/null \
            || verifier_window_tmux has-session -t "=$VERIFIER_WINDOW_SESSION" 2>/dev/null || return 1
    fi
    verifier_window_tmux new-window -d -S -c "$HOME" -t "=${VERIFIER_WINDOW_SESSION}:" -n "$name" \
        sleep 2147483647 2>/dev/null || return 1
    verifier_window_tmux set-window-option -t "$target" automatic-rename off >/dev/null 2>&1 || return 1
    verifier_window_tmux set-window-option -t "$target" allow-rename off >/dev/null 2>&1 || return 1
}

# verifier_window_tail <log-path>
#
# Points the pane at a live, read-only follow of <log-path>, from its
# start (so a freshly attached operator sees the whole run so far, not
# only new lines). <log-path> reaches tmux and then `tail` as one argv
# element each hop -- never a shell string -- so it is exact regardless of
# spaces or shell-special characters.
verifier_window_tail() {
    local log="$1" name
    verifier_window_enabled || return 1
    name="$(verifier_window_project)" || return 1
    verifier_window_ensure "$name" || return 1
    verifier_window_tmux respawn-pane -k -c "$HOME" -t "=${VERIFIER_WINDOW_SESSION}:=$name" \
        tail -n +1 -F "$log" 2>/dev/null || return 1
}

# verifier_window_banner <text>
#
# Points the pane at a static line of <text>, held open indefinitely.
# <text> reaches the pane as `bash -c`'s own $1 -- a positional parameter,
# never interpolated into the -c script string -- so arbitrary punctuation
# in <text> cannot be reinterpreted as shell syntax.
verifier_window_banner() {
    local text="$1" name
    if [ -n "${STORYHOOK_ACTIVITY_LOG_DIR:-}" ]; then
        printf '%s\n' "$text" >&2
    fi
    verifier_window_enabled || return 1
    name="$(verifier_window_project)" || return 1
    verifier_window_ensure "$name" || return 1
    # shellcheck disable=SC2016 # deliberate: $1 must NOT expand here -- it
    # is bash -c's own positional parameter, populated at exec time from
    # the argv element that follows, never from this shell's own $1.
    verifier_window_tmux respawn-pane -k -c "$HOME" -t "=${VERIFIER_WINDOW_SESSION}:=$name" \
        bash -c 'printf "%s\n" "$1"; exec sleep 2147483647' verifier-banner "$text" \
        2>/dev/null || return 1
}

# The daemon uses its own executable and explicit store; neither is shell code.
verifier_window_logs() {
    local store="$2" name
    verifier_window_enabled || return 1
    # Resolve relative manual invocations before changing the reader's cwd.
    case "$store" in /*) ;; *) store="$PWD/$store" ;; esac
    # The journal can start outside Git. Canonicalize file symlinks too, then
    # hash path bytes independently of any enclosing repository's object format.
    name="$(python3 - "$store" <<'PY'
import hashlib
import os
import re
import sys

store = os.path.realpath(sys.argv[1], strict=True)
label = re.sub(r"[^A-Za-z0-9_-]", "-", os.path.basename(os.path.dirname(store)))
digest = hashlib.sha256(os.fsencode(store)).hexdigest()
print(f"activity-{label}-{digest}")
PY
    )" || return 1
    verifier_window_ensure "$name" || return 1
    verifier_window_tmux respawn-pane -k -c "$HOME" -t "=${VERIFIER_WINDOW_SESSION}:=$name" \
        "$1" --store-path "$store" daemon logs --follow || return 1
}

# Direct-invocation form, for manual smoke testing:
#   bash scripts/verify-window.sh banner "text"
#   bash scripts/verify-window.sh tail /path/to/log
#   bash scripts/verify-window.sh logs /path/to/story /path/to/store.db
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    set -u
    case "${1:-}" in
    banner) verifier_window_banner "${2:?usage: verify-window.sh banner <text>}" ;;
    tail) verifier_window_tail "${2:?usage: verify-window.sh tail <log-path>}" ;;
    logs) verifier_window_logs "${2:?story binary required}" "${3:?store path required}" ;;
    *)
        echo "usage: verify-window.sh banner <text> | tail <log-path> | logs <story-binary> <store-path>" >&2
        exit 64
        ;;
    esac
fi
