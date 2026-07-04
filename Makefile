# Storyhook developer tasks.
#
# `make test` is the canonical pre-push gate: it runs the same checks as CI
# (formatting, clippy with warnings-as-errors, and the full test suite).

.PHONY: test build fmt lint clippy check release-build

# Full local gate — mirrors .github/workflows/ci.yml.
test:
	cargo fmt -- --check
	cargo clippy --all-targets -- -D warnings
	cargo test

# Debug build of the `story` binary.
build:
	cargo build

# Apply formatting in place.
fmt:
	cargo fmt

# Lint only (warnings treated as errors).
lint clippy:
	cargo clippy --all-targets -- -D warnings

# Fast type-check without producing a binary.
check:
	cargo check --all-targets

# Optimized release build.
release-build:
	cargo build --release
