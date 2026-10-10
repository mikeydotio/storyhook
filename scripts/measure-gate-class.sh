#!/usr/bin/env bash
# Local CLI entry into the verifier's owned measurement operation.
set -euo pipefail
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$script_dir/python-runtime.sh"
storyhook_python_init || { printf '%s\n' "$STORYHOOK_PYTHON_ERROR" >&2; exit 2; }
exec "$STORYHOOK_PYTHON" -B "$script_dir/gate_measurement.py" prepare "$@"
