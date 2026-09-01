#!/usr/bin/env bash
#
# Captures a deterministic, per-test-binary source-file coverage map against
# a green baseline tree — SH-429.
#
# `scripts/select-tests.sh` is what USES this map to decide which test
# binaries a `make test-changed` run actually executes. This script only
# CAPTURES it: for the tree currently checked out (which must already carry a
# `gate` or `full` `gate-receipt.sh` receipt — this script never decides
# greenness, it reuses the decision that mechanism already made), it builds
# every workspace test binary with LLVM source-based coverage instrumentation
# (`-C instrument-coverage`), runs each one, and records which tracked source
# files each one touched. The output is a flat TSV,
# `<test-binary>\t<repo-relative-file>`, one line per pair, sorted — never a
# hand-maintained list, and never anything a human is expected to read
# directly; `select-tests.sh` greps it.
#
# WHY THIS IS SOUND ONLY AS AN ADDITIVE SIGNAL. LLVM line coverage sees which
# LINES OF RUST SOURCE executed. It cannot see a test that reads a tracked
# file directly (`CARGO_MANIFEST_DIR`-relative reads — ~57 files in this repo)
# or shells out to `git ls-files` (~19 files) to scan the tree at runtime: the
# executed lines there belong to `git`, or to a `std::fs::read` call whose own
# body is generic across every file it might read, not to whichever *specific*
# file happened to be read this run. `select-tests.sh` therefore never trusts
# this map to prove a binary UNAFFECTED — it only ever lets the map ADD
# binaries to a run, on top of unconditional escape hatches (a changed path
# outside `src/**.rs`/`crates/**.rs`/`tests/*.rs`, or a binary whose own
# source names `git ls-files`/`CARGO_MANIFEST_DIR`/`include_str!`, derived at
# selection time) that run regardless of what this map says.
#
# THE STORAGE LOCATION mirrors `gate-receipt.sh`'s own: keyed by the tracked
# tree oid, under `$(git rev-parse --git-common-dir)/storyhook/coverage-maps`
# — shared across worktrees (content is the key, so two worktrees holding the
# byte-identical tree share one map), inside `.git/` so it is never committed
# and never cloned, which is what makes a fresh clone fail closed: no map, no
# selection, `select-tests.sh` runs everything and says so.
#
# THREE THINGS THIS SCRIPT MEASURED THE HARD WAY, recorded here because none
# of them is discoverable from the `-C instrument-coverage` docs alone:
#
# 1. `LLVM_PROFILE_FILE`'s `%c` ("continuous mode") flag — attractive because
#    it is meant to survive a `SIGKILL` by keeping the profile memory-mapped
#    to disk rather than writing once at exit — SILENTLY PRODUCES ALL-ZERO
#    PROFILES on this toolchain: `LLVM Profile Error: Counters section not
#    page-aligned` on stderr, and the run continues as if nothing were wrong.
#    Continuous mode needs the linker to page-align the counters section;
#    macOS's default linker (`ld64`, what `cargo build` uses here) does not.
#    So `%c` is never used below — `%p-%m` (pid, binary signature) is enough
#    to keep every process's profile file distinct, and the cost (a killed
#    process loses its profile) is the same safe direction as every other gap
#    this map already has: it can only ever cause a binary to be included
#    that did not strictly need to be, never the reverse.
# 2. **Most of this project's own logic runs in a DAEMON, not the CLI
#    process being tested**, and the daemon is a background process this
#    script's own test binary spawns, not one it waits on. Coverage only
#    flushes to disk on a NORMAL process exit (`atexit`, which `std::process::
#    exit` still runs — a raw `SIGKILL` does not). `src/daemon/serve.rs`'s
#    `watch_parent` is what makes a daemon exit normally when unattended: it
#    polls whether `STORYHOOK_PARENT_PID` is still alive every
#    `SHUTDOWN_CHECK` (250ms) and calls `std::process::exit(0)` the moment it
#    is not. Measured directly: capturing one test file's coverage with
#    `STORYHOOK_PARENT_PID` pointed at a long-lived process (this script's own
#    shell) produced 15 covered files, ALL client-side (`cli.rs`, `invoke.rs`,
#    `main.rs`...) and NONE from `src/service/*`, `src/store/*` or
#    `src/daemon/serve.rs` — because the daemon that actually ran the query
#    was still sitting there, never having exited. Pointing
#    `STORYHOOK_PARENT_PID` at a dedicated sentinel process instead, and
#    killing that sentinel immediately after the test binary itself exits,
#    produced 52 covered files including every one of those. This is why
#    `run_one_binary` below spawns and kills its own sentinel per binary
#    rather than sharing one for the whole run the way `scripts/run-tests.sh`
#    does — that script does not care WHEN its daemons die, only that they do
#    before it deletes its data directory; this script needs each binary's
#    daemons dead and flushed before it merges that binary's profile, so the
#    next binary's run cannot be blamed for coverage that was actually theirs.
# 3. **The `llvm-profdata`/`llvm-cov` build must share rustc's own LLVM major
#    version**, because the raw profile format rustc emits is versioned to
#    the LLVM release that generated it. This machine has no `rustup` and no
#    `llvm-tools-preview` component; Homebrew's `llvm` formula
#    (`/opt/homebrew/opt/llvm`) happened to be LLVM 22, matching this
#    machine's rustc exactly, while Xcode's own toolchain is a DIFFERENT
#    major (21) and silently produces nonsense if used instead — `resolve_llvm_tools`
#    below checks the version match explicitly rather than assuming a path.
#
# WHY A SEPARATE `CARGO_TARGET_DIR` (`target-coverage/`, gitignored, inside
# whichever worktree this script runs in — never shared across worktrees;
# `docs/spec/test-tiers.md` already ruled that out for `check-no-orphan-
# servers.sh`'s and `non_temporary_dir`'s reasons, which apply here too).
# `-C instrument-coverage` changes every crate's own build fingerprint, so
# sharing the ordinary `target/debug` would mean every `make test` run after a
# coverage capture rebuilds from scratch, and vice versa — a second full
# `target/` is the accepted cost, the same one `scripts/browser-watch.sh`'s
# own dedicated worktree already pays to keep ITS build warm.

