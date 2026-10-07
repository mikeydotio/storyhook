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
# THE BINARY IS LEASED, NEVER RUN FROM CARGO'S OWN PATH (SH-635). After the
# build, `target/debug/story` is hard-linked into a run-owned lease
# (`scripts/binary-lease.sh`, the shell twin of SH-532's `story_binary()`)
# and `$story_bin` is that lease -- for the daemon, for every seeding and
# cleanup call here, and, through `DASHBOARD_STORY_BIN`, for every CLI call a
# spec makes (`e2e/specs/support.ts`'s `storyBinary()` is the specs' one
# door). Cargo replaces the artifact by rename, so any `cargo build|test|check`
# in this checkout while a run is live leaves the lease's inode untouched.
# Before the lease, one `cargo test` beside a live chromium run changed the
# daemon's `(exe, exe_mtime)` identity, the next CLI call replaced the daemon
# on a new port, and 167 tests failed in 25 minutes with nothing naming why.
#
# `/private/tmp` rather than `$TMPDIR`: the latter is Spotlight-indexed on
# macOS (SH-53).
#
# ONE PROJECT PER SEED (SH-335). `e2e/specs/dispatch.spec.ts` and
# `e2e/specs/engine.spec.ts` claim seeded stories for real (a CAS-guarded
# transition into the active state) and create real git worktrees -- story.sh
# refuses outright to redispatch a story already in-progress. Running every
# desktop spec under two engines against
# ONE shared daemon and ONE seed, as an earlier draft of this change did,
# meant the second engine's pass hit fixtures the first engine's pass had
# already consumed, and failed for reasons that had nothing to do with the
# engine under test -- see SH-335 (`story show SH-335` carries the verdict).
# So the unit of isolation here is "one Playwright invocation, one seed, one
# daemon, one FAKE_TMUX_STATE" -- exactly what this script already built for a
# single engine -- and a bare `bash scripts/run-e2e.sh` with no project filter
# runs every project *this script derives from `e2e/playwright.config.ts`*,
# never from a hand-maintained list here. A project added to the config is
# covered without this file changing.
#
# SLICES, AT THE SAME TIME (SH-792). One project after another, that unit made
# this leg about 41 minutes. The selection is now listed once, split by
# `scripts/e2e-pool.sh` into slices -- one project and a Playwright
# `--test-list` of whole spec files each -- and up to STORYHOOK_E2E_JOBS slices
# run at once, each through its own `run_one_project`: the same isolation,
# smaller. `workers: 1` inside each invocation stays a council decision
# (SH-627); what changed is how many invocations run at once.
set -euo pipefail

# Refuse incompatible partitions before a build or artifact mutation.
isolate_files=0
caller_partition=0
explicit_project=""
extra_args=()
for arg in "$@"; do
  case "$arg" in
    --isolate-files) isolate_files=1 ;;
    --project=*) explicit_project="${arg#--project=}" ;;
    --shard | --shard=* | --test-list | --test-list=*)
      caller_partition=1
      extra_args+=("$arg")
      ;;
    *) extra_args+=("$arg") ;;
  esac
done
if [ "$isolate_files" = 1 ] && [ "$caller_partition" = 1 ]; then
  echo "run-e2e.sh: cannot combine --isolate-files with --shard or --test-list" >&2
  exit 2
fi
if [ "$isolate_files" = 1 ]; then
  for arg in "${extra_args[@]+"${extra_args[@]}"}"; do
    case "$arg" in
      --config | --config=* | -c | -c?* | --reporter | --reporter=* | --output | --output=* | --workers | --workers=* | -j | -j?* | --retries | --retries=* | --fully-parallel | --repeat-each | --repeat-each=* | --list | --ui | --ui=* | --debug)
        echo "run-e2e.sh: cannot combine --isolate-files with behavior override $arg" >&2
        exit 2
        ;;
    esac
  done
fi

. "$(dirname "${BASH_SOURCE[0]}")/python-runtime.sh"
storyhook_python_init || { printf '%s\n' "$STORYHOOK_PYTHON_ERROR" >&2; exit 2; }

# Playwright forces FORCE_COLOR=1 in workers. Translate NO_COLOR before Node
# starts: DEBUG_COLORS=0 makes Playwright strip ANSI from worker output, while
# dropping the conflicting flag prevents Node's warning in every new worker.
if [ "${NO_COLOR+x}" = x ]; then
  export FORCE_COLOR=0 DEBUG_COLORS=0
  unset NO_COLOR
fi

cd "$(dirname "$0")/.."
repo_root="$PWD"
# shellcheck source=gate-progress.sh
. "$repo_root/scripts/gate-progress.sh"
# shellcheck source=binary-lease.sh
. "$repo_root/scripts/binary-lease.sh"
# shellcheck source=e2e-daemon-check.sh
. "$repo_root/scripts/e2e-daemon-check.sh"
# shellcheck source=e2e-selection.sh
. "$repo_root/scripts/e2e-selection.sh"
# shellcheck source=e2e-provider-doubles.sh
. "$repo_root/scripts/e2e-provider-doubles.sh"
# shellcheck source=e2e-pool.sh
. "$repo_root/scripts/e2e-pool.sh"
# Cargo's own mutable artifact. Never invoked: `$story_bin`, assigned after
# the build below, is the leased hard link of it.
story_artifact="$repo_root/target/debug/story"
results_root="${STORYHOOK_E2E_RESULTS_DIR-$repo_root/e2e/test-results/current}"

