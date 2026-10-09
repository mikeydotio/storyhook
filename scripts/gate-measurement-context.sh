#!/usr/bin/env bash
# A measured gate executes every leg but cannot publish reusable evidence.
# The verifier's durable session identity, not this variable, grants that mode.
gate_measurement=0
if [ "${STORYHOOK_GATE_MEASUREMENT+x}" = x ]; then
    _measurement_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" || exit 2
    . "$_measurement_dir/python-runtime.sh" || exit 2
    storyhook_python_init || { printf '%s\n' "$STORYHOOK_PYTHON_ERROR" >&2; exit 2; }
    "$STORYHOOK_PYTHON" "$_measurement_dir/gate_measurement_context.py" || exit 2
    gate_measurement=1
fi
