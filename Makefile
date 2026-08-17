# Storyhook developer tasks.
#
# Two gate tiers (SH-394), because the browser suite dwarfs everything else in
# this file. `docs/rearch/baseline/timings.md` puts the whole Rust suite at a
# 36s median; `e2e/playwright.config.ts`'s own SH-222 measurement puts the
# Playwright leg at 2.9-6.4 MINUTES per desktop project, and the 52 desktop
# specs run twice (chromium and webkit). On a machine that routinely runs
# three-to-four concurrent worktree suites, that gap is the whole reason the
# gate was ever "nine minutes nominal, routinely longer" — the CLI's own tests
# were never the slow part.
#
#   make test        fmt, clippy, the Rust suite, a release build, the plugin
#                     bash harness. THE MERGE GATE — this is what
#                     `.githooks/pre-push` requires a receipt for.
#   make test-full    `make test`, plus `scripts/run-e2e.sh` (the dashboard's
#                     browser suite). THE RELEASE GATE — `scripts/release.sh`
#                     will not cut a public release without it, and
#                     `--skip-gate` stays refused there. See
#                     `docs/spec/test-tiers.md` for the design of record.
#
# The dashboard is a feature; the CLI is the tool storyhook actually is. A
# push that changed only Rust code was never made safer by a browser suite
# that exercises none of it, and a push that changed `src/web_dashboard.html`
# is still caught — one release away, not never. There is no push/PR CI (see
# .github/workflows/release.yml, which triggers on version tags only), so
# `.githooks/pre-push` plus this file is the entire gate. Skipping `make test`
# is how a non-compiling commit reaches `main` undetected (see #23).
#
# `make test` used to run the browser suite too, and before that it was one of
# two Rust legs: `make test-daemon` ran the identical suite a second time with
# every CLI command going over `/api/v1/invoke`, and `make gate` ran both.
# That leg existed because there were two transports, and it is gone with the
# second one (SH-114). What it caught — a fixture that is only correct when
# nothing else holds the store — is now caught by the only run there is,
# because every command in it goes through a daemon.
#
# `scripts/run-e2e.sh` is the one leg `cargo test` cannot cover: the
# dashboard's JavaScript has no Rust to exercise it, so it is driven in a real
# browser instead (see `e2e/`). Within `make test-full` it fails the whole gate
# loudly rather than skipping if its Node toolchain was never installed (`make
# e2e-install`) — a green run that quietly skipped the browser suite would say
# "the dashboard was verified" when nothing ran, which is worse than a red
# one. `scripts/leg.sh` is what makes the OTHER direction loud too: when
# `make test` runs without it, the deferral is printed and named, never
# silent.

.PHONY: test test-full build fmt lint clippy check release-build install check-no-orphan-servers e2e-install e2e merge-watch

# Where `make install` puts the binary. Mirrors install.sh's default and its
# STORYHOOK_INSTALL_DIR override so both entry points agree; a one-off can
# still say `make install INSTALL_DIR=/somewhere/else`.
STORYHOOK_INSTALL_DIR ?= $(HOME)/.local/bin
INSTALL_DIR ?= $(STORYHOOK_INSTALL_DIR)

