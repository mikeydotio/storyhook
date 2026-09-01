#!/usr/bin/env bash
#
# Runs the dashboard's Playwright suite against a real `story daemon`.
#
# Modeled on `run-tests.sh`'s isolation, and more strictly: that script's
# daemons are started in-process by `cargo test` fixtures and never touch a
# real store even if the override were dropped, because a test build refuses
# to resolve one at all (`storyhook::env::is_test_build`). This script starts
# `target/debug/story` directly — a real, non-test binary — so the isolated
# `STORYHOOK_DATA_DIR` each project run below gets is the *only* thing
# standing between this run and the developer's actual
# `~/.local/share/storyhook/store.db`. There is no second guard here.
#
# `/private/tmp` rather than `$TMPDIR`: the latter is Spotlight-indexed on
# macOS (SH-53).
#
# ONE PROJECT PER SEED (SH-335). `e2e/specs/dispatch.spec.ts` claims a seeded
# story for real (a CAS-guarded `story move ... in-progress`) and creates a
# real `git worktree` -- story.sh refuses outright to redispatch a story
# already in-progress. Running every desktop spec under two engines against
# ONE shared daemon and ONE seed, as an earlier draft of this change did,
# meant the second engine's pass hit fixtures the first engine's pass had
# already consumed, and failed for reasons that had nothing to do with the
# engine under test -- see SH-335 (`story show SH-335` carries the verdict).
# So the unit of isolation here is "one project, one seed, one daemon, one
# FAKE_TMUX_STATE" -- exactly what this script already built for a single
# engine -- and a bare `bash scripts/run-e2e.sh` with no project filter loops
# it once per project *this script derives from `e2e/playwright.config.ts`*,
# never from a hand-maintained list here. A project added to the config (the
# already-filed `mobile-webkit` follow-up, say) is covered by this loop
# without this file changing.
set -euo pipefail

cd "$(dirname "$0")/.."
repo_root="$PWD"
# shellcheck source=gate-progress.sh
. "$repo_root/scripts/gate-progress.sh"
story_bin="$repo_root/target/debug/story"
results_root="$repo_root/e2e/test-results/current"

# One artifact tree for this invocation. Each Playwright project gets its own
# output directory below, so a later project's startup no longer erases the
# screenshots, traces and error contexts from earlier failures while this
# script deliberately continues through the remaining matrix.
rm -rf "$results_root"
mkdir -p "$results_root"

# --- The engine set, derived from the config (SH-335). ------------------
#
# `e2e/playwright.config.ts` names each project with a line of the exact
# shape `      name: "<project>",` (six-space indent, inside the `projects:`
# array) -- verified against the current file, which has no other line at
# that indentation containing `name:`. A parser this narrow fails loud on a
# reformat rather than silently matching a doc comment; the floor check
# below is the fence against that.
config_project_names() {
  sed -n 's/^      name: "\([^"]*\)",$/\1/p' "$repo_root/e2e/playwright.config.ts"
}
# `mapfile`/`readarray` is bash 4+; this repo's shebang resolves to macOS's
# system bash (3.2 -- CLAUDE.md's bash-3.2 memory applies here too), which
# has neither, so the array is built with a portable read loop instead.
ALL_PROJECTS=()
while IFS= read -r _project_name; do
  ALL_PROJECTS+=("$_project_name")
done < <(config_project_names)
unset _project_name
if [ "${#ALL_PROJECTS[@]}" -lt 2 ]; then
  echo "run-e2e.sh: parsed only ${#ALL_PROJECTS[@]} project name(s) out of" >&2
  echo "  e2e/playwright.config.ts -- config_project_names()'s pattern has" >&2
  echo "  drifted from the file's actual shape, or the config lost a project." >&2
  exit 1
fi

# --- WebKit's Tab order, measured once, never assumed (SH-335). ---------
#
# `AppleKeyboardUIMode` is a macOS SYSTEM preference, not a property of the
# dashboard's DOM: at its default (0, "text boxes and lists only"), Tab
# skips buttons and links -- real Safari's own out-of-box behavior for a
# keyboard user who has never turned on System Settings -> Keyboard -> Full
# Keyboard Access. Playwright's WebKit driver inherits it, and no
# in-repo or Playwright-exposed override exists (`e2e/node_modules/
# playwright-core` has none). A test suite that silently flipped this
# machine-wide preference would make a green/red verdict on the same tree
# depend on unversioned state outside the repo -- exactly the shape CLAUDE.md
# already records a cost for (SH-306: a gate's state is not evidence of what
# actually ran). So this script only ever MEASURES it and says so loudly;
# `e2e/specs/support.ts`'s `fullKeyboardAccess()` reads the exported result
# to gate the handful of assertions that need it, unconditionally on
# `chromium`, only under an unconfigured `webkit`.
# SH-335 is the design of record -- `story show SH-335` carries the verdict.
if [ "$(uname -s)" = "Darwin" ]; then
  keyboard_ui_mode="$(defaults read -g AppleKeyboardUIMode 2>/dev/null || true)"
