#!/usr/bin/env bash
set -euo pipefail
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$script_dir/python-runtime.sh"
storyhook_python_init || { printf '%s\n' "$STORYHOOK_PYTHON_ERROR" >&2; exit 125; }
exec "$STORYHOOK_PYTHON" -B "$script_dir/purge-detached-products.py" "$@"