# Full local gate: formatting, clippy with warnings-as-errors, full test
# suite, plus the Claude Code plugin's own bash harness (bin/story.sh's
# ready-gate/CAS-claim/dispatch behavior — issue #40). The plugin suite
# exercises the REAL `story` binary this build just produced (never a
# possibly-stale globally-installed one, and never a fake -- a fake can't
# catch a genuine CAS race or a real is_ready() interaction), so `cargo
# build` runs first and target/debug is prepended to PATH for that one step.
#
# The orphan check brackets the run: before, because a survivor of an earlier
# run makes this one lie (SH-51), and after, because a run that leaks one has
# just armed the same trap for the next person.
#
# INSTA_UPDATE=no makes the golden CLI corpus (tests/golden_cli.rs) a real gate.
# insta's default is to WRITE a .snap.new beside any snapshot that no longer
# matches; a developer who then runs `cargo insta accept` has silently rewritten
# the byte-compatibility contract the whole rearchitecture is measured against.
# Under `no`, a mismatch fails the run and writes nothing -- updating a snapshot
# becomes a deliberate `INSTA_UPDATE=always cargo test` plus a reviewed diff.
#
# The isolated data directory is NOT optional, and it is the single most
# dangerous line in this file to delete. Story data lives in one global store
# now, and ~45 test files still build their fixtures with `tempfile::tempdir()`
# and run `story` with this process's environment. Without the override, every
# one of them writes into the developer's real
# ~/.local/share/storyhook/store.db. `storyhook_test_support::TestEnv` isolates
# the tests that use it and overrides this again with its own directory; this
# covers the ones that do not. /private/tmp rather than $TMPDIR because the
# latter is Spotlight-indexed (SH-53).
#
# Since W8 the binary refuses to run at all if a test build resolves no
# `STORYHOOK_DATA_DIR` (`storyhook::env::is_test_build`), so deleting the
# wrapper's export now fails the suite loudly instead of quietly eating real
# data. Belt and braces on purpose: the wrapper is what makes the run *correct*,
# the guard is what makes a run without it *impossible*.
#
# `--test-threads=4` is a bound on how many daemons exist at once, not tuning.
# Each test binary's shared environment gets a daemon and each isolated one gets
# another, and since SH-114 every `story` command starts one — so the quantity
# this bounds strictly grew when the second transport went.
#
# W5 measured the daemon leg stalling wide open:
# `move_if_state_under_real_concurrency_yields_exactly_one_winner` passed in 1.2s
# alone and had not finished after 60s unbounded. W8 (2026-07-29) re-measured and
# found it passing at every parallelism tried — three green runs unbounded at
# 51s, 52s, 51s; `8` at 53s; `4` at 55s; `2` at 67s — and kept the bound anyway,
# deliberately: it cost four seconds on a 51-second leg, what it retires is a
# *stall* (no signal, no failing test name, a wall-clock timeout several layers
# up long after the evidence is gone), and the quantity it bounds keeps growing.
#
# Raise it if the wall clock ever becomes the problem, but re-measure first: the
# symptom of overshooting is a stall, not a failure. The lever is threads, never
# scope.
#
# `gate-receipt.sh` brackets the run the way check-no-orphan-servers does, and
# the two phases are not interchangeable (SH-306). The preflight ENROLS this
# clone -- it sets core.hooksPath so git's own pre-push hook enforces the gate,
# which is what makes running this target the thing that installs the gate
# rather than a ritual someone has to remember. The postlude WRITES THE RECEIPT
# naming the tree that just went green, and it is the LAST recipe line on
# purpose: make aborts the recipe at the first failing line, so "no receipt
# unless every leg passed" is true by construction rather than by exit-code
# plumbing. Anything appended after it starts certifying failed runs.
#
# The gate it feeds replaced a Claude Code PreToolUse hook that was SIGTERMed at
# its own 900-second ceiling, after which the push proceeded ungated and
# unannounced -- six times in three days. A pre-push hook that re-ran this
# target instead would have inherited the same deadline pressure and started a
# second nine-minute suite while the first was still running, which this project
# has already recorded as the cause of a false red.
#
# `test-full: E2E=1` is a target-specific variable (not recursive make): it is
# visible to every prerequisite `test` reaches, including the `$(if $(E2E),…)`
# conditional below, without a second process and without re-parsing this
# file. `test-full` depends on `test` rather than duplicating its recipe, so
# the two tiers can never drift apart on everything but the browser leg.
#
# The receipt's tier argument mirrors E2E exactly: `full` only when the
# browser suite actually ran, `gate` otherwise. `gate-receipt.sh` refuses to
# let a `gate` postlude downgrade an existing `full` receipt for the same
# tree, so re-running the cheap tier after the expensive one never loses the
# stronger claim.
#
# `$(if …)` treats any NON-EMPTY expansion as true -- E2E=0 would still be
# "on", the same footgun as a shell `[ -n "$VAR" ]` check. There is exactly
# one place this variable is ever set, immediately below, and it is set or
# absent, never to a string meaning false.
test-full: E2E=1
test-full: test

