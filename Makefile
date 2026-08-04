# Storyhook developer tasks.
#
# `make test` is the gate, and the only one — there is no push/PR CI (see
# .github/workflows/release.yml, which triggers on version tags only), so the
# local pre-push hook is it. Skipping it is how a non-compiling commit reaches
# `main` undetected (see #23).
#
# It used to be one of two: `make test-daemon` ran the identical suite a second
# time with every CLI command going over `/api/v1/invoke`, and `make gate` ran
# both. That leg existed because there were two transports, and it is gone with
# the second one (SH-114). What it caught — a fixture that is only correct when
# nothing else holds the store — is now caught by the only run there is, because
# every command in it goes through a daemon.
#
# `scripts/run-e2e.sh` is the one leg `cargo test` cannot cover: the dashboard's
# JavaScript has no Rust to exercise it, so it is driven in a real browser
# instead (see `e2e/`). It fails the whole gate loudly rather than skipping if
# its Node toolchain was never installed (`make e2e-install`) — a green `make
# test` that quietly skipped the browser suite would say "the dashboard was
# verified" when nothing ran, which is worse than a red one.

.PHONY: test build fmt lint clippy check release-build install check-no-orphan-servers e2e-install e2e

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
test: check-no-orphan-servers
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets -- -D warnings
	@bash scripts/run-tests.sh -- --test-threads=4
	cargo build
	PATH="$(CURDIR)/target/debug:$$PATH" bash plugin/claude-code/tests/run-tests.sh
	bash scripts/run-e2e.sh
	@bash scripts/check-no-orphan-servers.sh postlude

# Installs the e2e/ Node toolchain and Playwright's chromium browser. Not part
# of `test` itself -- it is a one-time (per-machine, per-Playwright-version)
# bootstrap step, not something every run should repeat -- but `test`'s e2e
# leg fails loudly, naming this target, if it was never run.
e2e-install:
	cd e2e && npm ci
	cd e2e && npx playwright install --with-deps chromium

# Runs just the dashboard's browser suite, against an isolated daemon this
# starts and stops. Pass Playwright CLI flags through, e.g. `make e2e ARGS=--headed`.
e2e:
	bash scripts/run-e2e.sh $(ARGS)

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