# One artifact tree for this invocation. Each Playwright project gets its own
# output directory below, so a later project's startup no longer erases the
# screenshots, traces and error contexts from earlier failures while this
# script deliberately continues through the remaining matrix.
if [ "${STORYHOOK_E2E_RESULTS_DIR+x}" = x ]; then
  case "$results_root" in
    /*) ;;
    *) echo "run-e2e.sh: STORYHOOK_E2E_RESULTS_DIR must be absolute" >&2; exit 2 ;;
  esac
  # mkdir (without -p) refuses an existing path, including a symlink. The
  # caller owns its parent and no existing artifacts can be overwritten.
  mkdir "$results_root" || { echo "run-e2e.sh: results directory must be new: $results_root" >&2; exit 2; }
else
  rm -rf "$results_root"
  mkdir -p "$results_root"
fi
mkdir -p "$results_root/slice-reports" "$results_root/timings"
# History is advisory and shared across linked worktrees, like the Rust pool.
e2e_history="${STORYHOOK_E2E_DURATIONS:-$(git rev-parse --path-format=absolute --git-common-dir)/storyhook/e2e-durations.tsv}"
e2e_timing() {
  printf '%s\t%s\n' "$1" "$((SECONDS - $2))" >>"$results_root/timings/${slice:-plan}.tsv"
}

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

# --- How many slices run at once (SH-792). -------------------------------
#
# STORYHOOK_E2E_JOBS bounds the pool, and the selection is cut into
# E2E_SLICES_PER_JOB slices per job, so one knob sets both. The defaults are
# measured, not assumed (docs/spec/test-audit.md records the sweep): 8 of 10
# cores kept two free, where 12 bought 4% at twice the load flakes; two slices
# per job let a freed slot take the next slice, where one per job left the
# small projects holding whole slots.
E2E_DEFAULT_JOBS=8
E2E_SLICES_PER_JOB=2
# What a signalled slice's writers, and then its shells, each get to exit
# before the pool kills them: `cleanup` below stops a daemon and removes a
# seed in about a second.
E2E_STOP_GRACE_SECONDS=10
e2e_jobs="${STORYHOOK_E2E_JOBS:-$E2E_DEFAULT_JOBS}"
case "$e2e_jobs" in
  '' | *[!0-9]* | 0*)
    echo "run-e2e.sh: STORYHOOK_E2E_JOBS must be a positive integer, got '$e2e_jobs'" >&2
    exit 2
    ;;
esac

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

if [ ! -x "$story_artifact" ]; then
  echo "run-e2e.sh: $story_artifact not found after build" >&2
  exit 1
fi

# The lease (SH-635, see the header). Owned by THIS pid: the per-project
# subshells below stop their daemons before they exit, so by the time the
# outer trap removes the lease nothing runs from it. A run killed outright
# leaves its lease for the next sweeper -- this script's or the Rust suite's,
# which share the root -- to reclaim once this pid is gone.
story_bin="$(storyhook_lease_binary "$story_artifact")" || exit 1
story_lease_dir="$(dirname "$story_bin")"
trap 'rm -rf "$story_lease_dir"' EXIT
echo "run-e2e.sh: leased $story_artifact as $story_bin" >&2

if [ ! -d "$repo_root/e2e/node_modules" ] || ! (cd "$repo_root/e2e" && npx --no-install playwright --version >/dev/null 2>&1); then
  echo "run-e2e.sh: e2e/node_modules or the Playwright CLI is missing — run 'make e2e-install' first" >&2
  exit 1
fi

# --- Keep the display awake for the whole run (SH-628). -------------------
#
# WindowServer retains every IOSurface headless WebKit commits while all
# displays are asleep, and aborts the console session -- every agent session,
# every GUI app, this gate -- once the system-wide count reaches 65,535.
# Measured 2026-09-08/09, not assumed: 411 webkit tests with the display on
# peaked at 448 live surfaces; 103,753 surfaces created in thirteen dark
# minutes ended in the 01:31 crash. A display wake releases them, so the
# condition to avoid is simply "Playwright running while the display sleeps".
# `-u` turns the display on if it is already off, `-d` holds it on for the
# run, `-i` keeps the machine from idling under it. Chromium's headless shell
# never touches WindowServer, but wrapping every project keeps the rule one
# line rather than a per-engine table. Absent `caffeinate` (not macOS) the
# array is empty and expands to nothing.
keep_display_awake=()
if command -v caffeinate >/dev/null 2>&1; then
  keep_display_awake=(caffeinate -d -u -i)
fi

# --- One fully isolated run per slice. -----------------------------------
#
# run_one_project PROJECT SLICE TEST_LIST [PLAYWRIGHT_ARGS...]
#
# Everything from here down used to be this whole script's top level, run
# once. It is now a function run once PER SLICE (SH-792; once per project
# before that), each invocation in its own subshell so `trap cleanup EXIT`
# scopes to that subshell's process rather than the outer script's -- the
# same isolation a fresh `bash scripts/run-e2e.sh` process gave for free,
# reused rather than reinvented with a RETURN trap and manual bookkeeping.
# SLICE names everything this run writes that another slice could collide
# with: its output directory, its `selected/` count and its progress row.
# TEST_LIST is the slice's Playwright test-list, or `-` when the caller
# partitioned the selection itself.
run_one_project() {
(
  project="$1"
  slice="$2"
  test_list="$3"
  shift 3
  playwright_args=("$@")
  slice_args=()
  if [ "$test_list" != - ]; then
    slice_args=(--test-list="$test_list")
  fi

  slice_started=$SECONDS
  data_root="$(mktemp -d /private/tmp/story-e2e.XXXXXX)"
  isolated=0

  cleanup() {
    local status=$? cleanup_started=$SECONDS
    # A second signal here would abandon a half-stopped daemon and a
    # half-removed seed; the pool KILLs a cleanup that overruns its grace.
    trap '' TERM INT HUP
    # Whenever this run's isolated store is in effect, and only then: before
    # `storyhook_isolate` below, this environment still names the developer's
    # real store. Not only after the explicit `daemon start`, because every
    # `story` call in the seeding auto-starts this run's daemon well before
    # it. Stopping is synchronous, and a no-op when nothing runs, so the
    # removal below never races a daemon still writing its state home.
    if [ "$isolated" = "1" ]; then
      "$story_bin" daemon stop >/dev/null 2>&1 || true
    fi
    # The fake's placeholder pane process is told below to outlive this whole
    # run (FAKE_TMUX_PANE_LIFETIME), so the run is what ends it: the last
    # dispatch's placeholder has no later new-window to reap it, and a
    # `sleep` that outlives its state directory is an orphan of this script.
    local placeholder
    placeholder="$(cat "$data_root/faketmux/pane_pid" 2>/dev/null || printf '')"
    case "$placeholder" in
      '' | *[!0-9]*) : ;;
      *) kill -9 "$placeholder" >/dev/null 2>&1 || true ;;
    esac
    rm -rf "$data_root"
    e2e_timing cleanup "$cleanup_started"
    e2e_timing total "$slice_started"
    exit "$status"
  }
  trap cleanup EXIT

  echo "run-e2e.sh: === project=$project slice=$slice ===" >&2
  cd "$repo_root/e2e"

  # THE ISOLATION, in one shared place -- `scripts/test-env.sh`, whose own
  # header carries the parameters and the reason for each.
  #
  # It matters more here than anywhere else in this repository: this script
  # starts a lease of `target/debug/story` -- a real, NON-test binary -- so
  # `storyhook::env::is_test_build`'s refusal does not apply and this
  # environment is the ONLY thing standing between an e2e run and the
  # developer's actual store. There is no second guard behind it.
  #
  # `--home` is not passed: this leg also drives `npm`/playwright, whose
  # browser cache lives under the real $HOME.
  #
  # `$$` inside this subshell is the OUTER script's pid, which is the one that
  # should own these daemons -- a per-project subshell exits between projects
  # and its daemon is stopped by `cleanup` above, not by dying with it.
  # shellcheck source=test-env.sh
  . "$repo_root/scripts/test-env.sh"
  storyhook_isolate "$data_root"
  isolated=1

  # The leased binary, for the specs' own CLI calls (SH-635). A spec that ran
  # Cargo's artifact directly would, after a rebuild, itself be the client
  # that replaces this run's daemon -- `untrusted-origin-cookie.spec.ts`
  # restarts it on purpose, and would restart it from the wrong build.
  export DASHBOARD_STORY_BIN="$story_bin"
  # Notification and hook children also invoke the CLI. Their allowlisted
  # environment retains STORY_BIN and PATH, so both must name this same lease.
  # Otherwise an ambient installed CLI replaces the fixture daemon on first use.
  export STORY_BIN="$story_bin"
  export PATH="$story_lease_dir:$PATH"

  # This is not a store-isolation parameter, so test-env.sh deliberately does
  # not own it. It is still a browser-fixture input: an ambient proxy allowlist
  # makes TrustedHosts::behind_a_proxy() true and withdraws local_request
  # authority even from loopback, which makes handoff.spec.ts fail according to
  # the developer's shell rather than this harness (SH-321). Start every
  # project from the explicit no-proxy baseline; the one specialized project
  # that needs an allowlist will opt in below.
  unset STORYHOOK_WEB_TRUSTED_HOSTS
  UNTRUSTED_ORIGIN_HOST="storyhook.e2e.test"
  if [ "$project" = "untrusted-origin-chromium" ]; then
    export STORYHOOK_WEB_TRUSTED_HOSTS="$UNTRUSTED_ORIGIN_HOST"
  fi

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
  # CLEARED and rebuilt from an allowlist -- PATH, HOME and the XDG base
  # directories, TMPDIR, the locale/terminal names, and any
  # STORY_*/STORYHOOK_* name (`src/env/spawn_env.rs`, SH-193; TMUX and
  # TMUX_PANE are deliberately NOT on it) -- so a FAKE_TMUX_STATE exported
  # here has never reached one. XDG_STATE_HOME did not reach one either
  # until SH-633, which is how every dispatch below used to start a second
  # daemon for this run's store under the developer's REAL state home.
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
  # slices too, since each slice's subshell gets its own `data_root` and
  # therefore its own `FAKE_TMUX_STATE`: slices run at the same time (SH-792),
  # never in one state directory.
  export FAKE_TMUX_STATE="$data_root/faketmux"
  mkdir -p "$FAKE_TMUX_STATE"
  # The placeholder process the fake's `new-window` spawns to stand in for the
  # pane's occupant self-expires after FAKE_TMUX_PANE_LIFETIME seconds (30 by
  # default, a self-heal for shell tests that forget to kill it). Since
  # SH-626 the daemon's liveness probe actually reaches the fake, so that
  # expiry would read as a real dead window -- a lane alive longer than the
  # default is quarantined `window-gone` for no reason the test controls.
  # Derived from the longest browser leg this repository has measured, 6538s
  # under contention (docs/spec/test-tiers.md, SH-627), with a margin of
  # more than tenfold: one day. Nothing outlives the run regardless, because
  # `cleanup` above kills the recorded placeholder. Exported before the
  # snapshot below so the dispatch child's new-window sees it too.
  export FAKE_TMUX_PANE_LIFETIME=86400
  # A real provider always draws a composer, and dispatch types only into one
  # that it sees drawn and idle (SH-799). The fake's default `legacy` screen is
  # a footer with no composer row, so it would refuse every browser dispatch
  # as `composer-not-idle`; `marker` is its model of an idle Claude composer.
  # Exported before the snapshot below, like every knob.
  export FAKE_TMUX_CAPTURE=marker

  # The `claude`, `codex` and `tmux` executables the daemon resolves on PATH
  # during this run, generated by `scripts/e2e-provider-doubles.sh` (SH-584,
  # SH-616, and since SH-626 the tmux double bridges the FAKE_TMUX_* snapshot
  # below into the daemon's own allowlisted tmux calls -- the liveness probe
  # and kill-window never pass through the dispatch wrapper, and a tmux that
  # died on an unset FAKE_TMUX_IMPLEMENTATION read as "window gone" on every
  # steady pass). The snapshot directory is named here and filled below; the
  # double reads it when tmux runs, never now.
  provider_bin="$data_root/provider-bin"
  faketmux_env="$data_root/faketmux-env"
  export FAKE_TMUX_IMPLEMENTATION="$repo_root/plugins/story/tests/fakes/tmux"
  write_e2e_provider_doubles "$provider_bin" "$faketmux_env" "$FAKE_TMUX_IMPLEMENTATION" || exit 1
  export PATH="$provider_bin:$PATH"

  # Every FAKE_TMUX_* the harness has set, snapshotted one file per knob --
  # derived, so a knob added later crosses for free, and never interpolated
  # into the generated shell below, because a knob's value is data:
  # FAKE_TMUX_SESSIONS and FAKE_TMUX_TRANSCRIPT are deliberately multi-line,
  # and "remember to quote it correctly" is the discipline that fails on the
  # seventh knob. Anything exported AFTER this snapshot does not reach a
  # dispatch child -- `tests/store_isolation.rs` fails the build if a later
  # line tries.
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

# The production allowlist intentionally removes tmux selectors. Restore a
# run-owned caller here, where the fixture owns it, before resource discovery
# can fall back to the host default server (SH-888). Publish the fake endpoint
# for native inventory too; FAKE_TMUX_* alone cannot cross that boundary.
export TMUX="\$(cd "\$FAKE_TMUX_STATE" && pwd -P)/tmux.sock,0,0"
unset TMUX_PANE
tmux display-message -p '#{socket_path}' >/dev/null || exit 1

# Identify the helper verb without mistaking a separated --project value for
# it. This wrapper receives both --project <slug> dispatch and --project
# <slug> unclaim; the latter is stop-now's deliberate inverse while the
# dispatch helper may still be returning.
_helper_verb=""
_expect_project=false
for _arg in "\$@"; do
  if [ "\$_expect_project" = true ]; then
    _expect_project=false
    continue
  fi
  case "\$_arg" in
    --project) _expect_project=true ;;
    --project=*) ;;
    -*) ;;
    *) _helper_verb="\$_arg"; break ;;
  esac
