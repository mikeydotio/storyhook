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
#   make test         fmt, clippy, disjoint core/checkout-contract Rust
#                      batteries, a release build, and the plugin bash
#                      harness. THE MERGE GATE — this is what
#                      `.githooks/pre-push` requires a receipt for.
#   make test-full     `make test`, plus `scripts/run-e2e.sh` (the dashboard's
#                      browser suite). THE RELEASE GATE — `scripts/release.sh`
#                      will not cut a public release without it, and
#                      `--skip-gate` stays refused there. See
#                      `docs/spec/test-tiers.md` for the design of record.
#   make test-changed  `make test` with the Rust-suite leg narrowed to
#                      whatever `scripts/select-tests.sh` decides is affected
#                      since the nearest fully-certified ancestor (SH-429) —
#                      fmt, clippy, the build and the plugin harness stay
#                      unconditional; only test EXECUTION is selective.
#                      Writes a `changed`-tier receipt, which `.githooks/pre-
#                      push` accepts for a push but `scripts/merge-preflight.
#                      sh` never does for a merge (a council verdict — story
#                      SH-429, and `docs/spec/selective-testing.md`). A
#                      developer-loop accelerant, never a weaker merge gate.
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
#
# That deferral says the browser suite was skipped; until SH-418 it said
# nothing about whether it had EVER run, which is precisely the silence it
# exists to prevent — one tier up. It is now followed by
# `scripts/browser-status.sh`, which names how far `main` is from the last
# tree the browser suite certified. This is the one place that fact is
# collected with NO bootstrap at all: `make browser-watch` needs a per-machine
# timer nobody has installed yet, whereas the daemon-owned verifying queue
# runs the merge gate. It never gates —
# `|| true`, because a merge gate that failed on the release tier's staleness
# would undo the split SH-394 measured — and it is deliberately absent from
# the `test-full` branch, where the suite is about to actually run.

.PHONY: test test-full test-changed _test-body _test-full-body _test-changed-body build fmt lint clippy check release-build install check-no-orphan-servers e2e-install e2e merge-watch browser-watch browser-status coverage-map coverage-watch coverage-status scratch scratch-clean

# Where `make install` puts the binary. Mirrors install.sh's default and its
# STORYHOOK_INSTALL_DIR override so both entry points agree; a one-off can
# still say `make install INSTALL_DIR=/somewhere/else`.
STORYHOOK_INSTALL_DIR ?= $(HOME)/.local/bin
INSTALL_DIR ?= $(STORYHOOK_INSTALL_DIR)

# A recipe line containing `$(MAKE)` still runs under GNU Make's -n, -t and
# -q modes so its recursive make can inherit the operation. That exception
# must not make our surrounding wrapper run a REAL orphan postlude. GNU Make
# documents the no-argument flag group and recommends `findstring` for testing
# it. GNU Make 3.81 (the version Xcode supplies here) may put a long option
# BEFORE that group, unlike current Make's first-word contract, so inspect
# every word after excluding long options and variable assignments. That
# accepts both 3.81's `-pn` and current Make's `pn` without mistaking
# `--no-print-directory` for an `n` flag.
STORYHOOK_MAKE_SHORT_FLAGS := $(strip $(foreach makeflag,$(MAKEFLAGS), \
	$(if $(filter --%,$(makeflag)),, \
		$(if $(findstring =,$(makeflag)),,$(makeflag)))))
STORYHOOK_MAKE_NO_EXEC := $(strip \
	$(findstring n,$(STORYHOOK_MAKE_SHORT_FLAGS)) \
	$(findstring t,$(STORYHOOK_MAKE_SHORT_FLAGS)) \
	$(findstring q,$(STORYHOOK_MAKE_SHORT_FLAGS)))

# Full local gate: formatting, clippy with warnings-as-errors, full test
# suite, plus the shared agent plugin's own bash harness (bin/story.sh's
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
#
# The private body targets exist for one reason: `with-orphan-postlude.sh`
# must own the whole fallible body as one command so it can reach the orphan
# postlude after either outcome. They are not alternate gates -- called by
# hand they neither run the preflight nor write a receipt. The public targets
# keep those two trust-boundary steps outside the wrapper, with the receipt
# still last and therefore reachable only when both body and cleanup passed.
test-full: E2E=1
test-full: test

