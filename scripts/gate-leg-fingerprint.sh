#!/usr/bin/env bash
#
# Prints the content fingerprint for one reusable gate leg.
#
# `scripts/leg.sh --reuse` records a successful leg under this fingerprint.
# A later gate can reuse that result only while every tracked input that can
# affect the leg is byte-identical and the command invocation is identical.
# This is deliberately narrower than `scripts/tracked-tree.sh`: a browser
# failure does not make a still-identical Rust formatting, lint, test, build,
# or plugin result false.
#
# The scopes are conservative dependency sets, not directory ownership. The
# ordinary `rust-suite` owns Rust sources, fixtures, and doctests. Checkout-
# reading Rust tests live in the separate `rust-contracts` battery, whose
# inputs are the tracked tree. A browser edit can therefore invalidate those
# related contract checks without throwing away the unrelated core Rust run.
#
# Interface:
#   gate-leg-fingerprint.sh <fmt|clippy|rust-suite|rust-contracts|build|plugin|e2e> [argv...]
#
# stdout is exactly one git object id. All failures are nonzero and loud.

set -euo pipefail

label="${1:-}"
[ -n "$label" ] || {
    echo "gate-leg-fingerprint: missing leg label" >&2
    exit 1
}
shift

case "$label" in
(fmt | clippy | rust-suite | rust-contracts | build | plugin | e2e) ;;
(*)
    echo "gate-leg-fingerprint: unknown reusable leg '$label'" >&2
    exit 1
    ;;
esac

root="$(git rev-parse --show-toplevel 2>/dev/null)" || {
    echo "gate-leg-fingerprint: not inside a git worktree" >&2
    exit 1
}
cd "$root"

# Use the same derived target partition for evidence that `run-rust-battery.sh`
# uses for execution. A contract test edit must not invalidate the ordinary
# Rust battery merely because both kinds of integration target live under
# `tests/`.
core_rust_targets=""
if [ "$label" = "rust-suite" ]; then
    core_rust_targets="$(bash scripts/rust-test-targets.sh core)"
fi

is_core_rust_target_path() {
    path="$1"
    [ "${path%/*}" = "tests" ] || return 1
    case "$path" in
    (*.rs) ;;
    (*) return 1 ;;
    esac
    target="${path##*/}"
    target="${target%.rs}"
    printf '%s\n' "$core_rust_targets" | grep -Fqx -- "$target"
}

# Inputs shared by every cached verdict. If the orchestration or the scope
# definition changes, no result produced under the previous contract is
# silently reused.
is_gate_contract() {
    case "$1" in
    (Makefile | scripts/leg.sh | scripts/gate-leg-fingerprint.sh) return 0 ;;
    (*) return 1 ;;
    esac
}

# Everything that can change the production Rust binary. Cargo manifests are
# matched at any depth so adding a workspace crate invalidates the result
# without adding another arm here.
is_production_rust() {
    case "$1" in
    (Cargo.toml | Cargo.lock | build.rs | VERSION | clippy.toml | .cargo/*) return 0 ;;
    (*Cargo.toml | src/* | crates/*) return 0 ;;
    (*) return 1 ;;
    esac
}

# The dashboard is compiled into the binary through `include_str!`, so a page
# edit must rebuild the executable. It is not Rust source, however, and the
# checkout-contract battery owns the Rust assertions that inspect it. Keeping
# that boundary explicit prevents a browser fix from throwing away formatting,
# lint, core-Rust, or shell-plugin evidence merely because the page lives under
# `src/`.
is_non_dashboard_production_rust() {
    [ "$1" != "src/web_dashboard.html" ] || return 1
    is_production_rust "$1"
}

is_rust_lint_input() {
    case "$1" in
    (Cargo.toml | Cargo.lock | build.rs | clippy.toml | .cargo/*) return 0 ;;
    (*Cargo.toml | *.rs) return 0 ;;
    (*) return 1 ;;
    esac
}

is_input() {
    path="$1"
    is_gate_contract "$path" && return 0

    case "$label" in
    (fmt)
        case "$path" in
        (*.rs | Cargo.toml | *Cargo.toml | rustfmt.toml | .rustfmt.toml) return 0 ;;
        esac
        ;;
    (clippy)
        is_rust_lint_input "$path" && return 0
        ;;
    (rust-suite)
        is_non_dashboard_production_rust "$path" && return 0
        case "$path" in
        (tests/*.rs)
            if [ "${path%/*}" = "tests" ]; then
                is_core_rust_target_path "$path" && return 0
            else
                # A nested module can be shared by more than one integration
                # target; without a Rust dependency graph it stays a
                # conservative input to core.
                return 0
            fi
            ;;
        (tests/* | scripts/run-tests.sh | scripts/run-rust-battery.sh | scripts/rust-test-targets.sh) return 0 ;;
        esac
        ;;
    (rust-contracts)
        # This deliberately smaller *execution* battery reads every major
        # repository surface at runtime, so its input fingerprint is the
        # tracked tree. It absorbs cross-space invalidation without forcing
        # the ordinary Rust battery to execute again.
        return 0
        ;;
    (build)
        is_production_rust "$path" && return 0
        ;;
    (plugin)
        is_non_dashboard_production_rust "$path" && return 0
        case "$path" in
        (plugins/story/* | .agents/plugins/marketplace.json | .claude-plugin/marketplace.json) return 0 ;;
        esac
        ;;
    (e2e)
        is_production_rust "$path" && return 0
        case "$path" in
        (e2e/* | plugins/story/* | scripts/run-e2e.sh | .storyhook.toml) return 0 ;;
        esac
        ;;
    esac

    return 1
}

# Batch every selected path through one `git hash-object --stdin-paths` call.
# Spawning one git process per file makes the whole-tree Rust fingerprint take
# tens of seconds on this repository; batching keeps all seven gate fingerprints
# below a second while producing the same content-addressed answer.
paths="$(mktemp -t storyhook-gate-leg-paths.XXXXXX)"
hashes="$(mktemp -t storyhook-gate-leg-hashes.XXXXXX)"
missing="$(mktemp -t storyhook-gate-leg-missing.XXXXXX)"
cleanup() {
    rm -f "$paths" "$hashes" "$missing"
}
trap cleanup EXIT

while IFS= read -r path; do
    is_input "$path" || continue
    if [ -e "$path" ] || [ -L "$path" ]; then
        printf '%s\n' "$path" >>"$paths"
    else
        # A tracked deletion is an input too. `git ls-files` still names it,
        # so preserve the distinction from an empty file.
        printf '%s\tMISSING\n' "$path" >>"$missing"
    fi
done < <(git ls-files)

git hash-object --stdin-paths --no-filters <"$paths" >"$hashes"

# NUL separators make argv unambiguous. Tracked filenames in this repository
# cannot contain newlines: the batch interface above is line-oriented, as are
# the project's existing `git ls-files`-derived gate scripts.
{
    printf 'storyhook-gate-leg-v1\0'
    printf 'label\0%s\0argv\0' "$label"
    for arg in "$@"; do
        printf '%s\0' "$arg"
    done
    printf 'files\0'
    paste "$paths" "$hashes"
    cat "$missing"
} | git hash-object --stdin