done

# One dispatch writer per state directory, CHECKED rather than assumed. The
# daemon permits several dispatch children at once (MAX_RUNNING), and two in
# one directory is exactly SH-263: each one's new-window clears the other's
# launched flag and pane pid, and the readiness gate then refuses a pane that
# genuinely reads as holding a shell. Liveness is queried, never inferred from
# the file, so a child killed by the daemon's own process-group timeout leaves
# no lock behind. This process becomes story.sh via exec below, so its pid stays
# the right one to publish for exactly as long as the dispatch runs. An
# overlapping unclaim bypasses this guard because production stop-now
# deliberately supports a lane whose dispatch helper is still returning.
if [ "\$_helper_verb" = dispatch ]; then
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
fi

exec bash "$_real_dispatch_script" "\$@"
WRAPPER
  chmod 600 "$STORYHOOK_DISPATCH_SCRIPT"
  unset _real_dispatch_script _dispatch_protocol

  # --- Seed five projects: Alpha/Beta with a checkout (switching between
  # them is the whole point of project-selector.spec.ts and
  # filter-persistence.spec.ts), Gamma deliberately unattached so the
  # selector's read-only path -- SH-42's defect, see the commit that fixes it
  # -- has something to exercise, and Delta (SH-208) a fourth checked-out
  # project reserved for Dispatch Auto's own dispatch target, and Engine a
  # fifth checked-out project reserved for Full Auto's real lane. Delta and
  # Engine are separate because each real-dispatch spec owns the story it
  # consumes; coupling them would make either spec's success order-dependent.
  # Delta exists
  # because Alpha's exact two-story, four-empty-column shape is itself a
  # fixture other specs assert on byte-for-byte (filter-persistence.spec.ts's
  # `0 / 2`, column-visibility.spec.ts's `2 / 2` and its four-empty-columns
  # claim) -- a third Alpha story would silently break both. Seeded fresh
  # for THIS project's own daemon, never shared with another project's --
  # `dispatch.spec.ts` and `engine.spec.ts` claim these stories for real and a
  # second Playwright project reusing them would find them already claimed
  # (SH-335, SH-473).
  seed_dir="$data_root/seed"
  mkdir -p "$seed_dir/alpha" "$seed_dir/beta" "$seed_dir/delta" "$seed_dir/engine"

  seed_started=$SECONDS
  e2e_timing prepare "$slice_started"
  echo "run-e2e.sh: seeding projects…" >&2

  # A real git repo, not just a directory: neither project-selector.spec.ts
  # nor the pre-SH-50 suite needed one (a checkout is only a recorded path
  # until something reads it as a repository), but dispatch's worktree
  # creation does -- confirmed the hard way when AA-1's checkout wasn't one
  # and story.sh refused with exactly that message. No origin is configured;
  # story.sh's own base-resolution tolerates that (its `none` tier bases the
  # work on HEAD and says so — SH-691), so this is the minimum dispatch
  # actually needs.
  init_git_repo() {
    storyhook_fixture_git init -q -b main
    storyhook_fixture_git config user.email "e2e@storyhook.test"
    storyhook_fixture_git config user.name "storyhook e2e"
    echo "# $(basename "$PWD")" >README.md
    storyhook_fixture_git add README.md
    storyhook_fixture_git commit -q -m "init"
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
  (
    cd "$seed_dir/engine"
    init_git_repo
    # SH-706: destructive reset resolves protected branches from a real
    # origin. Keep that contract offline with a run-owned bare repository.
    storyhook_fixture_git clone -q --bare . "$seed_dir/engine-origin.git"
    storyhook_fixture_git remote add origin "$seed_dir/engine-origin.git"
    "$story_bin" project new --prefix EE --name "Engine Project" --no-agents-md >/dev/null
    # SH-473's one real Full Auto lane. A dedicated project prevents the
    # engine's claim/reset cycle from changing Alpha's exact board shape or
    # consuming Delta's ordinary Auto target.
    "$story_bin" new "Exercise Full Auto end to end" --json | jq -r '.story.story.id' >"$data_root/engine-story-id"
  )
  engine_story_id="$(cat "$data_root/engine-story-id")"

  e2e_timing seed "$seed_started"
  daemon_started=$SECONDS

  # --- Start the daemon and discover the port it actually bound. `daemon
  # start` blocks until the daemon reports ready (or times out), but its
  # listener accepting connections is a separate fact from its process
  # having started, so the readiness poll below is a belt-and-braces check,
  # not a formality.
  echo "run-e2e.sh: starting the daemon…" >&2
  start_output="$("$story_bin" daemon start 2>&1)"
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

  probe_url="http://127.0.0.1:$port"
  base_url="$probe_url"
  if [ "$project" = "untrusted-origin-chromium" ]; then
    base_url="http://$UNTRUSTED_ORIGIN_HOST:$port"
  fi
  deadline=$((SECONDS + 15))
  # `GET /` rather than `GET /api/repos` (SH-187: the latter now requires the
  # daemon's bearer token, which this probe has no reason to carry -- it is
  # asking "is the HTTP server up", not "is it authenticated").
  until curl -sf -o /dev/null "$probe_url/"; do
    if [ "$SECONDS" -ge "$deadline" ]; then
      echo "run-e2e.sh: $probe_url never answered GET / within 15s" >&2
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
  export DASHBOARD_ENGINE_STORY_ID="$engine_story_id"

  # --- Run the suite, this project only.
  #
  # Both checks below fail loudly rather than skipping: a browser suite that
  # quietly no-ops and exits 0 reads as "the dashboard was verified" when
  # nothing ran, which is the silent-fallback failure shape CLAUDE.md
  # forbids.
  export DASHBOARD_URL="$base_url"

  e2e_timing daemon "$daemon_started"
  listing_started=$SECONDS
  # Planner-owned slices reuse the already validated selection. The real run's
  # reporter checks discovery against it, so no second Node listing is needed.
  # A caller partition retains Playwright's per-project shard semantics.
  if [ "$test_list" != - ]; then
    expected_manifest="$test_list.tsv"
    known_total="$(awk -F '\t' '{ sum += $3 } END { print sum + 0 }' "$expected_manifest")" || exit 1
  else
    list_status=0
    list_output="$(e2e_list_selection "$data_root/playwright-list.stderr" \
      npx playwright test --project="$project" "${slice_args[@]+"${slice_args[@]}"}" "${playwright_args[@]+"${playwright_args[@]}"}")" || list_status=$?
    case "$list_status" in
      0) ;;
      "$E2E_SELECTION_EMPTY")
        echo "run-e2e.sh: slice=$slice selects no tests under this filter — skipping" >&2
        gate_progress_emit_item "release gate/e2e/$slice" skipped
        exit 0
        ;;
      *)
        echo "run-e2e.sh: slice=$slice could not be listed (exit $list_status) — refusing, not skipping (SH-625)" >&2
        gate_progress_emit_item "release gate/e2e/$slice" failed
        exit "$list_status"
        ;;
    esac
    known_total="$(e2e_selection_total "$list_output")" || exit 1
    expected_manifest="$results_root/slice-reports/$slice.expected.tsv"
    printf '%s\n' "$list_output" | e2e_pool_file_counts >"$expected_manifest" || exit 1
  fi
  e2e_timing listing "$listing_started"
  export E2E_SLICE_EXPECTED="$expected_manifest"
  export E2E_SLICE_REPORT="$results_root/slice-reports/$slice.json"
  export STORYHOOK_GATE_PROGRESS_PATH="release gate/e2e/$slice"
  gate_progress_emit_item "$STORYHOOK_GATE_PROGRESS_PATH" running "total=$known_total"
  # This slice is about to run $known_total tests: say so where the outer
  # script can add it up (SH-625, SH-792). Written BEFORE the real run, so a red project
  # still counts as having run -- the verdict below carries its own failure.
  mkdir -p "$results_root/selected"
  printf '%s\n' "$known_total" >"$results_root/selected/$slice"
  e2e_start=$(date +%s)
  # Exact filenames exclude the separately stubbed context-menu specs.
  real_dispatch_selected="$(e2e_selection_real_dispatch <"$expected_manifest")" || exit 1

  # `|| status=$?` rather than `if ! npx ...; then status=$?`: under `!`,
  # bash inverts the command's exit status, so `$?` inside that then-branch
  # is the *inverted* value -- always 0 -- and `exit "$status"` would always
  # exit 0 regardless of whether Playwright passed (SH-224).
  # SH-627: the launch probe (`e2e/launch-probe.ts`, the config's
  # `globalSetup`) launches THIS project's engine once before any worker
  # starts, and reads the project's name from here rather than re-parsing
  # Playwright's own argv. Exported after `--list` above on purpose: listing
  # runs no global setup, so a filter that selects nothing is answered without
  # a browser launch. A dead browser then costs one launch timeout and a
  # refusal that names the machine, not one launch timeout per test.
  export E2E_PROJECT="$project"
  if [ "$isolate_files" = 1 ]; then
    mkdir -p "$results_root/reports" "$results_root/executed"
    export E2E_ISOLATION_REPORT="$results_root/reports/$slice.json"
  fi
  status=0
  "${keep_display_awake[@]+"${keep_display_awake[@]}"}" npx playwright test --project="$project" --output="$results_root/$slice" "${slice_args[@]+"${slice_args[@]}"}" "${playwright_args[@]+"${playwright_args[@]}"}" || status=$?
  e2e_elapsed=$(( $(date +%s) - e2e_start ))
  if [ "$isolate_files" = 1 ]; then
    printf '%s\n' "$status" >"$results_root/executed/$slice"
  fi
  printf 'playwright\t%s\n' "$e2e_elapsed" >>"$results_root/timings/$slice.tsv"
  # Playwright can swallow reporter exceptions or the caller can override its
  # reporter list. Neither is allowed to manufacture successful coverage.
  if [ "$status" = 0 ]; then
    python3 "$repo_root/scripts/e2e-durations.py" validate "$E2E_SLICE_REPORT" "$expected_manifest" || status=1
  fi

  # --- Was the run's binary rebuilt under it? Informational either way
  # (SH-635): the lease is exactly what makes this harmless, and saying so
  # turns "the artifact changed mid-run" from a silent fact into a printed
  # one, so a reader triaging a red run can rule it out by name.
  if ! [ "$story_bin" -ef "$story_artifact" ]; then
    echo "run-e2e.sh: note: $story_artifact was rebuilt during this run; the lease" >&2
    echo "  $story_bin kept every process on the build the run started with (SH-635)." >&2
  fi

  # --- Is the daemon that answered this run the one this script started?
  # Checked on a green verdict too: a suite that passed against a daemon this
  # harness did not configure is not a verdict (SH-226, SH-306). Pid is
  # deliberately not part of the question -- untrusted-origin-cookie.spec.ts
  # restarts the daemon on purpose, keeping its port -- so the check is port,
  # executable inode and liveness, which is exactly the shape of the SH-635
  # incident: a replaced daemon on a fresh `--port 0` binding.
  if ! storyhook_daemon_is_still_ours "$portfile" "$port" "$story_bin"; then
    echo "run-e2e.sh: slice=$slice: the daemon this run started is gone or replaced" >&2
    echo "  (see above). Read every failure after that point as ONE dead daemon," >&2
    echo "  not as that many tree failures -- the SH-627 shape (SH-635)." >&2
    if [ "$status" -eq 0 ]; then
      status=1
    fi
  fi

  gate_progress_emit_item "$STORYHOOK_GATE_PROGRESS_PATH" \
    "$([ "$status" = 0 ] && echo passed || echo failed)" "seconds=$e2e_elapsed"
  if [ "$status" -ne 0 ]; then
    echo "run-e2e.sh: slice=$slice failed. If the error above is about a missing browser executable, run 'make e2e-install' and retry." >&2
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
  if [ "$real_dispatch_selected" -gt 0 ] && [ ! -s "$FAKE_TMUX_STATE/new_window_args.log" ]; then
    echo "run-e2e.sh: slice=$slice selected a real-dispatch spec, but no dispatch reached this run's fake tmux state" >&2
    echo "  directory ($FAKE_TMUX_STATE/new_window_args.log is missing or empty)." >&2
    echo "  Either the spec didn't actually dispatch, or the dispatch children used a" >&2
    echo "  different fake-tmux state than the one this script configured (SH-263)." >&2
    exit 1
  fi
)
}