test: check-no-orphan-servers
	@bash scripts/gate-receipt.sh preflight
	bash scripts/leg.sh fmt -- cargo fmt --all -- --check
	bash scripts/leg.sh clippy -- cargo clippy --workspace --all-targets -- -D warnings
	@bash scripts/leg.sh rust-suite -- bash scripts/run-tests.sh -- --test-threads=4
	bash scripts/leg.sh build -- cargo build
	PATH="$(CURDIR)/target/debug:$$PATH" bash scripts/leg.sh plugin -- bash plugin/claude-code/tests/run-tests.sh
	$(if $(E2E),bash scripts/leg.sh e2e -- bash scripts/run-e2e.sh,@bash scripts/leg.sh --skipped e2e)
	@bash scripts/check-no-orphan-servers.sh postlude
	@bash scripts/gate-receipt.sh postlude $(if $(E2E),full,gate)

# Installs the e2e/ Node toolchain and the browsers e2e/playwright.config.ts
# names (chromium, webkit -- SH-335). Not part of either gate target itself --
# it is a one-time (per-machine, per-Playwright-version) bootstrap step, not
# something every run should repeat -- but `test-full`'s e2e leg fails loudly,
# naming this target, if it was never run. `make test` never reaches it.
#
# One further, OPTIONAL per-machine step this target documents rather than
# performs: WebKit's Tab order skips buttons and links unless macOS's Full
# Keyboard Access is on -- real Safari's own out-of-box default, not a bug
# `scripts/run-e2e.sh` can fix for you. It measures the setting and gates the
# handful of assertions that need it rather than silently mutating your
# System Settings (SH-335 -- story show SH-335 carries the verdict)
# -- for those to run under `webkit` instead of reporting skipped, once:
#   defaults write -g AppleKeyboardUIMode -int 2
e2e-install:
	cd e2e && npm ci
	cd e2e && npx playwright install --with-deps chromium webkit

# Runs just the dashboard's browser suite. Bare, this loops once per project
# `e2e/playwright.config.ts` names (chromium, webkit, mobile-chromium,
# mobile-webkit), each against its own isolated daemon and seed (SH-335,
# SH-348). Pass Playwright CLI
# flags through, e.g. `make e2e ARGS=--headed` (applies to every project in
# the loop) or `make e2e ARGS=--project=webkit` (runs that one project only).
e2e:
	bash scripts/run-e2e.sh $(ARGS)

# One reconcile pass over every open PR (SH-396): computes each PR's merge
# tree against origin/main, and for any tree with no `make test` receipt yet,
# tests it in a persistent worktree and certifies it for real on green --
# so a PR that has gone green here needs no further work at merge time, and
# `.githooks/pre-push` already recognizes the resulting tree once it lands.
# Reports on the PR itself (an upserted comment, never a fresh one per pass --
# see scripts/merge-watch.sh's own header for why). This target runs ONE
# pass and exits; it installs no timer of its own. Meant to be re-run every
# 1-3 minutes by something that already exists on this machine (`/loop`, a
# launchd job) -- bootstrapping that recurrence is a per-machine choice, not
# something this target does for you.
merge-watch:
	bash scripts/merge-watch.sh

# Fails if a test-spawned server from this worktree is still running. Never
# looks at the installed dashboard daemon on :3456 — that one is production.
check-no-orphan-servers:
	@bash scripts/check-no-orphan-servers.sh preflight

# Debug build of the `story` binary.
build:
	cargo build

# Apply formatting in place.
fmt:
	cargo fmt

# Lint only (warnings treated as errors).
lint clippy:
	cargo clippy --workspace --all-targets -- -D warnings

# Fast type-check without producing a binary.
check:
	cargo check --workspace --all-targets

# Optimized release build.
release-build:
	cargo build --release

# Build release and install it to INSTALL_DIR (see SH-55).
#
# Uses install(1) and never `cp`. macOS invalidates a Mach-O's cached
# code-signing state when its contents are rewritten *in place*; `cp` keeps the
# destination inode, so copying over a path that a live process still has
# mapped (a running `story web --serve`) leaves every later exec of that inode
# SIGKILLed -- exit 137, despite correct bytes and a passing `codesign -v`.
# install(1) replaces the file with a fresh inode: new invocations get a
# cleanly-signed binary and the running process keeps its old mapping.
#
# Note this does NOT restart a running dashboard daemon; it keeps serving the
# old code until restarted (see SH-54).
install: release-build
	@mkdir -p "$(INSTALL_DIR)"
	install -m 755 target/release/story "$(INSTALL_DIR)/story"
	@echo "Installed story $$("$(INSTALL_DIR)/story" --version | awk '{print $$2}') to $(INSTALL_DIR)/story"