set -uo pipefail

die() {
    printf 'coverage-map: %s\n' "$1" >&2
    exit 1
}

note() {
    printf 'coverage-map: %s\n' "$1" >&2
}

root="$(git rev-parse --show-toplevel 2>/dev/null)" \
    || die "not inside a git worktree"
cd "$root" || die "cannot enter $root"

common_dir="$(cd "$(git rev-parse --git-common-dir)" && pwd)" \
    || die "cannot resolve the shared git directory"

receipts="$common_dir/storyhook/gate-receipts"
maps_dir="$common_dir/storyhook/coverage-maps"

tree="$("$(dirname "${BASH_SOURCE[0]}")/tracked-tree.sh")" \
    || die "could not resolve this worktree's tracked content"

# This script never decides greenness — it reuses `gate-receipt.sh`'s
# decision. A `changed` receipt does not qualify: it was itself produced by a
# SELECTIVE run, so building a coverage map from it would let a gap in
# yesterday's selection quietly become a gap in today's map too.
receipt="$receipts/$tree"
[ -f "$receipt" ] \
    || die "no receipt for tree $tree — run 'make test' (or 'make test-full') first. \
A coverage map may only be captured against a tree already proven green."
tier="$(sed -n 's/^tier //p' "$receipt" 2>/dev/null | head -n1)"
tier="${tier:-gate}"
case "$tier" in
gate | full) ;;
*) die "tree $tree carries a '$tier' receipt, not gate/full — refusing to build a map from it" ;;
esac
note "capturing against tree $tree (tier $tier)"

# ---------------------------------------------------------------------------
# Resolve a version-matched llvm-profdata/llvm-cov pair
# ---------------------------------------------------------------------------

rustc_llvm_major="$(rustc -vV 2>/dev/null | sed -n 's/^LLVM version: \([0-9]*\).*/\1/p')"
[ -n "$rustc_llvm_major" ] \
    || die "could not read rustc's own LLVM version from 'rustc -vV'"

llvm_tool_major() {
    "$1" --version 2>/dev/null | sed -n 's/.*LLVM version \([0-9]*\).*/\1/p' | head -n1
}

resolve_llvm_tools() {
    local candidates=(
        "$(command -v llvm-profdata 2>/dev/null || true)"
        "/opt/homebrew/opt/llvm/bin/llvm-profdata"
        "/opt/homebrew/opt/llvm@${rustc_llvm_major}/bin/llvm-profdata"
        "/usr/local/opt/llvm/bin/llvm-profdata"
    )
    local profdata cov dir major
    for profdata in "${candidates[@]}"; do
        [ -n "$profdata" ] && [ -x "$profdata" ] || continue
        dir="$(dirname "$profdata")"
        cov="$dir/llvm-cov"
        [ -x "$cov" ] || continue
        major="$(llvm_tool_major "$profdata")"
        [ "$major" = "$rustc_llvm_major" ] || continue
        printf '%s\n%s\n' "$profdata" "$cov"
        return 0
    done
    return 1
}