# --- Decide: which projects, then which slices (SH-792). ------------------
#
# `--project=NAME` narrows the run to that project (the `make e2e
# ARGS=--project=webkit` triage path), and that project is still sliced. A bare
# `--project NAME` (space-separated) is not supported by this wrapper --
# Playwright itself accepts both, but every caller in this repo uses the `=`
# form. A caller that partitions the selection itself -- `--shard` or
# `--test-list`, in either spelling -- gets one slice per project, so its
# partition is never cut a second time.
if [ -n "$explicit_project" ]; then
  projects_to_run=("$explicit_project")
else
  projects_to_run=("${ALL_PROJECTS[@]}")
fi
project_flags=()
for project in "${projects_to_run[@]}"; do
  project_flags+=(--project="$project")
done

# --- The plan: which slices this run is (SH-792). -------------------------
slice_names=()
slice_projects=()
slice_lists=()
slice_counts=()
plan_total=""

# A caller's own partition (`--shard`, `--test-list`) is applied exactly as
# this runner always applied it: per project, one Playwright invocation each.
# Playwright shards a multi-project listing across the projects instead, so no
# plan listing can describe that partition; each slice lists itself.
partition_by_project() {
  local project
  for project in "${projects_to_run[@]}"; do
    slice_names+=("$project")
    slice_projects+=("$project")
    slice_lists+=(-)
    slice_counts+=("?")
  done
}

