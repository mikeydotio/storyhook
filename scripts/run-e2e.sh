#!/usr/bin/env bash
#
# Runs the dashboard's Playwright suite against a real `story daemon`.
#
# Modeled on `run-tests.sh`'s isolation, and more strictly: that script's
# daemons are started in-process by `cargo test` fixtures and never touch a
# real store even if the override were dropped, because a test build refuses
# to resolve one at all (`storyhook::env::is_test_build`). This script starts
# `target/debug/story` directly — a real, non-test binary — so the isolated
# `STORYHOOK_DATA_DIR` below is the *only* thing standing between this run
# and the developer's actual `~/.local/share/storyhook/store.db`. There is no
# second guard here.
#
# `/private/tmp` rather than `$TMPDIR`: the latter is Spotlight-indexed on
# macOS (SH-53).
set -euo pipefail

cd "$(dirname "$0")/.."
repo_root="$PWD"

data_root="$(mktemp -d /private/tmp/storyhook-e2e.XXXXXX)"
story_bin="$repo_root/target/debug/story"
daemon_started=0

cleanup() {
  local status=$?
  if [ "$daemon_started" = "1" ]; then
    "$story_bin" daemon stop >/dev/null 2>&1 || true
  fi
  rm -rf "$data_root"
  exit "$status"
}
trap cleanup EXIT

export STORYHOOK_DATA_DIR="$data_root/data"
export XDG_STATE_HOME="$data_root/state"

# STORYHOOK_STORE_PATH outranks STORYHOOK_DATA_DIR (SH-113); a developer
# debugging a second store would have it exported, and this run must not
# inherit it.
unset STORYHOOK_STORE_PATH

# Never the production port (3456), and never any port a parallel `cargo
# test` run might also pick — this is a single daemon, not a pool, so an
# ephemeral bind is enough. Site 5 of the "nothing derives this list" note in
# this repo's CLAUDE.md.
export STORYHOOK_DAEMON_ADDR="127.0.0.1:0"
export STORYHOOK_PARENT_PID="$$"

case "$STORYHOOK_DATA_DIR" in
  /private/tmp/*) ;;
  *)
    echo "run-e2e.sh: refusing to run with STORYHOOK_DATA_DIR=$STORYHOOK_DATA_DIR" >&2
    echo "  the e2e harness must never point at a real storyhook store" >&2
    exit 1
    ;;
esac

echo "run-e2e.sh: building the story binary…" >&2
cargo build --quiet

if [ ! -x "$story_bin" ]; then
  echo "run-e2e.sh: $story_bin not found after build" >&2
  exit 1
fi

# --- Seed three projects: two with a checkout (switching between them is
# the whole point of the story), one deliberately unattached so the
# selector's read-only path — SH-42's defect, see the commit that fixes it —
# has something to exercise.
seed_dir="$data_root/seed"
mkdir -p "$seed_dir/alpha" "$seed_dir/beta"

echo "run-e2e.sh: seeding projects…" >&2
(
  cd "$seed_dir/alpha"
  "$story_bin" project new --prefix AA --name "Alpha Project" --no-agents-md >/dev/null
  "$story_bin" new "Wire up the auth flow" >/dev/null
  "$story_bin" new "Fix the flaky upload test" >/dev/null
)
(
  cd "$seed_dir/beta"
  "$story_bin" project new --prefix BB --name "Beta Project" --no-agents-md >/dev/null
  "$story_bin" new "Draft the release notes" >/dev/null
)
"$story_bin" project new --prefix GA --name "Gamma Archive" --no-attach --no-agents-md >/dev/null

# --- Start the daemon and discover the port it actually bound. `daemon
# start` blocks until the daemon reports ready (or times out), but its
# listener accepting connections is a separate fact from its process having
# started, so the readiness poll below is a belt-and-braces check, not a
# formality.
echo "run-e2e.sh: starting the daemon…" >&2
start_output="$("$story_bin" daemon start 2>&1)"
daemon_started=1
echo "$start_output" >&2

# The daemon always binds loopback, and *additionally* binds its Tailscale
# interface when `tailscale` is installed and reports one — in which case
# `dashboard_url()`, and so this message, advertises the tailnet MagicDNS
# name instead of 127.0.0.1. The port is what this script needs; targeting
# loopback explicitly (rather than whatever host got printed) keeps the
# suite from depending on this machine's tailnet or DNS resolution at all.
port="$(printf '%s' "$start_output" | sed -nE 's#.*running at [a-zA-Z]+://[^[:space:]]+:([0-9]+) .*#\1#p')"
if [ -z "$port" ]; then
  echo "run-e2e.sh: could not parse the daemon's port from: $start_output" >&2
  exit 1
fi

base_url="http://127.0.0.1:$port"
deadline=$((SECONDS + 15))
until curl -sf -o /dev/null "$base_url/api/repos"; do
  if [ "$SECONDS" -ge "$deadline" ]; then
    echo "run-e2e.sh: $base_url never answered GET /api/repos within 15s" >&2
    exit 1
  fi
  sleep 0.2
done
echo "run-e2e.sh: dashboard live at $base_url" >&2

# --- Run the suite.
#
# Both checks fail loudly rather than skipping: a browser suite that quietly
# no-ops and exits 0 reads as "the dashboard was verified" when nothing ran,
# which is the silent-fallback failure shape CLAUDE.md forbids.
export DASHBOARD_URL="$base_url"
cd "$repo_root/e2e"

if [ ! -d node_modules ] || ! npx --no-install playwright --version >/dev/null 2>&1; then
  echo "run-e2e.sh: e2e/node_modules or the Playwright CLI is missing — run 'make e2e-install' first" >&2
  exit 1
fi

# Whether the chromium *browser binary* (as opposed to the CLI above) is
# installed is deliberately not pre-checked: Playwright's own failure for a
# missing browser already names the fix imperatively, and duplicating that
# detection here would be a second copy to keep in sync with how Playwright
# reports it. The hint below only adds where that fix lives in this repo.
if ! npx playwright test "$@"; then
  status=$?
  echo "run-e2e.sh: the suite failed. If the error above is about a missing browser executable, run 'make e2e-install' and retry." >&2
  exit "$status"
fi
