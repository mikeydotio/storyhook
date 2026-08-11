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

# --- Dispatch (SH-50): the daemon invokes this repo's own plugin script,
# against the fake tmux the plugin test harness already uses — no real tmux
# server, no real claude, no network. Exported before `daemon start` below so
# the daemon (and, through it, every dispatch child it spawns) inherits every
# one of these; the readiness/confirm delays are zeroed the same way
# test-dispatch-happy.sh zeroes them, since the fake tmux's default capture
# fixture confirms readiness on the first poll and there is nothing to wait
# out.
export STORYHOOK_DISPATCH_SCRIPT="$repo_root/plugin/claude-code/bin/story.sh"
export PATH="$repo_root/plugin/claude-code/tests/fakes:$PATH"
export FAKE_TMUX_STATE="$data_root/faketmux"
export STORY_READY_DELAY=0
export STORY_READY_FALLBACK_DELAY=0
export STORY_CONFIRM_DELAY=0
export STORY_PASTE_SETTLE_DELAY=0

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

# --- Seed four projects: Alpha/Beta with a checkout (switching between
# them is the whole point of project-selector.spec.ts and
# filter-persistence.spec.ts), Gamma deliberately unattached so the
# selector's read-only path — SH-42's defect, see the commit that fixes it
# — has something to exercise, and Delta (SH-208) a fourth checked-out
# project reserved for Dispatch Auto's own dispatch target. Delta exists
# because Alpha's exact two-story, four-empty-column shape is itself a
# fixture other specs assert on byte-for-byte (filter-persistence.spec.ts's
# `0 / 2`, column-visibility.spec.ts's `2 / 2` and its four-empty-columns
# claim) -- a third Alpha story would silently break both.
seed_dir="$data_root/seed"
mkdir -p "$seed_dir/alpha" "$seed_dir/beta" "$seed_dir/delta"

echo "run-e2e.sh: seeding projects…" >&2

# A real git repo, not just a directory: neither project-selector.spec.ts
# nor the pre-SH-50 suite needed one (a checkout is only a recorded path
# until something reads it as a repository), but dispatch's worktree
# creation does -- confirmed the hard way when AA-1's checkout wasn't one
# and story.sh refused with exactly that message. No origin is configured;
# story.sh's own base-resolution tolerates that (falls back to HEAD), so
# this is the minimum dispatch actually needs.
init_git_repo() {
  git init -q -b main
  git config user.email "e2e@storyhook.test"
  git config user.name "storyhook e2e"
  echo "# $(basename "$PWD")" >README.md
  git add README.md
  git commit -q -m "init"
}

(
  cd "$seed_dir/alpha"
  init_git_repo
  "$story_bin" project new --prefix AA --name "Alpha Project" --no-agents-md >/dev/null
  "$story_bin" new "Wire up the auth flow" --json | jq -r '.story.story.id' >"$data_root/alpha-story-id"
  "$story_bin" new "Fix the flaky upload test" >/dev/null
  # Alpha-only state, deliberately absent from Beta and Gamma: filter-
  # persistence.spec.ts needs one project's state vocabulary to be a value
  # the *next* project can't possibly have, to prove a carried-over state
  # filter gets pruned rather than silently hiding every story in whatever
  # project it's carried into.
  "$story_bin" state add review --super OPEN >/dev/null
)
alpha_story_id="$(cat "$data_root/alpha-story-id")"
(
  cd "$seed_dir/beta"
  init_git_repo
  "$story_bin" project new --prefix BB --name "Beta Project" --no-agents-md >/dev/null
  "$story_bin" new "Draft the release notes" >/dev/null
)
# Gamma needs at least one story too (SH-50's AC1 spec opens it to confirm
# Dispatch is absent) even though it has no checkout to run `new` from —
# `project new`'s own message names the slug it assigned, which is read back
# rather than assumed, the same reasoning lib.sh's `slug_for` documents for
# the plugin harness.
gamma_message="$("$story_bin" project new --prefix GA --name "Gamma Archive" --no-attach --no-agents-md --json | jq -r '.message')"
gamma_slug="$(printf '%s' "$gamma_message" | sed -n 's/.*`\([a-z0-9-]*\)`.*/\1/p' | head -n1)"
if [ -z "$gamma_slug" ]; then
  echo "run-e2e.sh: could not read Gamma Archive's slug from: $gamma_message" >&2
  exit 1
fi
"$story_bin" --project "$gamma_slug" new "Archived idea" >/dev/null
(
  cd "$seed_dir/delta"
  init_git_repo
  "$story_bin" project new --prefix DD --name "Delta Project" --no-agents-md >/dev/null
  # SH-208's own dispatch target, in a project of its own -- Alpha's exact
  # two-story shape is a fixture filter-persistence.spec.ts and
  # column-visibility.spec.ts assert on byte-for-byte, so Dispatch Auto's
  # e2e test gets a project nothing else looks at rather than growing it.
  "$story_bin" new "Roll out the new onboarding flow" --json | jq -r '.story.story.id' >"$data_root/delta-story-id"
)
delta_story_id="$(cat "$data_root/delta-story-id")"

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
# `GET /` rather than `GET /api/repos` (SH-187: the latter now requires the
# daemon's bearer token, which this probe has no reason to carry -- it is
# asking "is the HTTP server up", not "is it authenticated").
until curl -sf -o /dev/null "$base_url/"; do
  if [ "$SECONDS" -ge "$deadline" ]; then
    echo "run-e2e.sh: $base_url never answered GET / within 15s" >&2
    exit 1
  fi
  sleep 0.2
done
echo "run-e2e.sh: dashboard live at $base_url" >&2

# --- The daemon's bearer token (SH-187: every /api/** route requires it,
# not just dispatch's own since SH-50), and where AA's dispatch is expected
# to land, for the specs' own assertions. `daemon token` prints the token on
# its own first line, then a rotation note on a second -- `head -n1` is the
# token alone.
export DASHBOARD_TOKEN
DASHBOARD_TOKEN="$("$story_bin" daemon token | head -n1)"
export DASHBOARD_ALPHA_STORY_ID="$alpha_story_id"
export DASHBOARD_ALPHA_CHECKOUT="$seed_dir/alpha"
export DASHBOARD_DELTA_STORY_ID="$delta_story_id"
export DASHBOARD_DELTA_CHECKOUT="$seed_dir/delta"

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
#
# `|| status=$?` rather than `if ! npx ...; then status=$?`: under `!`, bash
# inverts the command's exit status, so `$?` inside that then-branch is the
# *inverted* value — always 0 — and `exit "$status"` would always exit 0
# regardless of whether Playwright passed (SH-224).
status=0
npx playwright test "$@" || status=$?
if [ "$status" -ne 0 ]; then
  echo "run-e2e.sh: the suite failed. If the error above is about a missing browser executable, run 'make e2e-install' and retry." >&2
  exit "$status"
fi