# The plan listing: one listing of every selected project under the caller's
# filters, before any fixture exists, through the same library as each
# slice's own listing -- so a spec that cannot load is refused by name here
# too and never read as an empty selection (SH-625). No daemon or seed exists
# yet, so it runs in `e2e/plan-listing.ts`'s mode: a dashboard URL that is
# never contacted, and fixture values `requiredEnv` hands out only to a
# listing. It also compiles every selected spec into Playwright's shared
# transform cache before the slices start at once.
#
# A failure here refuses the whole leg before any project runs; one project's
# listing failing used to refuse only that project. A spec that will not load
# under the plan would not load in any slice either.
plan_slices() {
  local plan_status=0 plan_output file_counts counted_total plan_rows plan_started=$SECONDS
  local slice project list count has_slice
  plan_output="$(cd "$repo_root/e2e" && e2e_list_selection "$results_root/plan-listing.stderr" \
    env E2E_PLAN_LISTING=1 DASHBOARD_URL=http://plan-listing.invalid npx playwright test "${project_flags[@]}" "${extra_args[@]+"${extra_args[@]}"}")" || plan_status=$?
  case "$plan_status" in
    0) ;;
    "$E2E_SELECTION_EMPTY")
      for project in "${projects_to_run[@]}"; do
        gate_progress_emit_item "release gate/e2e/$project" skipped
      done
      echo "run-e2e.sh: no project selected a test under this filter — nothing ran, refusing to report green (SH-625)" >&2
      exit 1
      ;;
    *)
      echo "run-e2e.sh: the plan listing could not be read (exit $plan_status) — refusing, not skipping (SH-625)" >&2
      exit "$plan_status"
      ;;
  esac

  # Every listed test must be counted under some file, or a drift in
  # Playwright's line shape would drop tests from every slice without a word.
  plan_total="$(e2e_selection_total "$plan_output")"
  file_counts="$(printf '%s\n' "$plan_output" | e2e_pool_file_counts)"
  counted_total="$(printf '%s\n' "$file_counts" | awk -F '\t' '{ sum += $3 } END { print sum + 0 }')"
  if [ "$counted_total" != "$plan_total" ]; then
    echo "run-e2e.sh: the plan listing's Total: line says $plan_total tests, but its test lines" >&2
    echo "  name $counted_total. The shape of Playwright's listing lines has drifted from what" >&2
    echo "  scripts/e2e-pool.sh reads; refusing rather than running part of the selection." >&2
    exit 1
  fi

  # Kept with the run's artifacts: which files each slice ran is the first
  # question a red slice raises.
  mkdir -p "$results_root/slices"
  if [ "$isolate_files" = 1 ]; then
    plan_rows="$(printf '%s\n' "$file_counts" | python3 -B "$repo_root/scripts/e2e-isolation.py" plan "$results_root/slices" "${projects_to_run[@]}")" || return $?
  else
    plan_rows="$(printf '%s\n' "$file_counts" | python3 "$repo_root/scripts/e2e-durations.py" weights "$e2e_history" | e2e_pool_plan "$((e2e_jobs * E2E_SLICES_PER_JOB))" "$results_root/slices" "${projects_to_run[@]}")"
  fi
  e2e_timing planning "$plan_started"
  while IFS=$'\t' read -r slice project list count; do
    [ -n "$slice" ] || continue
    slice_names+=("$slice")
    slice_projects+=("$project")
    slice_lists+=("$list")
    slice_counts+=("$count")
  done < <(printf '%s\n' "$plan_rows")

  for project in "${projects_to_run[@]}"; do
    has_slice=0
    for slice in ${slice_projects[@]+"${slice_projects[@]}"}; do
      if [ "$slice" = "$project" ]; then
        has_slice=1
      fi
    done
    if [ "$has_slice" = 0 ]; then
      echo "run-e2e.sh: project=$project selects no tests under this filter — skipping" >&2
      gate_progress_emit_item "release gate/e2e/$project" skipped
    fi
  done
}

