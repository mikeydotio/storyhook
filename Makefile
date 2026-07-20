# Storyhook developer tasks.
#
# `make test` is the *only* build/test gate this project runs — there is no
# push/PR CI (see .github/workflows/release.yml, which triggers on version
# tags only). It's enforced locally by the pre-push hook, so it must be run
# (and pass) before every push; skipping it is how a non-compiling commit
# reaches `main` undetected (see #23).

.PHONY: test build fmt lint clippy check release-build

# Full local gate: formatting, clippy with warnings-as-errors, full test suite.
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
