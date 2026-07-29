# Storyhook developer tasks.
#
# `make test` is the *only* build/test gate this project runs — there is no
# push/PR CI (see .github/workflows/release.yml, which triggers on version
# tags only). It's enforced locally by the pre-push hook, so it must be run
# (and pass) before every push; skipping it is how a non-compiling commit
# reaches `main` undetected (see #23).

.PHONY: test test-daemon build fmt lint clippy check release-build install check-no-orphan-servers

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
test: check-no-orphan-servers
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets -- -D warnings
	@bash scripts/run-tests.sh
	cargo build
	PATH="$(CURDIR)/target/debug:$$PATH" bash plugin/claude-code/tests/run-tests.sh
	@bash scripts/check-no-orphan-servers.sh postlude

# The same integration suite, over RPC.
#
# `make test` runs every command in its own process; this runs the identical
# tests with `STORYHOOK_INVOKER=daemon`, so each CLI-driven one goes over
# `/api/v1/invoke` to a real daemon. The in-process tests (`service_`, `store_`,
# `differential_`) are unaffected — they call the library directly and never
# see an invoker at all.
#
# Deliberately NOT part of `make test`. Two thousand tests each taking a
# network hop through one shared daemon would be slower and would couple every
# test to one process's health, and the property that matters — that the two
# modes agree — is proved by the byte-comparison test rather than by running
# everything twice.
#
# Isolation is the same as `make test`'s, which is what makes this safe to run:
# a private data directory, a private state home, port 0, and a parent-pid
# contract that kills any daemon this run leaves behind.
# `--test-threads=4` is not tuning, it is a bound on how many daemons exist at
# once. Each test binary's shared environment gets a daemon, and each isolated
# one gets another; the default parallelism starts dozens simultaneously, and
# the machine spends its time context-switching between SQLite processes rather
# than running tests. Measured: the suite passes binary-by-binary and stalls
# wide open.
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