if [ "$caller_partition" = 1 ]; then
  partition_by_project
  echo "run-e2e.sh: ${#slice_names[@]} slice(s), one per project under the caller's own partition, up to $e2e_jobs at once:" >&2
else
  plan_slices
  echo "run-e2e.sh: $plan_total tests in ${#slice_names[@]} slice(s), up to $e2e_jobs at once (STORYHOOK_E2E_JOBS):" >&2
fi
_i=0
while [ "$_i" -lt "${#slice_names[@]}" ]; do
  printf '  %-36s %5s tests\n' "${slice_names[$_i]}" "${slice_counts[$_i]}" >&2
  # SH-524: every slice this run will attempt shows in the checklist before
  # any starts, pending rather than absent until the pool reaches it.
  gate_progress_emit_item "release gate/e2e/${slice_names[$_i]}" pending
  _i=$((_i + 1))
done
unset _i

# The pool's runner: one slice, by name, through its own fixture.
run_slice() {
  local i=0 status=0
  while [ "$i" -lt "${#slice_names[@]}" ]; do
    if [ "${slice_names[$i]}" = "$1" ]; then
      # Serial isolation reruns own a different artifact root.
      mkdir -p "$results_root/slice-reports" "$results_root/timings"
      if [ "$isolate_files" = 1 ]; then
        mkdir -p "$results_root/logs" "$results_root/verdicts"
        run_one_project "${slice_projects[$i]}" "$1" "${slice_lists[$i]}" "${extra_args[@]+"${extra_args[@]}"}" >"$results_root/logs/$1.log" 2>&1 || status=$?
        printf '%s\n' "$status" >"$results_root/verdicts/$1"
        cat "$results_root/logs/$1.log"
        return "$status"
      else
        run_one_project "${slice_projects[$i]}" "$1" "${slice_lists[$i]}" "${extra_args[@]+"${extra_args[@]}"}"
        return
      fi
    fi
    i=$((i + 1))
  done
  echo "run-e2e.sh: the plan has no slice named $1" >&2
  return 1
}

