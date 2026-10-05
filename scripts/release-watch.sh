#!/usr/bin/env bash
# Serialize all host/guest preflights, including independent checkout observers.
set -euo pipefail
[ "$#" -eq 0 ] || { echo 'usage: release-watch.sh' >&2; exit 2; }
script_dir="$(cd "$(dirname "$0")" && pwd)"
# The observer admits its preflight as a host admission root inside this lock
# (SH-869, decision D7); an enabled authority's drain allowance is the lock's
# termination grace, and a disabled one leaves the lock's own grace alone.
grace=()
drain="$("$script_dir/host-admit.py" --drain-seconds)"
[ "$drain" -gt 0 ] && grace=(--termination-grace "$drain")
exec bash "$script_dir/machine-lock.sh" "${grace[@]+"${grace[@]}"}" release-observer -- \
    python3 -B "$script_dir/release-observer.py" watch
