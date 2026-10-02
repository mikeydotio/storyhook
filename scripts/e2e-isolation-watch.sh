#!/bin/bash
# Use the same supported Python runtime as the production browser harness.
set -euo pipefail
script_dir="$(cd "$(dirname "$0")" && pwd)"
. "$script_dir/python-runtime.sh"
storyhook_python_init || { printf '%s\n' "$STORYHOOK_PYTHON_ERROR" >&2; exit 2; }
exec python3 -B "$script_dir/e2e-isolation-watch.py" "$@"