# Every slice runs even after one fails, and each keeps its own artifacts
# under $results_root/<slice>. The pool returns 0 or 1: a slice's own status
# of 125 or more would read to scripts/gate-legs.sh as a cancelled gate.
overall_status=0
e2e_pool_run "$e2e_jobs" "$E2E_STOP_GRACE_SECONDS" run_slice ${slice_names[@]+"${slice_names[@]}"} || overall_status=$?

# A run in which no slice selected a test executed nothing, and nothing
# executed is not a pass (SH-625) -- whatever each slice's own verdict was.
# The plan listing already refuses an empty selection; this is the same
# question asked of what the slices actually ran.
tests_run="$(e2e_selection_tests_run "$results_root/selected")"
if [ "$overall_status" = 0 ] && [ "$tests_run" = 0 ]; then
  echo "run-e2e.sh: no project selected a test under this filter — nothing ran, refusing to report green (SH-625)" >&2
  exit 1
fi
# Every test the plan listed ran in exactly one slice (SH-792). A caller's
# own partition has no plan total to hold the slices to.
if [ "$overall_status" = 0 ] && [ -n "$plan_total" ] && [ "$tests_run" != "$plan_total" ]; then
  echo "run-e2e.sh: the slices ran $tests_run tests, but the plan listed $plan_total. A slice's" >&2
  echo "  test-list lost or gained a file; refusing rather than reporting part of the" >&2
  echo "  selection green. Each slice's list is in $results_root/slices." >&2
  exit 1