else
  keyboard_ui_mode=""
fi
case "$keyboard_ui_mode" in
  '' | *[!0-9]*) keyboard_ui_mode=0 ;;
esac
if [ "$keyboard_ui_mode" -ge 2 ]; then
  export E2E_FULL_KEYBOARD_ACCESS=1
else
  export E2E_FULL_KEYBOARD_ACCESS=0
fi
echo "run-e2e.sh: AppleKeyboardUIMode=$keyboard_ui_mode -> E2E_FULL_KEYBOARD_ACCESS=$E2E_FULL_KEYBOARD_ACCESS" >&2
if [ "$E2E_FULL_KEYBOARD_ACCESS" = "0" ]; then
  echo "  WebKit's Tab order will skip buttons and links (real Safari's own" >&2
  echo "  default). A handful of keyboard-reachability specs gate on this and" >&2
  echo "  will report skipped, not failed, under webkit. For full coverage," >&2
  echo "  once per machine: defaults write -g AppleKeyboardUIMode -int 2" >&2
fi

echo "run-e2e.sh: building the story binary…" >&2
cargo build --quiet

if [ ! -x "$story_bin" ]; then
  echo "run-e2e.sh: $story_bin not found after build" >&2
  exit 1
fi

if [ ! -d "$repo_root/e2e/node_modules" ] || ! (cd "$repo_root/e2e" && npx --no-install playwright --version >/dev/null 2>&1); then
  echo "run-e2e.sh: e2e/node_modules or the Playwright CLI is missing — run 'make e2e-install' first" >&2
  exit 1
fi

