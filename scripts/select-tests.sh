#!/usr/bin/env bash
#
# Decides which test binaries `make test-changed` actually runs — SH-429.
#
# The whole decision lives here, in one place a test can drive without
# mocking anything (`tests/selective_gate.rs`), per the lesson SH-418 already
# drew from `scripts/merge-watch.sh`: a poller that only orchestrates an
# already-tested primitive needs no test of its own, but the primitive itself
# does. `scripts/coverage-map.sh` and `scripts/run-tests.sh` are both thin by
# comparison — this is where "run everything" versus "run these" is decided.
#
# OUTPUT CONTRACT. stdout, line 1 is always `BASELINE <tree-oid>` or
# `BASELINE NONE` (no fully-certified ancestor was found at all). Every line
# after that is either the single word `ALL` (run everything — the caller
# must not try to enumerate binaries in this case) or zero or more test
# binary names, one per line, sorted, deduplicated — zero lines is a valid
# answer meaning nothing needs to run. All reasoning goes to stderr. Exit
# status is 0 whenever an answer was produced (which is always, by design —
# every escape hatch below still produces a real answer); nonzero only for a
# genuine environment failure (not inside a git worktree, etc).
#
# NO MULTI-HOP CHAINS. The baseline this script diffs against is always the
# NEAREST tree with a `gate` or `full` receipt — never a `changed` receipt
# from a previous selective run. Walking `git log --first-parent` past a
# `changed`-tier tree rather than stopping there is what keeps this to one
# hop: the selected set can only ever miss something a FULL run would have
# caught relative to the LAST full run, never relative to an accumulating
# chain of prior selective guesses. The practical consequence is that the
# selected set grows the longer a branch goes without a full `make test` —
# staleness costs wall-clock, by running more, never soundness.
#
# COVERAGE ONLY EVER ADDS BINARIES. `coverage-map.sh`'s own header explains
# why: ~57 test files in this repo read `CARGO_MANIFEST_DIR` at runtime and
# ~19 shell out to `git ls-files`, both invisible to LLVM line coverage. Three
# unconditional escape hatches sit on top of the map for exactly that reason,
# and every one of them is checked BEFORE the map is ever consulted:
#
#   1. No map for the resolved baseline           -> ALL
#   2. Any changed path outside src/**.rs,
#      crates/**.rs, tests/*.rs                    -> ALL
#   3. (never bypassed) every binary whose OWN test source names
#      `git ls-files`, `CARGO_MANIFEST_DIR` or `include_str!` — derived by
#      scanning `tests/*.rs` at selection time, never a hand-kept list
#      (CLAUDE.md: SH-136, SH-198, SH-258, SH-260/276, SH-360 are five
#      recorded costs of exactly that shape) — is added to the selection
#      regardless of what the coverage map says.

set -uo pipefail

die() {
    printf 'select-tests: %s\n' "$1" >&2
    exit 1
}

note() {
    printf 'select-tests: %s\n' "$1" >&2
}

root="$(git rev-parse --show-toplevel 2>/dev/null)" \
    || die "not inside a git worktree"
cd "$root" || die "cannot enter $root"

common_dir="$(cd "$(git rev-parse --git-common-dir)" && pwd)" \
    || die "cannot resolve the shared git directory"

receipts="$common_dir/storyhook/gate-receipts"
maps_dir="$common_dir/storyhook/coverage-maps"

current_tree="$("$(dirname "${BASH_SOURCE[0]}")/tracked-tree.sh")" \
    || die "could not resolve this worktree's tracked content"

# ---------------------------------------------------------------------------
# Resolve the nearest fully-certified (gate|full) ancestor
# ---------------------------------------------------------------------------

# `--first-parent`, the same choice `scripts/browser-status.sh` already made
# and for the same reason: a commit a merge brought in from elsewhere was
# never THIS branch's own content, so it should never satisfy "the nearest
# tree I myself made green." On an ordinary feature branch with no merges
# into it, first-parent and full ancestry are identical, so this costs
# nothing in the common case and only matters when it matters.
baseline_tree=""
baseline_commit=""
# shellcheck disable=SC2162
while read -r commit ctree; do
    [ -n "$commit" ] || continue
    tier=""
    if [ -f "$receipts/$ctree" ]; then
        tier="$(sed -n 's/^tier //p' "$receipts/$ctree" 2>/dev/null | head -n1)"
        tier="${tier:-gate}"
    fi
    # Leading `(` on every pattern below: macOS's bash 3.2 (still the system
    # `bash`, GPLv3 having frozen it there) can misparse a bare `case` arm
    # when it sits near a `)`-closed construct -- this whole loop reads from
    # a `<(...)` process substitution -- and the leading paren is the
    # standard, harmless disambiguation.
    case "$tier" in
    (gate | full)
        baseline_tree="$ctree"
        baseline_commit="$commit"
        break
        ;;
    esac
done < <(git log --first-parent --max-count=2000 --format='%H %T' HEAD 2>/dev/null)