test: check-no-orphan-servers
	@bash scripts/gate-receipt.sh preflight
	@bash scripts/with-orphan-postlude.sh $(if $(STORYHOOK_MAKE_NO_EXEC),--make-no-exec) -- $(MAKE) --no-print-directory $(if $(E2E),_test-full-body,_test-body)
	@bash scripts/gate-receipt.sh postlude $(if $(E2E),full,gate)

_test-full-body: E2E=1
_test-full-body: _test-body

_test-body:
	bash scripts/leg.sh --reuse fmt -- cargo fmt --all -- --check
	bash scripts/leg.sh --reuse clippy -- cargo clippy --workspace --all-targets -- -D warnings
	@bash scripts/leg.sh --reuse rust-suite -- bash scripts/run-rust-battery.sh core
	@bash scripts/leg.sh --reuse rust-contracts -- bash scripts/run-rust-battery.sh contracts
	bash scripts/leg.sh --reuse build -- cargo build
	PATH="$(CURDIR)/target/debug:$$PATH" bash scripts/leg.sh --reuse plugin -- bash plugins/story/tests/run-tests.sh
	$(if $(E2E),bash scripts/leg.sh --reuse e2e -- bash scripts/run-e2e.sh,@bash scripts/leg.sh --skipped e2e; bash scripts/browser-status.sh >/dev/null || true)

# The selective tier (SH-429). Identical to `test` except the rust-suite leg
# runs `scripts/run-changed.sh` (which asks `scripts/select-tests.sh` what is
# affected and runs only that) instead of the whole workspace, and the
# postlude reads back whichever tier that leg actually earned —
# `changed <base>` for a genuine subset, or plain `gate` whenever an escape
# hatch ran everything, so the receipt never claims less than what actually
# ran. `docs/spec/selective-testing.md` is the design of record; the postlude
# staying the LAST line here, exactly as it is for `test`, is what
# `tests/selective_gate.rs` pins.
#
# `$$tier_args` below is deliberately UNQUOTED in the postlude call:
# `scripts/run-changed.sh` writes either `gate` (one word) or
# `changed <base-tree>` (two), and postlude's own $2/$3 need them as separate
# positional arguments — quoting would pass `changed <base-tree>` as a
# single, wrong $2.
test-changed: check-no-orphan-servers
	@bash scripts/gate-receipt.sh preflight
	@bash scripts/with-orphan-postlude.sh $(if $(STORYHOOK_MAKE_NO_EXEC),--make-no-exec) -- $(MAKE) --no-print-directory _test-changed-body
	@state_file="$$(git rev-parse --git-dir)/storyhook-changed-tier-args"; \
	 tier_args="$$(cat "$$state_file" 2>/dev/null)"; \
	 [ -n "$$tier_args" ] || tier_args=gate; \
	 rm -f "$$state_file"; \
	 bash scripts/gate-receipt.sh postlude $$tier_args

_test-changed-body:
	bash scripts/leg.sh --reuse fmt -- cargo fmt --all -- --check
	bash scripts/leg.sh --reuse clippy -- cargo clippy --workspace --all-targets -- -D warnings
	@bash scripts/leg.sh --reuse rust-suite -- bash scripts/run-changed.sh
	@bash scripts/leg.sh --reuse rust-contracts -- bash scripts/run-rust-battery.sh contracts
	bash scripts/leg.sh --reuse build -- cargo build
	PATH="$(CURDIR)/target/debug:$$PATH" bash scripts/leg.sh --reuse plugin -- bash plugins/story/tests/run-tests.sh
	@bash scripts/leg.sh --skipped e2e; bash scripts/browser-status.sh >/dev/null || true

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

# Retained as a migration aid after SH-521 retired the every-open-PR sweep.
# Verification now begins when one linked story moves to `verifying`.
merge-watch:
	@echo "merge-watch is retired; link one PR and move its story to verifying"

