# Storyhook developer tasks.
#
# `make test` is the *only* build/test gate this project runs — there is no
# push/PR CI (see .github/workflows/release.yml, which triggers on version
# tags only). It's enforced locally by the pre-push hook, so it must be run
# (and pass) before every push; skipping it is how a non-compiling commit
# reaches `main` undetected (see #23).

.PHONY: test test-store build fmt lint clippy check release-build install check-no-orphan-servers

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

# The store leg: the same integration suite, served by the store-backed
# invoker instead of `.storyhook/`. This is the strangler's proof engine —
# every failure here is a thing the flip would break, found while the legacy
# path is still the default and a surprise is cheap.
#
# NOT part of `make test`, deliberately. It roughly doubles the gate, and the
# gate is run on every commit of every wave; instead the waves that touch the
# data layer run it alongside `make test` and record both times in
# docs/rearch/STATE.md. It becomes the only leg at the flip.
#
# The exclusion list is documented — file, reason, burn-down wave — in
# docs/rearch/flip-checklist.md, section G. It must only ever shrink.
STORE_LEG_EXCLUDE = \
	--exclude-prefix store_ \
	--exclude-prefix service_ \
	--exclude-prefix differential_ \
	--exclude-file invoker_seam \
	--exclude-file wire_envelope \
	--exclude-file web_test \
	--exclude-file registry_test \
	--exclude-file tui_integration \
	--exclude-file tui_undo \
	--exclude-file worktree_truth \
	--exclude-file doctor \
	--exclude-file event_hooks \
	--exclude-file init_command \
	--exclude-file member_add \
	--exclude-file session_start \
	--exclude-file help_flag_sweep \
	--skip-test init_creates_storyhook_claude_md_with_prefix \
	--skip-test sh35_init_claude_md_does_not_reference_graph_tree \
	--skip-test sh35_init_claude_md_graph_section_only_has_valid_flags \
	--skip-test init_generated_claude_md_does_not_mention_mcp \
	--skip-test delete_archives_and_removes_open_jsonl \
	--skip-test doctor_fix_heals_stale_archived_snapshot_from_before_the_fix \
	--skip-test closed_state_moves_story_to_archive_db \
	--skip-test state_add_accepts_equals_form_flags \
	--skip-test state_add_stores_description_and_role \
	--skip-test state_remove_drops_an_empty_state \
	--skip-test state_set_moves_and_clears_the_active_role \
	--skip-test state_set_updates_description_then_clears_it \
	--skip-test move_if_state_under_real_concurrency_yields_exactly_one_winner \
	--skip-test hook_outputs_empty_json_when_plugin_disabled \
	--skip-test every_error_variant_holds_its_contract

test-store:
	@bash scripts/run-store-leg.sh $(STORE_LEG_EXCLUDE)

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