# --- One fully isolated run per project. ---------------------------------
#
# Everything from here down used to be this whole script's top level, run
# once. It is now a function run once PER PROJECT, each invocation in its
# own subshell so `trap cleanup EXIT` scopes to that subshell's process
# rather than the outer script's -- the same isolation a fresh `bash
# scripts/run-e2e.sh` process gave for free before this change, reused
# rather than reinvented with a RETURN trap and manual bookkeeping.
run_one_project() {
(
  project="$1"
  shift
  playwright_args=("$@")

  data_root="$(mktemp -d /private/tmp/storyhook-e2e.XXXXXX)"
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

  echo "run-e2e.sh: === project=$project ===" >&2
  cd "$repo_root/e2e"

  export STORYHOOK_DATA_DIR="$data_root/data"
  export XDG_STATE_HOME="$data_root/state"

  # STORYHOOK_STORE_PATH outranks STORYHOOK_DATA_DIR (SH-113); a developer
  # debugging a second store would have it exported, and this run must not
  # inherit it.
  unset STORYHOOK_STORE_PATH

  # Never the production port (3456), and never any port a parallel `cargo
  # test` run (or this same script's own next-project iteration) might also
  # pick -- this is a single daemon, not a pool, so an ephemeral bind is
  # enough. Site 5 of the "nothing derives this list" note in this repo's
  # CLAUDE.md.
  export STORYHOOK_DAEMON_ADDR="127.0.0.1:0"
  export STORYHOOK_PARENT_PID="$$"

  # --- Dispatch (SH-50): the daemon invokes this repo's own plugin script,
  # against the fake tmux the plugin test harness already uses -- no real
  # tmux server, no real claude, no network. Exported before `daemon start`
  # below so the daemon (and, through it, every dispatch child it spawns)
  # inherits every one of these; the readiness/confirm delays are zeroed the
  # same way test-dispatch-happy.sh zeroes them, since the fake tmux's
  # default capture fixture confirms readiness on the first poll and there
  # is nothing to wait out.
  #
  # "Inherits every one of these" is true of the STORY_* names below and
  # FALSE of the FAKE_TMUX_* ones (SH-263). A dispatch child's environment is
  # CLEARED and rebuilt from an allowlist -- PATH, HOME, TMPDIR, TMUX,
  # TMUX_PANE and any STORY_*/STORYHOOK_* name (`src/env/spawn_env.rs`,
  # SH-193) -- so a FAKE_TMUX_STATE exported here has never reached one.
  # Until SH-263 those children silently fell back to the fake's fixed
  # shared /tmp default; now the fake refuses instead, which is what made
  # the omission visible at all.
  #
  # The allowlist is a security boundary, not an oversight, so the harness
  # bridges it on its own side rather than widening it: a generated wrapper,
  # named through STORYHOOK_DISPATCH_SCRIPT (the seam every test already
  # uses to point dispatch at a stub), re-exports the fixture's knobs and
  # execs the real script. Council verdict, unanimous, recorded on SH-263.
  export PATH="$repo_root/plugins/story/tests/fakes:$PATH"
  export STORY_READY_DELAY=0
  export STORY_READY_FALLBACK_DELAY=0
  export STORY_CONFIRM_DELAY=0
  export STORY_PASTE_SETTLE_DELAY=0

  # One state directory for this project's whole run, not one per dispatch:
  # the fake models a tmux SERVER, whose session set is the one thing
  # `new-window` deliberately does not reset, and the second Alpha dispatch
  # is the only place a real daemon drives story.sh's has-session-hit
  # branch. Per-invocation directories would make every dispatch create a
  # session, and would strand each dispatch's placeholder process, which the
  # next `new-window` in the same directory reaps. The wrapper's own
  # one-writer check below is what keeps that safe -- and it is safe across
  # projects too, since each project's subshell gets its own `data_root` and
  # therefore its own `FAKE_TMUX_STATE`, run sequentially, never concurrently.
  export FAKE_TMUX_STATE="$data_root/faketmux"
  mkdir -p "$FAKE_TMUX_STATE"

  # Every FAKE_TMUX_* the harness has set, snapshotted one file per knob --
  # derived, so a knob added later crosses for free, and never interpolated
  # into the generated shell below, because a knob's value is data:
  # FAKE_TMUX_SESSIONS and FAKE_TMUX_TRANSCRIPT are deliberately multi-line,
  # and "remember to quote it correctly" is the discipline that fails on the
  # seventh knob. Anything exported AFTER this snapshot does not reach a
  # dispatch child -- `tests/store_isolation.rs` fails the build if a later
  # line tries.
  faketmux_env="$data_root/faketmux-env"
  mkdir -p "$faketmux_env"
  for _knob in $(compgen -e | grep -E '^FAKE_TMUX_[A-Z0-9_]*$' || true); do
    printf '%s' "${!_knob}" >"$faketmux_env/$_knob"
  done
  unset _knob

  # The protocol line is COPIED, not invented: `STORYHOOK_DISPATCH_SCRIPT`
  # takes dispatch.rs's `configured` branch, so `check_dispatch_protocol`
  # reads THIS file rather than story.sh, and a wrapper declaring nothing
  # reads as protocol 0 -- refused with a message blaming an out-of-date
  # plugin. Grep the real script and abort loudly rather than emit any
  # default; the parser takes the first matching line, and the wrapper has
  # exactly one.
  _real_dispatch_script="$repo_root/plugins/story/bin/story.sh"
  _dispatch_protocol="$(grep -m1 -E '^[[:space:]]*DISPATCH_PROTOCOL=' "$_real_dispatch_script" | sed 's/^[[:space:]]*//')"
  if [ -z "$_dispatch_protocol" ]; then
    echo "run-e2e.sh: no DISPATCH_PROTOCOL= line in $_real_dispatch_script" >&2
    echo "  the generated dispatch wrapper cannot declare a protocol it cannot read" >&2
    exit 1
  fi

  # Written inside data_root (0700, unpredictable, removed on exit) rather
  # than a predictable /tmp name, and 0600 with no execute bit -- the daemon
  # runs `bash <script>`, so it never needs one. It writes nothing to stdout
  # on the success path: the daemon parses a dispatch child's ENTIRE stdout
  # as one JSON object, and a stray echo would turn a good dispatch into a
  # failed one.
  export STORYHOOK_DISPATCH_SCRIPT="$data_root/dispatch-wrapper.sh"
  cat >"$STORYHOOK_DISPATCH_SCRIPT" <<WRAPPER
#!/usr/bin/env bash
# GENERATED by scripts/run-e2e.sh — regenerated every run, never committed.
# Bridges the fixture's FAKE_TMUX_* knobs across the dispatch child's cleared
# environment (SH-263), then becomes the real story.sh.
$_dispatch_protocol
set -uo pipefail

for _f in "$faketmux_env"/FAKE_TMUX_*; do
  [ -f "\$_f" ] || continue
  _name="\${_f##*/}"
  export "\$_name=\$(cat "\$_f")"
done

# One writer per state directory, CHECKED rather than assumed. The daemon
# permits several dispatch children at once (MAX_RUNNING), and two of them in
# one directory is exactly SH-263: each one's new-window clears the other's
# launched flag and pane pid, and the readiness gate then refuses a pane that
# genuinely reads as holding a shell. Liveness is queried, never inferred from
# the file, so a child killed by the daemon's own process-group timeout leaves
# no lock behind. This process becomes story.sh via exec below, so its pid stays
# the right one to publish for exactly as long as the dispatch runs.
_holders="\$FAKE_TMUX_STATE/holders"
if [ -f "\$_holders" ]; then
  while IFS= read -r _pid; do
    [ -n "\$_pid" ] || continue
    if kill -0 "\$_pid" 2>/dev/null; then
      printf 'e2e dispatch wrapper: pid %s is already driving %s — two dispatch children in one fake-tmux state directory corrupt each other (SH-263). Give each concurrent dispatch its own directory.\n' \\
        "\$_pid" "\$FAKE_TMUX_STATE" >&2
      exit 70
    fi
  done <"\$_holders"
fi
printf '%s\n' "\$\$" >"\$_holders"

exec bash "$_real_dispatch_script" "\$@"
WRAPPER
  chmod 600 "$STORYHOOK_DISPATCH_SCRIPT"
  unset _real_dispatch_script _dispatch_protocol

  case "$STORYHOOK_DATA_DIR" in
    /private/tmp/*) ;;
    *)
      echo "run-e2e.sh: refusing to run with STORYHOOK_DATA_DIR=$STORYHOOK_DATA_DIR" >&2
      echo "  the e2e harness must never point at a real storyhook store" >&2
      exit 1
      ;;
  esac

  # --- Seed four projects: Alpha/Beta with a checkout (switching between
  # them is the whole point of project-selector.spec.ts and
  # filter-persistence.spec.ts), Gamma deliberately unattached so the
  # selector's read-only path -- SH-42's defect, see the commit that fixes it
  # -- has something to exercise, and Delta (SH-208) a fourth checked-out
  # project reserved for Dispatch Auto's own dispatch target. Delta exists
  # because Alpha's exact two-story, four-empty-column shape is itself a
  # fixture other specs assert on byte-for-byte (filter-persistence.spec.ts's
  # `0 / 2`, column-visibility.spec.ts's `2 / 2` and its four-empty-columns
  # claim) -- a third Alpha story would silently break both. Seeded fresh
  # for THIS project's own daemon, never shared with another project's --
  # `dispatch.spec.ts` claims these stories for real and a second project
  # reusing them would find them already claimed (SH-335).
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
  # Dispatch is absent) even though it has no checkout to run `new` from --
  # `project new`'s own message names the slug it assigned, which is read
  # back rather than assumed, the same reasoning lib.sh's `slug_for`
  # documents for the plugin harness.
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
  # listener accepting connections is a separate fact from its process
  # having started, so the readiness poll below is a belt-and-braces check,
  # not a formality.
  echo "run-e2e.sh: starting the daemon…" >&2
  start_output="$("$story_bin" daemon start 2>&1)"
  daemon_started=1
  echo "$start_output" >&2

  # The daemon always binds loopback, and *additionally* binds its Tailscale
  # interface when `tailscale` is installed and reports one -- in which case
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
  # not just dispatch's own since SH-50), and where AA's dispatch is
  # expected to land, for the specs' own assertions. `daemon token` prints
  # the token on its own first line, then a rotation note on a second --
  # `head -n1` is the token alone.
  export DASHBOARD_TOKEN
  DASHBOARD_TOKEN="$("$story_bin" daemon token | head -n1)"

  # --- A named token minted for the suite, and the cookie name the daemon
  # publishes for it (SH-255) -- the credential `support.ts::seedToken` now
  # seeds, in place of the master token above. `story token new`'s own
  # contract is "stdout is the secret and only the secret," so no `head -n1`
  # is needed the way the master token's rotation-note second line needs one.
  export DASHBOARD_NAMED_TOKEN
  DASHBOARD_NAMED_TOKEN="$("$story_bin" token new e2e)"

  # The cookie name is per-store (`storyhook_<StoreLocation::key()>`), so the
  # suite reads it from the portfile the daemon just wrote rather than
  # recomputing the digest -- the same reasoning the portfile field's own
  # doc comment gives. Exactly one daemon.json exists under this run's
  # isolated state dir.
  portfile="$(find "$XDG_STATE_HOME/storyhook/daemons" -name daemon.json)"
  if [ -z "$portfile" ]; then
    echo "run-e2e.sh: no daemon.json found under $XDG_STATE_HOME/storyhook/daemons" >&2
    exit 1
  fi
  export DASHBOARD_COOKIE_NAME
  DASHBOARD_COOKIE_NAME="$(jq -r '.cookie_name' "$portfile")"
  if [ -z "$DASHBOARD_COOKIE_NAME" ] || [ "$DASHBOARD_COOKIE_NAME" = "null" ]; then
    echo "run-e2e.sh: $portfile named no cookie_name" >&2
    exit 1
  fi

  export DASHBOARD_ALPHA_STORY_ID="$alpha_story_id"
  export DASHBOARD_ALPHA_CHECKOUT="$seed_dir/alpha"
  export DASHBOARD_DELTA_STORY_ID="$delta_story_id"
  export DASHBOARD_DELTA_CHECKOUT="$seed_dir/delta"

  # --- Run the suite, this project only.
  #
  # Both checks below fail loudly rather than skipping: a browser suite that
  # quietly no-ops and exits 0 reads as "the dashboard was verified" when
  # nothing ran, which is the silent-fallback failure shape CLAUDE.md
  # forbids.
  export DASHBOARD_URL="$base_url"

  # --- Ask Playwright whether this project even selects a test under the
  # caller's filters, before spending the real run on it. A per-project loop
  # means a filter that only matches, say, `.mobile.spec.ts$` files gives
  # `chromium`/`webkit` nothing to run -- Playwright's own `--list` exits 1
  # and reports "Total: 0 tests" for that case, which used to be harmless
  # when every project shared one Playwright invocation (a filter matching
  # nothing under one project just meant that project contributed zero tests
  # to a run that still had others) but would now abort the whole loop over
  # a project the caller likely never meant to filter into. Skip it instead,
  # loudly. This has to run AFTER seeding and the daemon are up, not before:
  # `dispatch.spec.ts` reads its `DASHBOARD_*` fixture ids at MODULE load
  # time (`const ALPHA_STORY_ID = requiredEnv(...)`), so even `--list` -
  # which loads every matching file to enumerate its tests - throws before
  # those are exported.
  list_output="$(npx playwright test --project="$project" "${playwright_args[@]+"${playwright_args[@]}"}" --list --reporter=list 2>/dev/null || true)"
  if printf '%s\n' "$list_output" | grep -q '^Total: 0 tests'; then
    echo "run-e2e.sh: project=$project selects no tests under this filter — skipping" >&2
    gate_progress_emit_item "release gate/e2e/$project" skipped
    exit 0
  fi

  # SH-524: this project's own checklist row. `known_total` is read straight
  # back out of Playwright's own `--list` count above rather than guessed --
  # empty (never a guessed number) if that output's shape ever changes.
  # e2e/gate-progress-reporter.ts owns only the per-test "case" lines below;
  # the running/passed/failed "item" lifecycle for this project is this
  # script's alone, so the two writers never race over one event shape.
  # Portable BRE, not `\+`/`\?`: macOS's BSD sed does not support either
  # GNU extension, and this script's shebang resolves to it.
  known_total="$(printf '%s\n' "$list_output" | sed -n 's/^Total: \([0-9][0-9]*\) tests\{0,1\}.*/\1/p')"
  export STORYHOOK_GATE_PROGRESS_PATH="release gate/e2e/$project"
  if [ -n "$known_total" ]; then
    gate_progress_emit_item "$STORYHOOK_GATE_PROGRESS_PATH" running "total=$known_total"
  else
    gate_progress_emit_item "$STORYHOOK_GATE_PROGRESS_PATH" running
  fi
  e2e_start=$(date +%s)
  # The one spec in the suite that dispatches for real (`test.setTimeout` at
  # dispatch.spec.ts:125,210,278 and nowhere else that drives a real
  # dispatch; everything else stubs the route) -- consulted after the real
  # run below. This replaces the old `"$#" -eq 0` heuristic ("no CLI filter
  # was passed"), which a per-project `--project=X` invocation would always
  # defeat even on an otherwise-unfiltered run (SH-335) -- asking Playwright
  # directly is correct under any combination of project and extra filters.
  # Include the exact path and Playwright's following `:`. A suffix-only
  # match also selects story-context-menu-dispatch.spec.ts, whose requests
  # are all stubbed, then falsely fails the fake-tmux post-check below.
  dispatch_selected="$(printf '%s\n' "$list_output" | grep -c "specs/dispatch\.spec\.ts:" || true)"

  # `|| status=$?` rather than `if ! npx ...; then status=$?`: under `!`,
  # bash inverts the command's exit status, so `$?` inside that then-branch
  # is the *inverted* value -- always 0 -- and `exit "$status"` would always
  # exit 0 regardless of whether Playwright passed (SH-224).
  status=0
  npx playwright test --project="$project" --output="$results_root/$project" "${playwright_args[@]+"${playwright_args[@]}"}" || status=$?
  e2e_elapsed=$(( $(date +%s) - e2e_start ))
  gate_progress_emit_item "$STORYHOOK_GATE_PROGRESS_PATH" \
    "$([ "$status" = 0 ] && echo passed || echo failed)" "seconds=$e2e_elapsed"
  if [ "$status" -ne 0 ]; then
    echo "run-e2e.sh: project=$project failed. If the error above is about a missing browser executable, run 'make e2e-install' and retry." >&2
    exit "$status"
  fi

  # --- The dispatch children used THIS project's fake tmux, not some other
  # one.
  #
  # The check that would have caught SH-263's second half years earlier: a
  # green suite proves the dashboard dispatched, not that the fixture it
  # dispatched through was the one this script configured. FAKE_TMUX_STATE
  # was exported here for a long time while every dispatch child ignored it
  # and used the fake's fixed shared default, and nothing said so -- the
  # specs passed either way, because a shared directory serves a lone
  # dispatch perfectly well until something else writes to it.
  # `new_window_args.log` is the fake's own record of every window it was
  # asked to open, so a non-empty one here is the fixture saying, in its own
  # hand, that it was the one used. Skipped when this project+filter
  # combination selected no dispatch-driving spec at all, since then there
  # is nothing to have recorded.
  if [ "$dispatch_selected" -gt 0 ] && [ ! -s "$FAKE_TMUX_STATE/new_window_args.log" ]; then
    echo "run-e2e.sh: project=$project selected dispatch.spec.ts, but no dispatch reached this run's fake tmux state" >&2
    echo "  directory ($FAKE_TMUX_STATE/new_window_args.log is missing or empty)." >&2
    echo "  Either the spec didn't actually dispatch, or the dispatch children used a" >&2
    echo "  different fake-tmux state than the one this script configured (SH-263)." >&2
    exit 1
  fi
)
}