# The browser tier's detection layer (SH-418).
#
# `make test-full` is the release gate, and until SH-418 nothing ran it
# between releases: the browser suite ran when a human chose to and when
# `scripts/release.sh` demanded it, so a dashboard regression could merge and
# sit red until it blocked a release. Measured when this landed: 109 receipts
# in this machine's store, ZERO carrying `tier full`.
#
# `browser-watch` is ONE pass — if `origin/main`'s tip tree has no `tier full`
# receipt, it runs `make test-full` against that tip in its own persistent
# worktree, under a lock, and the ordinary `gate-receipt.sh` postlude
# certifies it. It installs no timer of its own; the recurrence is a per-machine
# bootstrap step. It wants a COARSE one: the browser leg measured 1454s
# (24.2 minutes) here, so a pass
# is the wrong shape for a 1-3 minute cadence and has its own lock to prove
# it. Its worktree needs `make e2e-install` run in it once; the script refuses
# with that command rather than running it, and refuses BEFORE spending the
# Rust legs.
#
# `browser-status` reports how far `main` is from the last tree the browser
# suite certified — commits-behind and age, or `never`. Read-only,
# millisecond-cheap, no cached marker anywhere: distance is computed from the
# receipt store per read, so a poller that has died, a `main` that is red, and
# a machine that has never run the suite are three readings on one scale that
# only grows. The dashboard and explicit status command remain its consumers.
browser-watch:
	bash scripts/browser-watch.sh

browser-status:
	@bash scripts/browser-status.sh

# The coverage tier's own detection layer (SH-429), the same shape as the
# browser tier's three targets just above — a council verdict on this story
# chose that shape explicitly (`docs/spec/selective-testing.md`).
#
# `make coverage-map` captures a coverage map for whatever tree is checked
# out HERE, right now — the direct, local-iteration entry point, requiring a
# `gate`/`full` receipt for this tree already on file (`scripts/coverage-
# map.sh` refuses without one).
#
# `make coverage-watch` is the poller: one pass, keyed to whether
# `origin/main`'s tip tree already has a map, running in its own persistent,
# locked worktree (separate from `browser-watch-worktree` — an instrumented
# build lives in a separate `target-coverage/`, so sharing a worktree would
# mean the two pollers evict each other's warm build on every alternating
# run). Meant to be re-run every few minutes by something that already exists
# on the machine, the same bootstrap posture `make browser-watch` takes.
#
# `make coverage-status` reports the distance — commits-behind or `never` —
# with no bootstrap needed at all, the same reason `make browser-status`
# exists at that tier.
coverage-map:
	bash scripts/coverage-map.sh

coverage-watch:
	bash scripts/coverage-watch.sh

coverage-status:
	@bash scripts/coverage-status.sh

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

# A disposable storyhook: this checkout's binary, a throwaway store, a daemon
# that dies with the shell it drops you into.
#
# The counterpart to `install` below, and the reason to reach for it first.
# `./target/debug/story list`, typed here, resolves the REAL store and the real
# daemon on 3456 -- `is_test_build` does not stop a `cargo build` binary, and
# this repository's committed `.storyhook.toml` names the project storyhook
# tracks itself with. Before this target the only way to exercise a change by
# hand was `make install`, which replaces the binary everything else on the
# machine runs.
#
# The isolation is the test suite's own (`scripts/test-env.sh`, documented by
# `story help test-environment`), so exercising a change by hand runs under the
# same contract as the gate rather than a weaker one.
#
#   make scratch                          a shell in the "default" environment
#   make scratch ARGS="--test-build"      ...running a build with crash points
#   make scratch ARGS="--name x --fresh"  a second, empty environment
#   make scratch-clean                    delete all of them
scratch:
	@bash scripts/scratch-env.sh $(ARGS)

# Every scratch environment at once. Nothing outside /private/tmp is ever
# named: `scratch-env.sh` refuses a root anywhere else, so there is nowhere
# else for one to be.
scratch-clean:
	@rm -rf /private/tmp/storyhook-scratch
	@echo "removed /private/tmp/storyhook-scratch"

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
#
# Reports the WHOLE `--version` line, not just the bare semver -- SH-406
# stamps every build with a build id derived from its tracked git content
# (build.rs), so two installs of the same VERSION distinguish themselves here
# whenever their tracked content differs.
install: release-build
	@mkdir -p "$(INSTALL_DIR)"
	install -m 755 target/release/story "$(INSTALL_DIR)/story"
	@echo "Installed $$("$(INSTALL_DIR)/story" --version) to $(INSTALL_DIR)/story"
