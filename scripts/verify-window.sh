#!/usr/bin/env bash
# Verifier phase diagnostics. The daemon owns the persistent project reader;
# gate scripts and fixtures must never allocate terminal resources (SH-748).
# Sourced from the daemon-owned verifier bundle by verify-pr.sh.

verifier_window_banner() {
    printf '%s\n' "$1" >&2
}

verifier_window_tail() {
    printf 'verification output: %s\n' "$1" >&2
}

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    case "${1:-}" in
        banner) verifier_window_banner "${2:?text required}" ;;
        tail) verifier_window_tail "${2:?log path required}" ;;
        *) printf '%s\n' 'usage: verify-window.sh banner <text> | tail <log-path>' >&2; exit 64 ;;
    esac
fi