# The current tree's own uncommitted state might itself already carry the
# tip commit's tree if nothing is staged/dirty — HEAD is included in the walk
# above via `git log ... HEAD`, so this needs no special case.

if [ -z "$baseline_tree" ]; then
    printf 'BASELINE NONE\n'
    printf 'ALL\n'
    note "no ancestor of HEAD (searched up to 2000 first-parent commits) carries a \
gate/full receipt -- running everything"
    exit 0
fi

note "baseline: $baseline_commit (tree $baseline_tree)"
printf 'BASELINE %s\n' "$baseline_tree"

if [ "$baseline_tree" = "$current_tree" ]; then
    note "no changes since the baseline -- nothing to run"
    exit 0
fi

# ---------------------------------------------------------------------------
# Escape hatch 1: no coverage map for this exact baseline
# ---------------------------------------------------------------------------

map="$maps_dir/$baseline_tree"
if [ ! -f "$map" ]; then
    printf 'ALL\n'
    note "no coverage map for baseline $baseline_tree -- running everything. \
Run 'make coverage-map' (or let 'make coverage-watch' catch up) to capture one."
    exit 0
fi

# ---------------------------------------------------------------------------
# What changed between the baseline and the current tree
# ---------------------------------------------------------------------------

changed="$(git diff --name-only "$baseline_tree" "$current_tree" 2>/dev/null)" \
    || die "could not diff baseline tree $baseline_tree against the current tree"

if [ -z "$changed" ]; then
    note "the diff between baseline and current tree is empty -- nothing to run"
    exit 0
fi

# ---------------------------------------------------------------------------
# Escape hatch 2: any changed path outside src/**.rs, crates/**.rs, tests/*.rs
# ---------------------------------------------------------------------------

outside=""
while IFS= read -r f; do
    [ -n "$f" ] || continue
    # A `case` pattern's `*` matches across `/` in bash (pure string
    # matching, not filesystem pathname expansion), so `src/*.rs` already
    # matches any depth -- `src/store/sqlite/read.rs` included -- with no
    # need to enumerate levels.
    case "$f" in
    (src/*.rs) ;;
    (crates/*/src/*.rs) ;;
    (tests/*.rs) ;;
    (*) outside="$outside$f
" ;;
    esac
done <<<"$changed"

if [ -n "$outside" ]; then
    printf 'ALL\n'
    note "changed path(s) outside src/**.rs, crates/**.rs, tests/*.rs -- running everything:"
    printf '%s' "$outside" | sed 's/^/  /' >&2
    exit 0
fi

# ---------------------------------------------------------------------------
# The derived tree-scanning set (escape hatch 3, always applied)
# ---------------------------------------------------------------------------

# Every integration test file whose own source shells out to `git ls-files`,
# reads `CARGO_MANIFEST_DIR`, or embeds a tracked file via `include_str!` --
# each is a blind spot coverage cannot see into (the file it reads or scans
# is not "executed", so no line of it is ever instrumented), so each of these
# binaries always runs regardless of what changed. Derived over `git ls-files`
# at selection time, never a hand-kept list -- CLAUDE.md's own
# SH-136/SH-198/SH-258/SH-260-276/SH-360 doctrine, applied here rather than
# repeated a sixth time.
tree_scanning="$(git grep -l -E 'git ls-files|CARGO_MANIFEST_DIR|include_str!' -- 'tests/*.rs' 2>/dev/null \
    | sed -n 's#^tests/\(.*\)\.rs$#\1#p')"

# ---------------------------------------------------------------------------
# Binaries the coverage map says are affected, plus any changed test file's
# own binary (a file the map has never seen, because it is new)
# ---------------------------------------------------------------------------

selected="$(
    {
        printf '%s\n' "$tree_scanning"
        while IFS= read -r f; do
            [ -n "$f" ] || continue
            # Leading `(` on every pattern: this case sits inside a $(...)
            # command substitution, and macOS's bash 3.2 (the system `bash`)
            # can misparse a bare `*)` arm there -- measured directly, see
            # tests/selective_gate.rs's own regression pin for this shape.
            case "$f" in
            (tests/*.rs)
                printf '%s\n' "${f#tests/}" | sed 's/\.rs$//'
                ;;
            (*)
                # awk with -F TAB, rather than grep -F: the map's second
                # column is a REPO-RELATIVE PATH, and a substring match would
                # also match a changed file that is merely a substring of
                # some other file's path -- src/api/wire.rs inside a
                # hypothetical src/api/wire.rs.bak, for instance -- which an
                # exact field comparison cannot.
                awk -F'\t' -v f="$f" '$2 == f { print $1 }' "$map"
                ;;
            esac
        done <<<"$changed"
    } | LC_ALL=C sort -u | sed '/^$/d'
)"

if [ -z "$selected" ]; then
    note "the coverage map found no binary affected by the changed file(s) -- nothing to run"
    exit 0
fi

count="$(printf '%s\n' "$selected" | wc -l | tr -d ' ')"
note "$count binaries selected against baseline $baseline_tree"
printf '%s\n' "$selected"
