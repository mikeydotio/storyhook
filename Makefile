# Storyhook developer tasks.
#
# `make test` is the per-commit build/test gate, and the only one enforced —
# there is no push/PR CI (see .github/workflows/release.yml, which triggers on
# version tags only), so the local pre-push hook is it. Skipping it is how a
# non-compiling commit reaches `main` undetected (see #23).
#
# `make gate` is `test` plus the daemon leg, and is what a wave ends with and
# what a change to the tests should run. The split is measured and argued for
# on those two targets below.

.PHONY: gate test test-daemon build fmt lint clippy check release-build install check-no-orphan-servers

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
test: check-no-orphan-servers
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets -- -D warnings
	@bash scripts/run-tests.sh
	cargo build
	PATH="$(CURDIR)/target/debug:$$PATH" bash plugin/claude-code/tests/run-tests.sh
	@bash scripts/check-no-orphan-servers.sh postlude

# Everything, both legs. What a wave ends with and what a push should run.
#
# `make test` is the per-commit gate and is what the pre-push hook invokes; this
# is the wider one. Measured 2026-07-29 on the M1 Max: `test` 68s warm (114s when
# it has compiling to do), `test-daemon` 51-60s, so `gate` lands between 120s and
# 175s depending on how much is already built. That is why the daemon leg is not
# simply folded into `test`: the suite budget's hard ceiling is 180s and its
# target is 120s, and a per-commit gate sitting on the target when warm has
# nothing left for a busy machine.
#
# Run it whenever tests are added or changed, and before opening a PR. That is
# the moment the daemon leg earns its keep — see the comment on `test-daemon`.
gate: test test-daemon

# The same integration suite, over RPC.
#
# `make test` runs every command in its own process; this runs the identical
# tests with `STORYHOOK_INVOKER=daemon`, so each CLI-driven one goes over
# `/api/v1/invoke` to a real daemon. The in-process tests (`service_`, `store_`)
# are unaffected — they call the library directly and never see an invoker at
# all.
#
# **Not part of `make test`, and not redundant either.** The property that the
# two modes *agree* is proved by the byte-comparison test, which is in `make
# test`. What this leg finds is different and it found six instances of it in
# W8 alone: tests that are only correct when nothing else holds the store. A
# running daemon keeps the database open with its own page cache and its own
# write-ahead-log handle, so a fixture that assumes an empty backup directory,
# or asks about bytes on disk through a client, quietly means something else.
# Every one of those was a test defect — and a test that is wrong in one mode is
# a test that can hide a product defect in both.
#
# That is a hazard introduced when a test is *written*, not when unrelated code
# changes, which is what makes a per-wave gate (`make gate`) the right cadence
# rather than a per-commit one.
#
# Isolation is the same as `make test`'s, which is what makes this safe to run:
# a private data directory, a private state home, port 0, and a parent-pid
# contract that kills any daemon this run leaves behind.
#
# `--test-threads=4` is a bound on how many daemons exist at once, not tuning.
# Each test binary's shared environment gets a daemon and each isolated one gets
# another. W5 measured the leg stalling wide open —
# `move_if_state_under_real_concurrency_yields_exactly_one_winner` passed in
# 1.2s alone and had not finished after 60s in the unbounded run — and adopted
# the bound.
#
# Re-measured in W8 (2026-07-29): the leg now passes at every parallelism tried,
# including the default. Three consecutive green runs unbounded at 51s, 52s,
# 51s; `8` at 53s; `4` at 55s; `2` at 67s. The bound is kept anyway, and
# deliberately:
#
#   - it costs four seconds on a 51-second leg;
#   - what it retires is a *stall*, which is the worst thing a gate can do —
#     no signal, no failing test name, and a wall-clock timeout several layers
#     up long after the evidence is gone;
#   - and the quantity it bounds grows with the suite. W8 alone added four test
#     files that take a `TestEnv::isolated()`, each of which is another daemon.
#     A ceiling that is comfortable at today's count is not self-evidently
#     comfortable at twice it.
#
# Raise it if the leg's wall clock ever becomes the problem, but re-measure
# first: the symptom of overshooting is a stall, not a failure.
test-daemon: check-no-orphan-servers
	cargo build
	@STORYHOOK_INVOKER=daemon bash scripts/run-tests.sh -- --test-threads=4
	@bash scripts/check-no-orphan-servers.sh postlude

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
