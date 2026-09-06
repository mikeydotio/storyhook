#!/usr/bin/env bash
# Serialize all host/guest preflights, including independent checkout observers.
set -euo pipefail
[ "$#" -eq 0 ] || { echo 'usage: release-watch.sh' >&2; exit 2; }
script_dir="$(cd "$(dirname "$0")" && pwd)"
exec bash "$script_dir/machine-lock.sh" release-observer -- \
    python3 -B "$script_dir/release-observer.py" watch