# --- Decide: one project (caller asked for it explicitly) or the full
# derived set (the `make test` / bare `make e2e` default). Recognizes
# `--project=NAME`; a bare `--project NAME` (space-separated) is not
# supported by this wrapper -- Playwright itself accepts both, but every
# caller in this repo (`make e2e ARGS='--project=webkit'`, this file's own
# prior triage runs) already uses the `=` form.
explicit_project=""
extra_args=()
for arg in "$@"; do
  case "$arg" in
    --project=*) explicit_project="${arg#--project=}" ;;
    *) extra_args+=("$arg") ;;
  esac
done

if [ -n "$explicit_project" ]; then
  projects_to_run=("$explicit_project")
else
  projects_to_run=("${ALL_PROJECTS[@]}")
fi

# SH-524: declare every project this run will attempt, before any of them
# start, so a not-yet-reached project (sequential -- SH-335) shows in the
# checklist as pending rather than being absent until its own turn arrives.
for project in "${projects_to_run[@]}"; do
  gate_progress_emit_item "release gate/e2e/$project" pending
done

overall_status=0
if [ -n "$explicit_project" ]; then
  run_one_project "$explicit_project" "${extra_args[@]+"${extra_args[@]}"}" || overall_status=$?
else
  for project in "${ALL_PROJECTS[@]}"; do
    run_one_project "$project" "${extra_args[@]+"${extra_args[@]}"}" || {
      status=$?
      echo "run-e2e.sh: project=$project FAILED (exit $status)" >&2
      overall_status=$status
    }
  done
fi

exit "$overall_status"