llvm_pair="$(resolve_llvm_tools)" \
    || die "no llvm-profdata/llvm-cov matching rustc's LLVM major version ($rustc_llvm_major) \
was found. Checked \$PATH, /opt/homebrew/opt/llvm, /opt/homebrew/opt/llvm@$rustc_llvm_major, \
/usr/local/opt/llvm. Install a matching Homebrew llvm formula (Xcode's own toolchain is a \
different LLVM major and must not be used for this)."
LLVM_PROFDATA="$(printf '%s' "$llvm_pair" | sed -n 1p)"
LLVM_COV="$(printf '%s' "$llvm_pair" | sed -n 2p)"
note "using $LLVM_PROFDATA / $LLVM_COV (LLVM $rustc_llvm_major, matches rustc)"

# ---------------------------------------------------------------------------
# Build every workspace test binary, instrumented
# ---------------------------------------------------------------------------

target_dir="$root/target-coverage"
work="$(mktemp -d /private/tmp/storyhook-coverage.XXXXXX)" \
    || die "could not create a scratch directory"
trap 'rm -rf "$work"' EXIT

note "building (this may recompile everything — instrumentation changes every fingerprint)"
build_json="$work/build.json"
if ! CARGO_TARGET_DIR="$target_dir" RUSTFLAGS="-C instrument-coverage" \
    cargo test --workspace --no-run --message-format=json >"$build_json" 2>"$work/build.stderr"; then
    cat "$work/build.stderr" >&2
    die "the instrumented build failed"
fi

story_bin="$target_dir/debug/story"
[ -x "$story_bin" ] || die "expected the instrumented story binary at $story_bin, not found"

# `target.kind` "test": every `tests/*.rs` integration binary. "lib": each
# crate's own `#[cfg(test)]` unit-test binary (storyhook's and storyhook-
# test-support's). "bin" (the `story` binary itself, built only because
# integration tests resolve it via `assert_cmd::Command::cargo_bin`) is
# excluded — it is not a test to run, only an object every OTHER binary's
# coverage export needs alongside it.
binaries="$(python3 -c '
import json, sys
seen = set()
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    d = json.loads(line)
    if d.get("reason") != "compiler-artifact":
        continue
    if not d.get("profile", {}).get("test"):
        continue
    exe = d.get("executable")
    if not exe:
        continue
    kind = d.get("target", {}).get("kind", [])
    if not any(k in ("test", "lib") for k in kind):
        continue
    name = d["target"]["name"]
    if name in seen:
        continue
    seen.add(name)
    print(f"{name}\t{exe}")
' <"$build_json")" || die "could not parse the instrumented build's own JSON output"

[ -n "$binaries" ] || die "the build produced no test binaries — this script's own JSON parsing \
is broken, not the workspace"

binary_count="$(printf '%s\n' "$binaries" | wc -l | tr -d ' ')"
note "$binary_count test binaries to run"

# ---------------------------------------------------------------------------
# Run each binary in isolation, one at a time
# ---------------------------------------------------------------------------

# How long a binary's own sentinel-death wait polls before giving up on a
# daemon exiting on its own — derived from `SHUTDOWN_CHECK` (`src/daemon/
# serve.rs`, 250ms), the same 40x margin `scripts/check-no-orphan-servers.sh`
# already uses for the identical mechanism (CLAUDE.md, SH-394's "never a bare
# literal" rule) — `tests/selective_gate.rs` pins both scripts to the same
# multiple of the constant they are both waiting on, so the two cannot
# silently drift apart.
readonly PARENT_WATCH_GRACE_SECS=10

data_root="$(mktemp -d /private/tmp/storyhook-coverage-data.XXXXXX)" \
    || die "could not create an isolated data directory"

