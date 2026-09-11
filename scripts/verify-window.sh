#!/usr/bin/env bash
#
# The daemon activity view (SH-590), extending the SH-545 verifier mirror.
# The daemon starts a continuous `story daemon logs --follow` reader;
# verifier phases leave that reader in place and emit their banners to stderr.
# Standalone verifier invocations retain the original tail/banner fallback.
# Current contract: docs/spec/activity-log.md.
#
# DESIGN OF RECORD: a /council-vote convened for SH-545 on 2026-09-04
# settled the choices below (unanimous ranked-choice runoff). Its own
# working trail does not survive worktree teardown, per this project's own
# standing rule -- the verdict and full reasoning are recorded as a
# comment on the story itself (`story show SH-545`):
#
#   - One FIXED session (storyhook-verifier) and one FIXED window
#     (verification) on tmux's DEFAULT server, reused across every
#     candidate the strictly-serial verifier (D4) processes in turn --
#     never a candidate's own dispatch socket
#     (VerificationCandidate.cleanup_lease.tmux.socket_path). A per-
#     candidate socket would relocate a human's attach point to a
#     different tmux server every time verification advances, defeating
#     the point of a live-watch feature for a possibly-hung run.
#   - Because the mirror never touches a story's own recorded dispatch
#     socket, plugins/story/bin/story.sh's leased `reap` (which kills any
#     window named after the story id on THAT story's socket) structurally
#     cannot reach this window regardless of its name -- and the name is
#     still a fixed constant, never a story id, as defense in depth.
#   - Content is a READ of an independently written log -- never a `tee`
#     or any pipe fused to the gate subprocess's own stdout/stderr, which
#     would reintroduce the "descendant holds the daemon's output pipe
#     forever" hazard tests/spawn_inventory.rs exists to prevent.
#   - Every path/text a caller supplies travels to tmux as its own argv
#     element (multi-word `respawn-pane`/`new-window`, or a script's own
#     positional parameter), never interpolated into a shell-command
#     STRING -- verified empirically against tmux 3.7c with a path
#     containing a space and text containing an apostrophe, em-dash and
#     semicolon. This is the SH-493 quoting-hazard class, closed by
#     construction rather than by escaping.
#   - Entirely best-effort and non-fatal, the same posture SH-524's
#     gate-progress journal already established: a missing/broken tmux
#     degrades to a returned failure a caller ignores, never an aborted
#     verification.
#
# WHEN MULTIPLE STORES/DAEMONS SHARE ONE MACHINE (SH-113 store isolation
# explicitly permits this): the session name is machine-wide, not
# store-scoped, so two daemons share one pane -- whichever last started
# wins the display. Deliberate simplicity for
# a single-operator, single-terminal workflow, not a defect: an operator
# checking in wants one place to look, and D4 makes each PROJECT's own
# verification serial, so concurrent writers are two different projects
# verifying at the same instant. STATED LIMIT since SH-648 (one verifier per
# project): that case is no longer rare on a machine with two active
# projects, and this one pane then shows whichever verification last
# started. The mirror is best effort and non-fatal, so nothing is lost but
# the view; a window per project inside the fixed session is the obvious
# repair and a separate story, since SH-545's council chose one window.
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
VERIFIER_WINDOW_NAME="verification"

# True (0) unless explicitly disabled. Checked first in every entry point
# below so a disabled mirror never even probes for tmux.
verifier_window_enabled() {
    [ "${STORYHOOK_VERIFIER_MIRROR:-1}" != "0" ]
}

# Idempotent: creates the fixed session/window if either is missing, reuses
# both otherwise. Never prints anything -- a caller degrades silently to
# no mirror on any failure, exactly like SH-524's own journal-write step.
verifier_window_ensure() {
    verifier_window_enabled || return 1
    command -v tmux >/dev/null 2>&1 || return 1
    if ! tmux has-session -t "$VERIFIER_WINDOW_SESSION" 2>/dev/null; then
        tmux new-session -d -c "$HOME" -s "$VERIFIER_WINDOW_SESSION" -n "$VERIFIER_WINDOW_NAME" \
            'sleep 2147483647' 2>/dev/null || return 1
    elif ! tmux list-windows -t "$VERIFIER_WINDOW_SESSION" -F '#{window_name}' 2>/dev/null \
            | grep -qx "$VERIFIER_WINDOW_NAME"; then
        tmux new-window -d -c "$HOME" -t "${VERIFIER_WINDOW_SESSION}:" -n "$VERIFIER_WINDOW_NAME" \
            'sleep 2147483647' 2>/dev/null || return 1
    fi
    tmux set-window-option -t "${VERIFIER_WINDOW_SESSION}:${VERIFIER_WINDOW_NAME}" \
        automatic-rename off >/dev/null 2>&1 || true
    tmux set-window-option -t "${VERIFIER_WINDOW_SESSION}:${VERIFIER_WINDOW_NAME}" \
        allow-rename off >/dev/null 2>&1 || true
    return 0
}

# verifier_window_tail <log-path>
#
# Points the pane at a live, read-only follow of <log-path>, from its
# start (so a freshly attached operator sees the whole run so far, not
# only new lines). <log-path> reaches tmux and then `tail` as one argv
# element each hop -- never a shell string -- so it is exact regardless of
# spaces or shell-special characters.
verifier_window_tail() {
    local log="$1"
    # The daemon now owns a continuous journal view. A phase must not replace it.
    [ -z "${STORYHOOK_ACTIVITY_LOG_DIR:-}" ] || return 0
    verifier_window_ensure || return 1
    tmux respawn-pane -k -c "$HOME" -t "${VERIFIER_WINDOW_SESSION}:${VERIFIER_WINDOW_NAME}" \
        tail -n +1 -F "$log" 2>/dev/null || return 1
}

# verifier_window_banner <text>
#
# Points the pane at a static line of <text>, held open indefinitely.
# <text> reaches the pane as `bash -c`'s own $1 -- a positional parameter,
# never interpolated into the -c script string -- so arbitrary punctuation
# in <text> cannot be reinterpreted as shell syntax.
verifier_window_banner() {
    local text="$1"
    if [ -n "${STORYHOOK_ACTIVITY_LOG_DIR:-}" ]; then
        printf '%s\n' "$text" >&2
        return 0
    fi
    verifier_window_ensure || return 1
    # shellcheck disable=SC2016 # deliberate: $1 must NOT expand here -- it
    # is bash -c's own positional parameter, populated at exec time from
    # the argv element that follows, never from this shell's own $1.
    tmux respawn-pane -k -c "$HOME" -t "${VERIFIER_WINDOW_SESSION}:${VERIFIER_WINDOW_NAME}" \
        bash -c 'printf "%s\n" "$1"; exec sleep 2147483647' verifier-banner "$text" \
        2>/dev/null || return 1
}

# The daemon uses its own executable and explicit store; neither is shell code.
verifier_window_logs() {
    verifier_window_ensure || return 1
    tmux respawn-pane -k -c "$HOME" -t "${VERIFIER_WINDOW_SESSION}:${VERIFIER_WINDOW_NAME}" \
        "$1" --store-path "$2" daemon logs --follow || return 1
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
