#!/bin/bash
# SH-858: choose Python independently of the client that started the daemon.
# Source, call storyhook_python_init, and report STORYHOOK_PYTHON_ERROR through
# the caller's protocol. Executed directly, this wraps a command after `--`.

_storyhook_python_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" || { return 1 2>/dev/null || exit 1; }

# Explicit candidates make discovery testable without changing host installs.
# Only storyhook_python_init chooses the production candidate list.
storyhook_python_select() {
    local candidate probe status version executable failures=""
    for candidate in "$@"; do
        case "$candidate" in
        (/*) ;;
        (*) failures="${failures}${candidate:-<empty>}: not an absolute executable path
"; continue ;;
        esac
        if [ ! -f "$candidate" ] || [ ! -x "$candidate" ]; then
            failures="${failures}$candidate: missing or not executable
"
            continue
        fi
        status=0
        probe="$(STORYHOOK_PYTHON_PROBE=1 "$candidate" -I -S -c '
import os, sys
print("%d.%d.%d" % sys.version_info[:3])
# Keep the venv path: dereferencing its symlink discards the environment.
print(os.path.abspath(sys.executable))
raise SystemExit(0 if sys.version_info.major == 3 and sys.version_info >= (3, 11) else 2)
' 2>&1)" || status=$?
        version="${probe%%$'\n'*}"
        executable="${probe#*$'\n'}"
        if [ "$status" -eq 0 ] && [[ "$version" =~ ^3\.([0-9]{1,3})\.[0-9]+$ ]] \
            && [ "${BASH_REMATCH[1]}" -ge 11 ] \
            && [[ "$executable" = /* && "$executable" != *$'\n'* ]] \
            && [ -f "$executable" ] && [ -x "$executable" ] \
            && [ ! "$executable" -ef "$_storyhook_python_dir/python-bin/python3" ]; then
            printf '%s\n' "$executable"
            return 0
        fi
        failures="${failures}$candidate: exit $status; ${probe:-no interpreter identity returned}
"
    done
    printf 'Python runtime requires Python >=3.11 and <4. Attempted:\n%sSet STORYHOOK_PYTHON to an absolute supported interpreter path.\n' "$failures" >&2
    return 1
}

# Pin both owned calls and PATH-based descendants, without reordering other tools.
storyhook_python_init() {
    local selected launcher="$_storyhook_python_dir/python-bin/python3"
    STORYHOOK_PYTHON_ERROR=""
    if [ "${STORYHOOK_PYTHON+x}" = x ]; then
        selected="$(storyhook_python_select "$STORYHOOK_PYTHON" 2>&1)" || {
            STORYHOOK_PYTHON_ERROR="$selected"
            return 1
        }
    else
        selected="$(storyhook_python_select /opt/homebrew/bin/python3 \
            /usr/local/bin/python3 /usr/bin/python3 /bin/python3 2>&1)" || {
            STORYHOOK_PYTHON_ERROR="$selected"
            return 1
        }
    fi
    if [ ! -x "$launcher" ]; then
        STORYHOOK_PYTHON_ERROR="Python runtime launcher missing: $launcher"
        return 1
    fi
    export STORYHOOK_PYTHON="$selected"
    case "${PATH:-}" in
    ("${launcher%/*}" | "${launcher%/*}:"*) ;;
    (*)
        if [ -n "${PATH:-}" ]; then
            export PATH="$_storyhook_python_dir/python-bin:$PATH"
        else
            export PATH="$_storyhook_python_dir/python-bin"
        fi
        ;;
    esac
    hash -r
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    storyhook_python_init || { printf '%s\n' "$STORYHOOK_PYTHON_ERROR" >&2; exit 2; }
    [ "${1:-}" = -- ] && [ "$#" -gt 1 ] || {
        printf 'usage: python-runtime.sh -- command [args...]\n' >&2
        exit 2
    }
    shift
    exec "$@"
fi
