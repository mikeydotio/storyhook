#!/usr/bin/env bash
# Read-only advisory: no network, guest startup, or gate receipt.
set -euo pipefail
[ "$#" -eq 0 ] || { echo 'usage: release-status.sh' >&2; exit 2; }
script_dir="$(cd "$(dirname "$0")" && pwd)"
exec python3 -B "$script_dir/release-observer.py" status