case "$data_root" in
/private/tmp/*) ;;
*) die "refusing to run with an isolated data dir outside /private/tmp: $data_root" ;;
esac

merged_dir="$work/merged"
mkdir -p "$merged_dir"
map_tmp="$work/map.tsv"
: >"$map_tmp"
failed=""

run_one_binary() {
    local name="$1" exe="$2"
    local profraw_dir="$work/profraw/$name"
    mkdir -p "$profraw_dir"

    local sentinel_pid
    sleep 3600 &
    sentinel_pid=$!

    (
        export STORYHOOK_DATA_DIR="$data_root/data"
        export XDG_STATE_HOME="$data_root/state"
        unset STORYHOOK_STORE_PATH
        # This binary is a compiled test target (tests/gate_leg_reuse.rs among
        # them), which shells back into scripts/leg.sh/make test for its own
        # fixtures (SH-524); an ambient journal from some other daemon-owned
        # verification run must not be inherited and corrupted by it.
        unset STORYHOOK_GATE_PROGRESS
        export STORYHOOK_DAEMON_ADDR="127.0.0.1:0"
        export STORYHOOK_PARENT_PID="$sentinel_pid"
        export INSTA_UPDATE=no
        export LLVM_PROFILE_FILE="$profraw_dir/%p-%m.profraw"
        "$exe" >"$work/last-run.out" 2>&1
    )
    local status=$?

    # Kill the sentinel, then poll for this binary's own daemons to notice and
    # exit on their own — short-circuits well under the ceiling in the
    # ordinary case (watch_parent's own poll is 250ms), never blind-sleeps the
    # ceiling every time.
    kill "$sentinel_pid" 2>/dev/null || true
    local deadline=$((SECONDS + PARENT_WATCH_GRACE_SECS))
    while [ "$SECONDS" -lt "$deadline" ]; do
        pgrep -f "$data_root" >/dev/null 2>&1 || break
        sleep 0.25
    done
    # A straggler past the grace window is killed rather than left to bleed
    # into the next binary's window — best-effort, its coverage is simply
    # incomplete for this run, which is the same safe direction as every
    # other gap in this map.
    # shellcheck disable=SC2046
    kill -9 $(pgrep -f "$data_root" 2>/dev/null) 2>/dev/null || true

    if [ "$status" -ne 0 ]; then
        failed="$failed  $name
"
        note "RED: $name (exit $status) — see $work/last-run.out"
        return
    fi

    local raws
    raws="$(find "$profraw_dir" -name '*.profraw' 2>/dev/null)"
    if [ -z "$raws" ]; then
        # A binary with no test functions of its own kind (rare, but a
        # `#[cfg(test)] mod tests {}` with nothing in it is valid Rust) leaves
        # no profile. Nothing to merge, nothing wrong.
        return
    fi

    local profdata="$merged_dir/$name.profdata"
    if ! "$LLVM_PROFDATA" merge -sparse -o "$profdata" $raws 2>"$work/merge.stderr"; then
        cat "$work/merge.stderr" >&2
        die "llvm-profdata merge failed for $name"
    fi

    "$LLVM_COV" export \
        --instr-profile="$profdata" \
        "$story_bin" \
        --object "$exe" \
        --format=text 2>"$work/export.stderr" \
        | python3 -c "
import json, sys
d = json.load(sys.stdin)
root = '$root/'
name = '$name'
files = set()
for exp in d.get('data', []):
    for f in exp.get('files', []):
        if f['summary']['lines']['covered'] > 0 and f['filename'].startswith(root):
            files.add(f['filename'][len(root):])
for rel in sorted(files):
    print(f'{name}\t{rel}')
" >>"$map_tmp" || die "llvm-cov export failed for $name (see $work/export.stderr)"
}

# shellcheck disable=SC2162
while IFS=$'\t' read -r name exe; do
    [ -n "$name" ] || continue
    run_one_binary "$name" "$exe"
done <<<"$binaries"

if [ -n "$failed" ]; then
    note "refusing to write a map — these binaries did not pass under instrumentation:"
    printf '%s' "$failed" >&2
    die "a coverage map may only be captured from a fully green run"
fi

# ---------------------------------------------------------------------------
# Write the map atomically
# ---------------------------------------------------------------------------

mkdir -p "$maps_dir" || die "could not create $maps_dir"
LC_ALL=C sort -u "$map_tmp" > "$work/map.sorted.tsv"

map_tmp_final="$maps_dir/.tmp.$$"
cp "$work/map.sorted.tsv" "$map_tmp_final" || die "could not stage the map"
mv -f "$map_tmp_final" "$maps_dir/$tree" || die "could not publish the map"

entry_count="$(wc -l <"$work/map.sorted.tsv" | tr -d ' ')"
note "wrote $maps_dir/$tree ($entry_count file/binary pairs across $binary_count binaries)"