fi

if [ "$isolate_files" = 1 ]; then
  # The outer pool has finished. Each rerun starts a new production fixture;
  # its counts and artifacts must not overwrite the first attempt's evidence.
  failed_slices="$(python3 -B "$repo_root/scripts/e2e-isolation.py" failed "$results_root")" || exit 1
  initial_results_root="$results_root"
  rerun_slices=()
  while IFS= read -r failed_slice; do
    [ -n "$failed_slice" ] || continue
    rerun_slices+=("$failed_slice")
  done < <(printf '%s\n' "$failed_slices")
  if [ "${#rerun_slices[@]}" -gt 0 ]; then
    (results_root="$initial_results_root/reruns";
      e2e_pool_run 1 "$E2E_STOP_GRACE_SECONDS" run_slice "${rerun_slices[@]}") || overall_status=1
  fi
  python3 -B "$repo_root/scripts/e2e-isolation.py" report "$results_root" || overall_status=1
fi

# Only ordinary unfiltered, planner-owned selections train history. Local
# reports remain available for every run, including failures and triage filters.
if [ "$overall_status" = 0 ] && [ "$caller_partition" = 0 ] && [ "${#extra_args[@]}" = 0 ] && [ "$isolate_files" = 0 ]; then
  python3 "$repo_root/scripts/e2e-durations.py" merge "$e2e_history" "$results_root/slice-reports/"*.json
fi

exit "$overall_status"
