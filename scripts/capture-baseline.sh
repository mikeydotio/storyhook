#!/usr/bin/env bash
#
# Captures the regression baseline into docs/rearch/baseline/.
#
# The data-layer rearchitecture replaces the storage engine underneath a CLI
# whose observable behaviour is its entire contract. The only defence against a
# silent regression is a *before* picture precise enough that an *after*
# picture can be diffed against it — so every artifact this writes is
# deterministic, sorted, and regenerable: re-run this at any wave boundary and
# `git diff` is the report. A test that quietly stops existing becomes a
# removed line rather than a mystery.
#
# Artifacts (all under $OUT, all overwritten in place):
#
#   manifest.json      machine tag + artifact inventory with sizes and digests
#   test-inventory.txt every test NAME, per binary, plus the bash suite
#   known-red.md       every #[ignore]d test and the criterion that un-ignores it
#   error-codes.md     the pinned error contract, extracted from its own test
#   timings.json/.md   per-binary and whole-gate wall times, N runs, median
#   flake-census.md    N consecutive `make test` runs, per-test N/N accounting
#   legacy-tree.tar.gz this repo's own .storyhook/ — W3's import-fidelity fixture
#   legacy-tree.txt    that tarball's contents, so the blob is reviewable in git
#   golden-export.json `story export` of the same tree — W3's import oracle
#   archive-schema.sql the legacy archive database's schema, at user_version 0
#
# Usage:
#   scripts/capture-baseline.sh [--out DIR] [--census-runs N] [--timing-runs N]
#                               [--skip-census]
#
# Exits non-zero if any census run was red, so a caller checking only the exit
# status cannot mistake a red baseline for a green one.
#
# Wall-clock cost: roughly (census-runs x 40s) + (timing-runs x 35s). The
# default 10-run census is deliberate — a flake that shows up 1 run in 5 is
# invisible to three runs, and this suite has already had one (SH-51).

# This script writes markdown, so most of its `printf` format strings are
# single-quoted prose full of backticks. Every SC2016 shellcheck reports here is
# a code span in that prose — non-expansion is the point.
# shellcheck disable=SC2016

set -euo pipefail

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$REPO_ROOT/docs/rearch/baseline"
CENSUS_RUNS=10
TIMING_RUNS=3

usage() {
  sed -n '3,32p' "${BASH_SOURCE[0]}" | sed 's/^#\{1,\} \{0,1\}//'
}

while [ $# -gt 0 ]; do
  case "$1" in
  --out)
    OUT="$2"
    shift 2
    ;;
  --census-runs)
    CENSUS_RUNS="$2"
    shift 2
    ;;
  --timing-runs)
    TIMING_RUNS="$2"
    shift 2
    ;;
  --skip-census)
    CENSUS_RUNS=0
    shift
    ;;
  -h | --help)
    usage
    exit 0
    ;;
  *)
    echo "capture-baseline: unknown argument [$1]" >&2
    usage >&2
    exit 2
    ;;
  esac
done

cd "$REPO_ROOT"
mkdir -p "$OUT"

TAB="$(printf '\t')"
WORK="$(mktemp -d /tmp/storyhook-baseline.XXXXXX)"
trap 'rm -rf "$WORK"' EXIT

for tool in cargo rustc jq sqlite3 tar gzip shasum; do
  command -v "$tool" >/dev/null 2>&1 || {
    echo "capture-baseline: required tool [$tool] is not on PATH" >&2
    exit 1
  }
done

# `/usr/bin/time -p -o FILE` is the only wall-clock source available on a stock
# macOS bash 3.2: no $EPOCHREALTIME, and BSD `date` has no %N.
[ -x /usr/bin/time ] || {
  echo "capture-baseline: /usr/bin/time is required for wall-clock measurement" >&2
  exit 1
}

# Mirrors the Makefile. Without it the golden corpus writes `.snap.new` files
# instead of failing, which is both a different code path and a different cost;
# every measurement here must be of the gate as the gate actually runs.
export INSTA_UPDATE=no

say() { printf '==> %s\n' "$*" >&2; }

# Runs a command with its output captured to $1, recording wall seconds in
# REAL_SECONDS. Returns the command's own exit status.
REAL_SECONDS=0
run_timed() {
  local log="$1"
  shift
  local timefile="$WORK/run_timed.time"
  local status=0
  /usr/bin/time -p -o "$timefile" "$@" >"$log" 2>&1 || status=$?
  REAL_SECONDS="$(awk '$1 == "real" { print $2 }' "$timefile")"
  return $status
}

# Median of the numbers on stdin, to 3dp. Empty input yields empty output.
median() {
  sort -n | awk '
    { value[NR] = $1 }
    END {
      if (NR == 0) { exit }
      if (NR % 2) { printf "%.3f\n", value[int((NR + 1) / 2)] }
      else        { printf "%.3f\n", (value[NR / 2] + value[NR / 2 + 1]) / 2 }
    }'
}

# ---------------------------------------------------------------------------
# Isolation — the same contract `scripts/run-tests.sh` provides
# ---------------------------------------------------------------------------

# This script runs test binaries **directly**, outside the Makefile, so it does
# not inherit the wrapper's isolated data directory. Without this block a
# baseline capture writes projects into the developer's real store, which is
# exactly how the junk project W7 found got there. Every test file builds its
# fixtures through the shared harness now (SH-531), which is a second layer and
# not a replacement: this script also runs the golden corpus and `story` itself
# outside any test binary.
#
# Not hypothetical here either: the binary's own test-build guard
# (`storyhook::env::is_test_build`) failed five tests in `cli_error_streams`
# during a W8 capture, naming a fixture at a `$TMPDIR` path that was about to
# run `story init` against the real home.
#
# `/private/tmp` rather than `$TMPDIR`, matching run-tests.sh: the latter is
# Spotlight-indexed (SH-53).
CAPTURE_DATA_ROOT="$(mktemp -d /private/tmp/storyhook-baseline.XXXXXX)"
trap 'rm -rf "$CAPTURE_DATA_ROOT"' EXIT
# THE ISOLATION, in one shared place -- `scripts/test-env.sh`, whose own header
# carries the parameters and the reason for each. `--home` is not passed: the
# flake census below runs `make test`, and cargo with a fake $HOME loses its
# registry and its build cache.
#
# This script used to claim in its own header that it provided "the same
# contract scripts/run-tests.sh provides" while carrying no path guard at all --
# it was the harness the derived scan in tests/store_isolation.rs was written
# after missing. Sourcing the one implementation is how that stops being a claim
# and starts being true.
# shellcheck source=test-env.sh
. "$REPO_ROOT/scripts/test-env.sh"
storyhook_isolate "$CAPTURE_DATA_ROOT"

# The flake census below runs `make test` N times in a row (SH-524); an
# ambient journal path from some other daemon-owned verification run must
# not be inherited, or these unrelated repeated runs would all write into it.
unset STORYHOOK_GATE_PROGRESS
export INSTA_UPDATE=no

# ---------------------------------------------------------------------------
# Machine tag
# ---------------------------------------------------------------------------

CAPTURED_AT="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
HOST="$(hostname -s)"
COMMIT="$(git rev-parse HEAD)"
COMMIT_SHORT="$(git rev-parse --short HEAD)"
BRANCH="$(git rev-parse --abbrev-ref HEAD)"
# Only the code decides whether a capture is reproducible; docs/ churns on
# every run of this very script, so it is deliberately not consulted.
if [ -n "$(git status --porcelain -- src crates tests plugin Cargo.toml Cargo.lock Makefile scripts)" ]; then
  TREE_STATE="dirty"
else
  TREE_STATE="clean"
fi
RUSTC="$(rustc --version)"
CARGO="$(cargo --version)"
OS="$(uname -s) $(sw_vers -productVersion 2>/dev/null || uname -r)"
ARCH="$(uname -m)"
CPU="$(sysctl -n machdep.cpu.brand_string 2>/dev/null || echo unknown)"
CORES="$(sysctl -n hw.ncpu 2>/dev/null || echo 0)"
case "$CORES" in
'' | *[!0-9]*) CORES=0 ;;
esac

PROVENANCE="host \`$HOST\` ($CPU, $CORES cores) · $OS/$ARCH · $RUSTC · commit \`$COMMIT_SHORT\` ($TREE_STATE) · $CAPTURED_AT"

# The standard provenance header every artifact carries.
header() {
  printf '# %s\n\n' "$1"
  printf 'Generated by `scripts/capture-baseline.sh` — do not hand-edit; re-run the\n'
  printf 'script and commit the diff.\n\n'
  printf 'Captured: %s\n\n' "$PROVENANCE"
}

# Wraps $1 as a markdown code span. A fragment containing a backtick — and one
# of the pinned error messages does — needs the doubled-delimiter form or it
# renders as broken markup in every artifact reader.
code_span() {
  case "$1" in
  *'`'*) printf '`` %s ``' "$1" ;;
  *) printf '`%s`' "$1" ;;
  esac
}

say "storyhook baseline capture — $COMMIT_SHORT on $HOST ($TREE_STATE tree)"

# ---------------------------------------------------------------------------
# Build once, so nothing below is measured cold
# ---------------------------------------------------------------------------

say "building (workspace + tests)"
cargo build --workspace --tests >"$WORK/build.log" 2>&1 || {
  cat "$WORK/build.log" >&2
  exit 1
}
cargo build >>"$WORK/build.log" 2>&1
STORY_BIN="$REPO_ROOT/target/debug/story"

# One TSV row per test binary: kind, target name, package, executable path. The
# executable's filename carries a content hash that changes on every rebuild,
# so it is used here and never written into an artifact.
#
# The package name needs both branches below: cargo writes `…#name@version` only
# when the package name differs from its directory's basename, and bare
# `…#version` when they match (`crates/storyhook-test-support`). Getting that
# wrong is not cosmetic — jq's array construction silently drops an ELEMENT when
# `capture` matches nothing, which shifts the executable into the package column
# and reports a whole crate's tests as zero.
cargo test --workspace --no-run --message-format=json 2>/dev/null |
  jq -r '
    select(.reason == "compiler-artifact" and .executable != null and .profile.test == true)
    | [ .target.kind[0]
      , .target.name
      , ( (.package_id | capture("#(?<n>[A-Za-z0-9_.-]+)@") | .n)
          // (.manifest_path | sub("/Cargo\\.toml$"; "") | sub("^.*/"; "")) )
      , .executable
      ] | @tsv' |
  LC_ALL=C sort -t"$TAB" -k1,1 -k2,2 >"$WORK/binaries.tsv"

BINARY_COUNT="$(wc -l <"$WORK/binaries.tsv" | tr -d ' ')"
[ "$BINARY_COUNT" -gt 0 ] || {
  echo "capture-baseline: cargo reported no test binaries" >&2
  exit 1
}

# Fail loud on a malformed row rather than quietly inventorying zero tests for
# whichever crate lost its executable path.
awk -F"$TAB" 'NF != 4 { print "row " NR ": " NF " fields: " $0; bad = 1 } END { exit bad }' \
  "$WORK/binaries.tsv" || {
  echo "capture-baseline: cargo's artifact JSON did not yield 4 fields per binary" >&2
  exit 1
}
while IFS="$TAB" read -r kind target package exe; do
  [ -x "$exe" ] || {
    echo "capture-baseline: test binary for $kind $target [$package] is not executable: [$exe]" >&2
    exit 1
  }
done <"$WORK/binaries.tsv"

say "$BINARY_COUNT test binaries"

# ---------------------------------------------------------------------------
# test-inventory.txt — every test name, per binary
# ---------------------------------------------------------------------------

say "capturing test inventory"

{
  header "storyhook test inventory"
  printf 'Every test NAME the workspace defines, grouped by the binary that owns it.\n'
  printf 'Names rather than counts, on purpose: a test that silently stops existing\n'
  printf 'shows up here as a removed line instead of as a number nobody checks.\n\n'
  printf 'Sources: `<test-binary> --list` for the Rust tiers (the same binaries\n'
  printf '`cargo test --workspace` runs), `cargo test --workspace --doc -- --list` for\n'
  printf 'doctests, and the file list for the bash suite, whose runner\n'
  printf '(`plugins/story/tests/run-tests.sh`) reports pass/fail per FILE and has\n'
  printf 'no finer unit to name.\n\n'
  printf -- '---\n\n'
} >"$WORK/inventory.txt"

: >"$WORK/all-tests.tsv"   # binary <TAB> test name — the collision check reads this
: >"$WORK/ignored.tsv"     # binary <TAB> test name — cross-checks known-red.md
RUST_TEST_TOTAL=0
RUST_IGNORED_TOTAL=0

while IFS="$TAB" read -r kind target package exe; do
  "$exe" --list 2>/dev/null | grep ': test$' | sed 's/: test$//' |
    LC_ALL=C sort >"$WORK/names.txt" || true
  "$exe" --list --ignored 2>/dev/null | grep ': test$' | sed 's/: test$//' |
    LC_ALL=C sort >"$WORK/ignored-names.txt" || true

  count="$(wc -l <"$WORK/names.txt" | tr -d ' ')"
  ignored="$(wc -l <"$WORK/ignored-names.txt" | tr -d ' ')"
  RUST_TEST_TOTAL=$((RUST_TEST_TOTAL + count))
  RUST_IGNORED_TOTAL=$((RUST_IGNORED_TOTAL + ignored))

  {
    printf '## %s %s [%s] — %s tests' "$kind" "$target" "$package" "$count"
    if [ "$ignored" -gt 0 ]; then printf ' (%s ignored)' "$ignored"; fi
    printf '\n\n'
    if [ "$count" -eq 0 ]; then
      printf '(none)\n\n'
    else
      cat "$WORK/names.txt"
      printf '\n'
    fi
  } >>"$WORK/inventory.txt"

  awk -v b="$target" '{ print b "\t" $0 }' "$WORK/names.txt" >>"$WORK/all-tests.tsv"
  awk -v b="$target" '{ print b "\t" $0 }' "$WORK/ignored-names.txt" >>"$WORK/ignored.tsv"
done <"$WORK/binaries.tsv"

cargo test --workspace --doc -- --list 2>/dev/null |
  grep ': test$' | sed 's/: test$//' | LC_ALL=C sort >"$WORK/doctests.txt" || true
DOCTEST_COUNT="$(wc -l <"$WORK/doctests.txt" | tr -d ' ')"

find plugins/story/tests -name 'test-*.sh' -type f |
  sed 's|.*/||' | LC_ALL=C sort >"$WORK/bash-tests.txt"
BASH_TEST_COUNT="$(wc -l <"$WORK/bash-tests.txt" | tr -d ' ')"

# A bare test name is the only handle the flake census has on an individual
# test — cargo's `test NAME ... ok` lines never name the binary — so a name used
# by two binaries would make the census ambiguous. Nothing collides today; this
# exists to say so out loud the day something does.
cut -f2 "$WORK/all-tests.tsv" | LC_ALL=C sort | uniq -d >"$WORK/dupes.txt"
DUPE_COUNT="$(wc -l <"$WORK/dupes.txt" | tr -d ' ')"

{
  printf '## Doctests — %s\n\n' "$DOCTEST_COUNT"
  if [ "$DOCTEST_COUNT" -eq 0 ]; then
    printf '(none)\n\n'
  else
    cat "$WORK/doctests.txt"
    printf '\n'
  fi

  printf '## Bash suite (plugins/story/tests) — %s files\n\n' "$BASH_TEST_COUNT"
  cat "$WORK/bash-tests.txt"
  printf '\n'

  printf '## Totals\n\n'
  printf 'rust test binaries      %s\n' "$BINARY_COUNT"
  printf 'rust tests              %s (of which ignored: %s — see known-red.md)\n' \
    "$RUST_TEST_TOTAL" "$RUST_IGNORED_TOTAL"
  printf 'doctests                %s\n' "$DOCTEST_COUNT"
  printf 'bash test files         %s\n' "$BASH_TEST_COUNT"
  printf 'duplicate test names    %s' "$DUPE_COUNT"
  if [ "$DUPE_COUNT" -eq 0 ]; then
    printf '\n\n'
    printf 'Every bare test name is unique workspace-wide, which is what lets\n'
    printf 'flake-census.md attribute a bare `test NAME ... ok` line to one test.\n'
  else
    printf ' — AMBIGUOUS in the flake census:\n\n'
    sed 's/^/  /' "$WORK/dupes.txt"
  fi
} >>"$WORK/inventory.txt"

cp "$WORK/inventory.txt" "$OUT/test-inventory.txt"

# ---------------------------------------------------------------------------
# known-red.md — the deliberately-ignored tests
# ---------------------------------------------------------------------------

say "capturing known-red list"

# `--list --ignored` knows WHICH tests are ignored; only the source knows WHY.
# This pairs them: remember the most recent `#[ignore ...]` attribute and attach
# it to the next `fn` that follows.
find tests crates src -name '*.rs' -type f 2>/dev/null | LC_ALL=C sort |
  while read -r file; do
    awk -v file="$file" '
      /^[[:space:]]*#\[ignore/ {
        reason = $0
        sub(/^[[:space:]]*#\[ignore[[:space:]]*=?[[:space:]]*"?/, "", reason)
        sub(/"?[[:space:]]*\][[:space:]]*$/, "", reason)
        if (reason == "") { reason = "(no reason given)" }
        pending = reason
        next
      }
      pending != "" && /^[[:space:]]*(async[[:space:]]+)?fn[[:space:]]+/ {
        name = $0
        sub(/^[[:space:]]*(async[[:space:]]+)?fn[[:space:]]+/, "", name)
        sub(/[(<].*$/, "", name)
        print name "\t" file "\t" FNR "\t" pending
        pending = ""
      }' "$file"
  done | LC_ALL=C sort >"$WORK/ignore-reasons.tsv"

SOURCE_IGNORED="$(wc -l <"$WORK/ignore-reasons.tsv" | tr -d ' ')"
if [ "$SOURCE_IGNORED" -ne "$RUST_IGNORED_TOTAL" ]; then
  echo "capture-baseline: found $SOURCE_IGNORED \`#[ignore]\` attributes in the source" >&2
  echo "  but the harness reports $RUST_IGNORED_TOTAL ignored tests — the scraper is" >&2
  echo "  wrong, or an #[ignore] sits behind a cfg. Fix it rather than shipping a" >&2
  echo "  known-red list that is missing a row." >&2
  exit 1
fi

{
  header "storyhook known-red tests"
  printf 'Tests that exist, are committed, and do **not** run under `make test`.\n\n'
  printf 'Every one is a deliberate debt with a named criterion for un-ignoring it.\n'
  printf 'The count below is asserted against `<binary> --list --ignored` at capture\n'
  printf 'time, so an `#[ignore]` added without a reason — or added at all — cannot\n'
  printf 'slip into the tree unnoticed.\n\n'
  printf 'Ignored: **%s** of %s Rust tests.\n\n' "$RUST_IGNORED_TOTAL" "$RUST_TEST_TOTAL"

  if [ "$RUST_IGNORED_TOTAL" -eq 0 ]; then
    printf 'None.\n\n'
  else
    printf '| test | binary | source | reason / un-ignore criterion |\n'
    printf '|---|---|---|---|\n'
    while IFS="$TAB" read -r name file line reason; do
      binary="$(awk -F"$TAB" -v n="$name" '$2 == n { print $1 }' "$WORK/all-tests.tsv" | head -1)"
      if [ -z "$binary" ]; then binary='(not a harness test)'; fi
      printf '| `%s` | `%s` | `%s:%s` | %s |\n' "$name" "$binary" "$file" "$line" "$reason"
    done <"$WORK/ignore-reasons.tsv"
    printf '\n'
  fi

  printf '## Running them anyway\n\n'
  printf '```sh\n'
  printf 'cargo test --workspace -- --ignored                        # every ignored test\n'
  printf 'cargo test --workspace --test worktree_truth -- --ignored  # the SH-46 pair\n'
  printf '```\n\n'
  printf 'The `worktree_truth` pair is **expected to fail** until W4, the flip: two\n'
  printf 'checkouts of one repository are two separate databases today, so they mint\n'
  printf 'colliding ids and cannot see each other'"'"'s stories. Its captured red output is\n'
  printf 'in `docs/rearch/STATE.md` (W0.3 step log), and removing those two `#[ignore]`\n'
  printf 'attributes is W4'"'"'s exit criterion — not a cleanup, the criterion itself.\n'
} >"$OUT/known-red.md"

# ---------------------------------------------------------------------------
# error-codes.md — the pinned error contract
# ---------------------------------------------------------------------------

say "capturing error contract"

CONTRACT_SRC="tests/error_contract.rs"

# The authoritative variant -> exit code table is the array in
# `unreachable_variants_still_hold_their_exit_codes`, exhaustive over `AppError`
# by construction: `variant_name`'s match refuses to compile if a variant is
# added without a decision about it.
sed -n "s/^ *(AppError::\([A-Za-z]*\)(.*), *\([0-9][0-9]*\)),\$/\1${TAB}\2/p" \
  "$CONTRACT_SRC" >"$WORK/exit-codes.tsv"

# The provoked rows. `variant`, `exit_code` and `message` appear in that order in
# every `Case` literal, so three consecutive field lines are one row. Matching
# on the LITERAL (`variant: "`, `exit_code: <digit>`) and not just the field name
# is what skips the `struct Case` declaration, whose fields look identical.
{ grep -E '^ +(variant: "|exit_code: [0-9]|message: ")' "$CONTRACT_SRC" || true; } |
  sed -E 's/^ +(variant|exit_code|message): //; s/,$//' |
  awk 'NR % 3 == 1 { v = $0 } NR % 3 == 2 { c = $0 } NR % 3 == 0 { print v "\t" c "\t" $0 }' |
  sed 's/"//g' >"$WORK/provoked.tsv"

VARIANT_COUNT="$(wc -l <"$WORK/exit-codes.tsv" | tr -d ' ')"
PROVOKED_COUNT="$(wc -l <"$WORK/provoked.tsv" | tr -d ' ')"
if [ "$VARIANT_COUNT" -lt 2 ] || [ "$PROVOKED_COUNT" -lt 2 ] ||
  [ "$PROVOKED_COUNT" -gt "$VARIANT_COUNT" ]; then
  echo "capture-baseline: extracted $VARIANT_COUNT variants and $PROVOKED_COUNT provoked" >&2
  echo "  rows from $CONTRACT_SRC — the file's shape changed; fix the extractor." >&2
  exit 1
fi

{
  header "storyhook error contract (baseline)"
  printf 'The machine-readable half of this CLI'"'"'s interface, extracted from\n'
  printf '`%s` — the enforcing test itself, not a copy of one. Exit codes\n' "$CONTRACT_SRC"
  printf 'are load-bearing: `plugins/story/bin/story.sh` branches on exit 9 to\n'
  printf 'detect a lost compare-and-swap claim, and exit 3 is how any caller learns an\n'
  printf 'id does not exist.\n\n'

  printf '## Stream placement (the SH-59 ruling)\n\n'
  printf '| form | stream | body |\n'
  printf '|---|---|---|\n'
  printf '| plain | **stderr**; stdout empty | `error: {message}` + newline |\n'
  printf '| `--json` | **stdout**; stderr empty | the envelope below |\n\n'
  printf 'stdout is the machine-readable result channel — exactly one self-describing\n'
  printf 'document per run — so a `--json` failure envelope belongs there, and a failed\n'
  printf 'plain run writes nothing to stdout at all. stderr also carries free-text hook\n'
  printf 'warnings, which is why it cannot be the JSON channel.\n\n'

  printf '## The table\n\n'
  printf '| variant | exit | stream (plain / `--json`) | envelope keys | message fragment | reachable via CLI |\n'
  printf '|---|---|---|---|---|---|\n'
  while IFS="$TAB" read -r variant code; do
    row="$(awk -F"$TAB" -v v="$variant" '$1 == v { print $3 }' "$WORK/provoked.tsv")"
    if [ "$variant" = "StateConflict" ]; then
      keys='`actual`, `error`, `exit_code`, `expected`, `result`'
    else
      keys='`error`, `exit_code`, `result`'
    fi
    if [ -n "$row" ]; then
      printf '| `%s` | %s | stderr / stdout | %s | %s | yes |\n' \
        "$variant" "$code" "$keys" "$(code_span "$row")"
    else
      printf '| `%s` | %s | stderr / stdout | %s | — | **no — dead variant** |\n' \
        "$variant" "$code" "$keys"
    fi
  done <"$WORK/exit-codes.tsv"

  printf '\n### Notes\n\n'
  printf -- '- **`StateConflict` is the one envelope exception.** Its `result` is\n'
  printf -- '  `"conflict"`, not `"error"`, and it carries `expected` + `actual`: a lost\n'
  printf -- '  compare-and-swap is a *result a caller acts on*, not a failure it reports.\n'
  printf -- '  `story.sh`'"'"'s CAS claim reads `actual` to report who won the story.\n'
  printf -- '- **Exit codes 8 and 10 are permanently unallocated.** They named\n'
  printf -- '  `SyncConflict`/`SyncErrors`, both retired with the story↔GitHub-Issues\n'
  printf -- '  sync engine that constructed them (SH-408); neither variant exists any\n'
  printf -- '  more, and the codes stay unallocated rather than being renumbered — see\n'
  printf -- '  `AppError::exit_code`'"'"'s own doc comment.\n'
  printf -- '- **`GithubAuth` (exit 6) alone is feature-gated**, behind the default-on\n'
  printf -- '  `github-pr` feature — it is constructed only by `pr-check`. `--no-\n'
  printf -- '  default-features` compiles it out, and the contract test skips its row\n'
  printf -- '  accordingly; a capture taken without the feature would show it as\n'
  printf -- '  unreachable. `GithubApi` (exit 7) is unconditional: `story update`\n'
  printf -- '  constructs it in every build, so its row is never skipped.\n'
  printf -- '- **The message fragments are fragments on purpose.** This table pins the\n'
  printf -- '  *contract*. Exact prose is pinned once, in the golden corpus\n'
  printf -- '  (`tests/snapshots/`), so a wording change fails one test and not two.\n'
  printf -- '- **The envelope key set is itself the contract.** A new key is a compatible\n'
  printf -- '  addition only once every consumer tolerates it, so the test asserts the\n'
  printf -- '  exact key list, sorted.\n'
  printf -- '- **`LockTimeout` costs ~5s to provoke** and is the single largest fixed cost\n'
  printf -- '  in the gate; see `timings.md`.\n'
} >"$OUT/error-codes.md"

# ---------------------------------------------------------------------------
# Legacy fixtures — the frozen .storyhook tree, its export, its archive schema
# ---------------------------------------------------------------------------

# This whole section is HISTORICAL and no longer runs in this repository: the W7
# cutover migrated storyhook's own tracker into the store and deleted the
# directory. `docs/rearch/baseline/legacy-tree.tar.gz` — captured while it still
# existed — is the permanent artifact, and `tests/migrate_round_trip.rs` is what
# keeps reading it. It survives so the script still documents how that fixture
# was made, and so it can be pointed at some other repository's legacy tree with
# `REPO_ROOT=` if one ever needs freezing.
#
# Skipped rather than fatal, and the difference matters at a wave boundary: this
# script's *other* job is the timings and the test inventory, which are exactly
# what a later wave needs to diff against the baseline. Aborting here because a
# directory the program deliberately retired is missing would make the tool
# unusable for the comparison it was built to enable — and W8 is the wave that
# needed it. `project.toml` rather than the directory itself is the test, because
# a pre-flip `story` on `PATH` still creates a bare `.storyhook/lock` beside any
# repository it runs in.
TREE_FILE_COUNT=0
TREE_BYTES=0
if [ -f "$REPO_ROOT/.storyhook/project.toml" ]; then
say "freezing the legacy .storyhook tree"

# Runtime files are excluded exactly as .storyhook/.gitignore excludes them: a
# lock file and SQLite's sidecars are not project data, and a stray `lock` in an
# import fixture is noise a W3 test would have to special-case.
find .storyhook -type f |
  grep -v -e '/lock$' -e '\.db-wal$' -e '\.db-shm$' -e '/\.DS_Store$' |
  LC_ALL=C sort >"$WORK/tree-files.txt"

# COPYFILE_DISABLE stops bsdtar smuggling AppleDouble `._*` members in; `gzip -n`
# drops the name/mtime header so an unchanged tree yields unchanged bytes and a
# re-capture is a no-op diff rather than a new 100KB blob in git.
COPYFILE_DISABLE=1 tar -cf - -T "$WORK/tree-files.txt" |
  gzip -9 -n >"$OUT/legacy-tree.tar.gz"

TREE_FILE_COUNT="$(wc -l <"$WORK/tree-files.txt" | tr -d ' ')"
TREE_BYTES="$(while read -r f; do wc -c <"$f"; done <"$WORK/tree-files.txt" |
  awk '{ s += $1 } END { print s + 0 }')"

{
  header "storyhook legacy .storyhook/ tree — contents of legacy-tree.tar.gz"
  printf 'A tarball is opaque to review and to `git diff`, so its contents are listed\n'
  printf 'here: a changed fixture shows up as a changed digest even though the blob\n'
  printf 'beside it is unreadable.\n\n'
  printf 'This is the richest legacy fixture in existence — the tracker storyhook uses\n'
  printf 'to track itself, with real event logs, an archive database, custom states,\n'
  printf 'custom types and members, accumulated over the project'"'"'s whole life. It keeps\n'
  printf 'changing, which is why it is frozen here rather than read live. **W3'"'"'s\n'
  printf 'importer is measured against this tree and `golden-export.json` beside it.**\n\n'
  printf 'Runtime files (`lock`, `*.db-wal`, `*.db-shm`) are excluded, matching\n'
  printf '`.storyhook/.gitignore`.\n\n'
  printf '| bytes | sha256 | path |\n'
  printf '|---|---|---|\n'
  while read -r f; do
    printf '| %s | `%s` | `%s` |\n' \
      "$(wc -c <"$f" | tr -d ' ')" "$(shasum -a 256 "$f" | cut -d' ' -f1)" "$f"
  done <"$WORK/tree-files.txt"
  printf '\n%s files, %s bytes uncompressed.\n\n' "$TREE_FILE_COUNT" "$TREE_BYTES"
  printf '## Restoring it\n\n'
  printf '```sh\n'
  printf 'mkdir /tmp/legacy && tar -xzf docs/rearch/baseline/legacy-tree.tar.gz -C /tmp/legacy\n'
  printf '(cd /tmp/legacy && story list)\n'
  printf '```\n'
} >"$OUT/legacy-tree.txt"

say "capturing the golden export"

# `story export` writes the import-consumable document to stdout. `story export
# --json` wraps that same document as an escaped STRING inside the envelope's
# `message` field, which is neither diffable nor feedable to `story
# import-project`. The plain form is what
# `tests/story_export.rs::export_import_export_is_byte_identical` round-trips,
# so the plain form is what gets frozen here.
"$STORY_BIN" export >"$OUT/golden-export.json"
jq -e 'type == "object" and has("schema")' "$OUT/golden-export.json" >/dev/null || {
  echo "capture-baseline: \`story export\` did not produce an export document" >&2
  exit 1
}

say "capturing the archive schema"

ARCHIVE_DB="$REPO_ROOT/.storyhook/archive/archive.db"
{
  printf -- '-- storyhook legacy archive database schema\n'
  printf -- '--\n'
  printf -- '-- Generated by scripts/capture-baseline.sh — do not hand-edit.\n'
  printf -- '-- Captured: %s\n' "$PROVENANCE"
  printf -- '--\n'
  printf -- '-- THIS IS SCHEMA VERSION 0.\n'
  printf -- '--\n'
  printf -- '-- `PRAGMA user_version` is 0 because nothing has ever set it: the legacy\n'
  printf -- '-- archive is created by CREATE TABLE IF NOT EXISTS with no migration\n'
  printf -- '-- framework behind it, and the one column added since (`deleted_reason`)\n'
  printf -- '-- arrived via an unversioned ALTER. W1 introduces real migrations, and\n'
  printf -- '-- every archive database already on every developer machine will present\n'
  printf -- '-- itself as user_version 0 — so 0 must be defined to mean exactly the\n'
  printf -- '-- schema below, including that late-added column.\n'
  printf -- '--\n'
  if [ -f "$ARCHIVE_DB" ]; then
    printf -- '-- PRAGMA user_version   = %s\n' "$(sqlite3 "$ARCHIVE_DB" 'PRAGMA user_version;')"
    printf -- '-- PRAGMA schema_version = %s   (sqlite internal; bumps on every DDL)\n' \
      "$(sqlite3 "$ARCHIVE_DB" 'PRAGMA schema_version;')"
    printf -- '-- PRAGMA journal_mode   = %s\n' "$(sqlite3 "$ARCHIVE_DB" 'PRAGMA journal_mode;')"
    printf -- '-- capture tool: sqlite3 %s\n\n' "$(sqlite3 --version | cut -d' ' -f1)"
    sqlite3 "$ARCHIVE_DB" '.schema'
    printf -- '\n-- Row counts at capture time:\n'
    sqlite3 "$ARCHIVE_DB" \
      "SELECT 'SELECT ''--   ' || name || ' = '' || COUNT(*) || '' rows'' FROM ' || quote(name) || ';'
         FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite@_%' ESCAPE '@'
        ORDER BY name;" |
      sqlite3 "$ARCHIVE_DB"
  else
    printf -- '-- NO ARCHIVE DATABASE PRESENT at capture time (%s).\n' "$ARCHIVE_DB"
    printf -- '-- It is created lazily, by the first `story move <id> <a-DONE-state>`.\n'
  fi
} >"$OUT/archive-schema.sql"
else
  say "no legacy .storyhook/ tree here — leaving the frozen fixtures alone"
  echo "  This repository was migrated in W7. legacy-tree.tar.gz, legacy-tree.txt," >&2
  echo "  golden-export.json and archive-schema.sql in docs/rearch/baseline/ are" >&2
  echo "  permanent historical artifacts and are neither regenerated nor removed." >&2
fi

# ---------------------------------------------------------------------------
# Timings — per test binary
# ---------------------------------------------------------------------------

say "timing $BINARY_COUNT test binaries x $TIMING_RUNS runs"

: >"$WORK/timings.tsv"
while IFS="$TAB" read -r kind target package exe; do
  samples=""
  run=1
  while [ "$run" -le "$TIMING_RUNS" ]; do
    run_timed "$WORK/bin.log" "$exe" || {
      echo "capture-baseline: $target failed during the timing pass:" >&2
      tail -40 "$WORK/bin.log" >&2
      exit 1
    }
    samples="$samples${samples:+,}$REAL_SECONDS"
    run=$((run + 1))
  done
  count="$(awk -F"$TAB" -v b="$target" '$1 == b' "$WORK/all-tests.tsv" | wc -l | tr -d ' ')"
  med="$(printf '%s\n' "$samples" | tr ',' '\n' | median)"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$target" "$kind" "$package" "$count" "$med" "$samples" \
    >>"$WORK/timings.tsv"
done <"$WORK/binaries.tsv"

# A test binary that leaks a server poisons every later run (SH-51), and the
# timing pass just ran web_test outside the Makefile's own bracket. Catch it
# here rather than letting census run 1 fail for a reason that looks like a flake.
bash scripts/check-no-orphan-servers.sh check "baseline timing pass"

# ---------------------------------------------------------------------------
# Flake census — N consecutive `make test` runs
# ---------------------------------------------------------------------------

say "flake census: $CENSUS_RUNS consecutive \`make test\` runs"

# A previous capture's red-run logs must not survive into a green capture.
find "$OUT" -maxdepth 1 -name 'flake-census-run-*.log' -delete

mkdir -p "$WORK/census"
: >"$WORK/census/runs.tsv"     # run, verdict, wall, rust p/f/i, bash p/f
: >"$WORK/census/observed.tsv" # run <TAB> test name <TAB> outcome
CENSUS_GREEN=0
CENSUS_RED=0

run=1
while [ "$run" -le "$CENSUS_RUNS" ]; do
  padded="$(printf '%02d' "$run")"
  log="$WORK/census/run-$padded.log"
  status=0
  run_timed "$log" make test || status=$?
  wall="$REAL_SECONDS"

  # `test result: ok. N passed; M failed; K ignored; …` — one line per binary.
  counts="$(awk '
    /^test result:/ {
      for (i = 1; i <= NF; i++) {
        if ($(i + 1) ~ /^passed/)  { p += $i }
        if ($(i + 1) ~ /^failed/)  { f += $i }
        if ($(i + 1) ~ /^ignored/) { g += $i }
      }
    }
    END { printf "%d\t%d\t%d", p + 0, f + 0, g + 0 }' "$log")"

  # `passed: N  failed: M` — the bash runner's one-line summary.
  bash_counts="$(awk '
    /^passed: [0-9]+  failed: [0-9]+$/ { p = $2; f = $4 }
    END { printf "%d\t%d", p + 0, f + 0 }' "$log")"

  if [ "$status" -eq 0 ]; then
    CENSUS_GREEN=$((CENSUS_GREEN + 1))
    verdict="green"
  else
    CENSUS_RED=$((CENSUS_RED + 1))
    verdict="RED"
    # A failing run's full log is the evidence; it is committed beside the census.
    cp "$log" "$OUT/flake-census-run-$padded.log"
  fi

  printf '%s\t%s\t%s\t%s\t%s\n' "$run" "$verdict" "$wall" "$counts" "$bash_counts" \
    >>"$WORK/census/runs.tsv"

  # Per-test outcomes. cargo prints `test NAME ... ok|FAILED` for every test it
  # runs (and `... ignored, <reason>` for the ones it does not — skipped here,
  # since known-red.md is where those belong). The name is taken as everything
  # between `test ` and ` ... ` so that doctest names, which contain spaces,
  # survive intact. The bash runner prints `  test-foo.sh   PASS|FAIL`.
  awk -v r="$run" '
    /^test .* \.\.\. / {
      cut = index($0, " ... ")
      if (cut == 0) { next }
      name = substr($0, 6, cut - 6)
      rest = substr($0, cut + 5)
      if (rest ~ /^ignored/) { next }
      print r "\t" name "\t" (rest == "ok" ? "ok" : rest)
    }' "$log" >>"$WORK/census/observed.tsv"
  awk -v r="$run" '
    /^ +test-[A-Za-z0-9-]+\.sh +(PASS|FAIL)$/ {
      print r "\t" $1 "\t" ($2 == "PASS" ? "ok" : "FAILED")
    }' "$log" >>"$WORK/census/observed.tsv"

  say "  run $run/$CENSUS_RUNS: $verdict in ${wall}s"
  run=$((run + 1))
done

# The census's actual product: every test that was not `ok` in every single run,
# whether it failed, or vanished from a run entirely.
if [ "$CENSUS_RUNS" -gt 0 ]; then
  awk -F"$TAB" -v runs="$CENSUS_RUNS" '
    { seen[$2]++; if ($3 == "ok") { ok[$2]++ } else { bad[$2] = bad[$2] " run" $1 ":" $3 } }
    END {
      for (t in seen) {
        if (ok[t] != runs) {
          printf "%s\t%d\t%d\t%s\n", t, ok[t] + 0, seen[t],
            (bad[t] == "" ? "(absent from some runs)" : bad[t])
        }
      }
    }' "$WORK/census/observed.tsv" | LC_ALL=C sort >"$WORK/census/not-clean.tsv"
else
  : >"$WORK/census/not-clean.tsv"
fi
NOT_CLEAN="$(wc -l <"$WORK/census/not-clean.tsv" | tr -d ' ')"

{
  header "storyhook flake census"
  if [ "$CENSUS_RUNS" -eq 0 ]; then
    printf '**Skipped** (`--skip-census`). This capture carries no census data.\n'
  else
    printf '%s consecutive `make test` runs, back to back, on an otherwise idle\n' "$CENSUS_RUNS"
    printf 'machine. Three runs cannot see a 1-in-5 flake, and this suite has already\n'
    printf 'had one: SH-51, orphaned test servers holding ports, 78 of 139 tests down\n'
    printf 'and nothing in the output pointing at the cause. The census is what makes\n'
    printf '"green" a measurement instead of an anecdote.\n\n'

    printf '## Verdict\n\n'
    printf '**%s/%s green.**' "$CENSUS_GREEN" "$CENSUS_RUNS"
    if [ "$NOT_CLEAN" -eq 0 ]; then
      printf ' No test was anything other than `ok` in any of the %s runs.\n\n' "$CENSUS_RUNS"
    else
      printf ' **%s test(s) were not clean in all %s runs**, named below.\n\n' \
        "$NOT_CLEAN" "$CENSUS_RUNS"
    fi

    printf '## Runs\n\n'
    printf '| run | verdict | wall (s) | rust passed | rust failed | rust ignored | bash passed | bash failed |\n'
    printf '|---|---|---|---|---|---|---|---|\n'
    while IFS="$TAB" read -r r verdict wall rp rf ri bp bf; do
      printf '| %s | %s | %s | %s | %s | %s | %s | %s |\n' \
        "$r" "$verdict" "$wall" "$rp" "$rf" "$ri" "$bp" "$bf"
    done <"$WORK/census/runs.tsv"
    printf '\n'
    printf 'Wall-time median **%ss** (min %ss, max %ss).\n\n' \
      "$(cut -f3 "$WORK/census/runs.tsv" | median)" \
      "$(cut -f3 "$WORK/census/runs.tsv" | sort -n | head -1)" \
      "$(cut -f3 "$WORK/census/runs.tsv" | sort -n | tail -1)"
    printf 'A run whose pass/fail/ignored counts differ from its neighbours is itself a\n'
    printf 'flake signal even when the run is green — the columns exist to make that\n'
    printf 'visible without reading %s logs.\n\n' "$CENSUS_RUNS"

    printf '## Tests that were not %s/%s `ok`\n\n' "$CENSUS_RUNS" "$CENSUS_RUNS"
    if [ "$NOT_CLEAN" -eq 0 ]; then
      printf 'None.\n\n'
    else
      printf '| test | ok runs | observed runs | detail |\n'
      printf '|---|---|---|---|\n'
      while IFS="$TAB" read -r name okc seen detail; do
        printf '| `%s` | %s | %s | %s |\n' "$name" "$okc" "$seen" "$detail"
      done <"$WORK/census/not-clean.tsv"
      printf '\nEvery RED run'"'"'s full log is committed beside this file as\n'
      printf '`flake-census-run-NN.log`.\n\n'
    fi

    printf '## Method, and what it cannot see\n\n'
    printf -- '- Attribution is by bare test name: cargo prints `test NAME ... ok` without\n'
    printf -- '  naming the binary. `test-inventory.txt` asserts every bare name is unique\n'
    printf -- '  workspace-wide, which is what makes that sound.\n'
    printf -- '- The %s `#[ignore]`d test(s) never run and are absent from the table above\n' "$RUST_IGNORED_TOTAL"
    printf -- '  by construction; `known-red.md` is their register.\n'
    printf -- '- Bash suite tests are counted per FILE (`test-foo.sh`) — the only unit its\n'
    printf -- '  runner reports.\n'
    printf -- '- **Known nondeterminism this census would NOT catch:** `story next`,\n'
    printf -- '  `story summary`, `story context` and `story handoff` order same-priority\n'
    printf -- '  stories by a `created_at` with *second* precision, so identical input can\n'
    printf -- '  yield different orderings (`docs/rearch/STATE.md`, W0.3 finding #1). The\n'
    printf -- '  golden corpus works around it with a deliberate `>=1s` sleep rather than\n'
    printf -- '  fixing it. If an ordering flake ever appears in the table above, it is\n'
    printf -- '  that defect resurfacing — not a new one.\n'
  fi
} >"$OUT/flake-census.md"

# ---------------------------------------------------------------------------
# timings.json / timings.md
# ---------------------------------------------------------------------------

say "writing timings"

TOTAL_MEDIAN=""
TOTAL_SAMPLES="[]"
if [ "$CENSUS_RUNS" -gt 0 ]; then
  TOTAL_MEDIAN="$(cut -f3 "$WORK/census/runs.tsv" | median)"
  TOTAL_SAMPLES="$(cut -f3 "$WORK/census/runs.tsv" |
    jq -R -s 'split("\n") | map(select(length > 0) | tonumber)')"
fi
BINARY_SUM="$(cut -f5 "$WORK/timings.tsv" | awk '{ s += $1 } END { printf "%.3f", s + 0 }')"

jq -n \
  --arg captured_at "$CAPTURED_AT" \
  --arg host "$HOST" \
  --arg commit "$COMMIT" \
  --arg branch "$BRANCH" \
  --arg tree_state "$TREE_STATE" \
  --arg rustc "$RUSTC" \
  --arg cargo "$CARGO" \
  --arg os "$OS" \
  --arg arch "$ARCH" \
  --arg cpu "$CPU" \
  --argjson cores "$CORES" \
  --argjson timing_runs "$TIMING_RUNS" \
  --argjson census_runs "$CENSUS_RUNS" \
  --arg total_median "${TOTAL_MEDIAN:-}" \
  --argjson total_samples "$TOTAL_SAMPLES" \
  --argjson binary_sum "$BINARY_SUM" \
  --rawfile rows "$WORK/timings.tsv" \
  '{
     machine: {
       captured_at: $captured_at, host: $host, commit: $commit, branch: $branch,
       tree_state: $tree_state, rustc: $rustc, cargo: $cargo,
       os: $os, arch: $arch, cpu: $cpu, cores: $cores
     },
     method: {
       unit: "seconds of wall clock, /usr/bin/time -p",
       per_binary: "test binaries invoked directly with INSTA_UPDATE=no, \($timing_runs) runs each, median reported",
       whole_gate: "`make test` end to end, sampled from the \($census_runs)-run flake census",
       warm: "cargo build --workspace --tests ran to completion first; no measurement includes a cold compile"
     },
     whole_gate: {
       runs: $census_runs,
       median_s: (if $total_median == "" then null else ($total_median | tonumber) end),
       samples_s: $total_samples
     },
     per_binary_serial_sum_s: $binary_sum,
     binaries: (
       $rows | split("\n") | map(select(length > 0)) | map(split("\t")) | map({
         target: .[0], kind: .[1], package: .[2],
         tests: (.[3] | tonumber),
         median_s: (.[4] | tonumber),
         samples_s: (.[5] | split(",") | map(tonumber))
       }) | sort_by(-.median_s)
     )
   }' >"$OUT/timings.json"

{
  header "storyhook baseline timings"
  printf 'Wall-clock seconds, `/usr/bin/time -p`. Everything below is **warm**: a full\n'
  printf '`cargo build --workspace --tests` completes before the first measurement, so\n'
  printf 'no number here includes a cold compile.\n\n'

  printf '## The whole gate\n\n'
  if [ "$CENSUS_RUNS" -gt 0 ]; then
    printf '`make test` end to end — orphan check, `cargo fmt --check`, clippy with\n'
    printf '`-D warnings`, `cargo test --workspace`, `cargo build`, the bash suite —\n'
    printf 'sampled from the %s-run flake census:\n\n' "$CENSUS_RUNS"
    printf '| runs | median | min | max |\n'
    printf '|---|---|---|---|\n'
    printf '| %s | **%ss** | %ss | %ss |\n\n' "$CENSUS_RUNS" "$TOTAL_MEDIAN" \
      "$(cut -f3 "$WORK/census/runs.tsv" | sort -n | head -1)" \
      "$(cut -f3 "$WORK/census/runs.tsv" | sort -n | tail -1)"
  else
    printf 'Not measured — the census was skipped.\n\n'
  fi

  printf '## Per test binary\n\n'
  printf 'Sampled %s time(s) each, invoked directly — what `cargo test` does, minus\n' "$TIMING_RUNS"
  printf 'cargo'"'"'s own startup. Serial sum **%ss**; the gate beats that because cargo\n' "$BINARY_SUM"
  printf 'overlaps binaries and each binary threads its own tests.\n\n'
  printf '| binary | kind | tests | median (s) | samples (s) |\n'
  printf '|---|---|---|---|---|\n'
  sort -t"$TAB" -k5,5gr "$WORK/timings.tsv" |
    while IFS="$TAB" read -r target kind package count med samples; do
      printf '| `%s` | %s | %s | %s | %s |\n' "$target" "$kind" "$count" "$med" "$samples"
    done
  printf '\n'

  printf '## The two costs that are contracts, not noise\n\n'
  printf 'The top of that table is not a performance problem to be optimised away.\n'
  printf 'Both leaders are deliberate, and each has a named production change that\n'
  printf 'would make it cheap — which is the only thing that should make it cheap.\n\n'
  printf -- '- **`error_contract` is dominated by one row.** `LockTimeout` provokes a\n'
  printf -- '  *real* lock timeout, and `with_project_lock` polls a hard-coded 5s deadline\n'
  printf -- '  (`src/lock.rs`) with no environment override. Both output forms are\n'
  printf -- '  provoked concurrently so the wait is paid once (11.1s → 5.9s) instead of\n'
  printf -- '  twice. **If W5 makes that deadline configurable, this binary drops to ~1s**\n'
  printf -- '  and the gate drops with it. Until then it is the price of pinning exit 4\n'
  printf -- '  at all, and exit 4 is part of the interface.\n'
  printf -- '- **`golden_cli` carries a deliberate `>=1s` sleep.** 177 CLI invocations\n'
  printf -- '  across 27 snapshots, plus one sleep placed so that every same-priority\n'
  printf -- '  ready PAIR straddles it. That sleep is the workaround for the\n'
  printf -- '  nondeterministic ready-list ordering (`docs/rearch/STATE.md`, W0.3 finding\n'
  printf -- '  #1): `created_at` has second precision, so stories created within one\n'
  printf -- '  second tie on both sort keys. **The sleep goes away when the comparator\n'
  printf -- '  gets a total order**, not before.\n'
  printf -- '- **`web_test` runs real servers.** Ports are kernel-assigned and daemons are\n'
  printf -- '  stopped by a guard (SH-51); the cost is genuine I/O against genuine HTTP,\n'
  printf -- '  which is the point of the file.\n\n'
  printf 'Those first two account for most of the gap between the ~29s gate before W0.3\n'
  printf 'and the gate measured above. Neither is a regression.\n\n'

  printf '## Reading a later capture against this one\n\n'
  printf 'Re-run `scripts/capture-baseline.sh` and diff. Absolute seconds are only\n'
  printf 'comparable on the same machine (see the provenance line above); **ratios**\n'
  printf 'are comparable anywhere. A binary whose share of the serial sum jumps without\n'
  printf 'gaining tests is the signal worth chasing.\n'
} >"$OUT/timings.md"

# ---------------------------------------------------------------------------
# manifest.json — machine tag + what was captured
# ---------------------------------------------------------------------------

say "writing manifest"

# README.md is the one file in $OUT this script does not own — it is hand-written
# prose about the directory, and the manifest describes what was *generated*.
find "$OUT" -maxdepth 1 -type f ! -name 'manifest.json' ! -name 'README.md' |
  LC_ALL=C sort |
  while read -r f; do
    printf '%s\t%s\t%s\n' "$(basename "$f")" "$(wc -c <"$f" | tr -d ' ')" \
      "$(shasum -a 256 "$f" | cut -d' ' -f1)"
  done >"$WORK/artifacts.tsv"

jq -n \
  --arg captured_at "$CAPTURED_AT" \
  --arg host "$HOST" \
  --arg commit "$COMMIT" \
  --arg branch "$BRANCH" \
  --arg tree_state "$TREE_STATE" \
  --arg rustc "$RUSTC" \
  --arg cargo "$CARGO" \
  --arg os "$OS" \
  --arg arch "$ARCH" \
  --arg cpu "$CPU" \
  --argjson cores "$CORES" \
  --argjson binaries "$BINARY_COUNT" \
  --argjson rust_tests "$RUST_TEST_TOTAL" \
  --argjson rust_ignored "$RUST_IGNORED_TOTAL" \
  --argjson doctests "$DOCTEST_COUNT" \
  --argjson bash_tests "$BASH_TEST_COUNT" \
  --argjson census_runs "$CENSUS_RUNS" \
  --argjson census_green "$CENSUS_GREEN" \
  --argjson census_red "$CENSUS_RED" \
  --argjson not_clean "$NOT_CLEAN" \
  --argjson timing_runs "$TIMING_RUNS" \
  --argjson tree_files "$TREE_FILE_COUNT" \
  --argjson tree_bytes "$TREE_BYTES" \
  --rawfile artifacts "$WORK/artifacts.tsv" \
  '{
     captured_at: $captured_at,
     generator: "scripts/capture-baseline.sh",
     machine: {
       host: $host, os: $os, arch: $arch, cpu: $cpu, cores: $cores,
       rustc: $rustc, cargo: $cargo
     },
     source: { commit: $commit, branch: $branch, tree_state: $tree_state },
     inventory: {
       test_binaries: $binaries,
       rust_tests: $rust_tests,
       rust_ignored: $rust_ignored,
       doctests: $doctests,
       bash_test_files: $bash_tests
     },
     legacy_tree: { files: $tree_files, uncompressed_bytes: $tree_bytes },
     census: {
       runs: $census_runs, green: $census_green, red: $census_red,
       tests_not_clean_in_every_run: $not_clean
     },
     timing_runs: $timing_runs,
     artifacts: (
       $artifacts | split("\n") | map(select(length > 0)) | map(split("\t")) | map({
         file: .[0], bytes: (.[1] | tonumber), sha256: .[2]
       })
     )
   }' >"$OUT/manifest.json"

say "done — $OUT"
say "  $RUST_TEST_TOTAL rust tests across $BINARY_COUNT binaries ($RUST_IGNORED_TOTAL ignored),"
say "  $DOCTEST_COUNT doctests, $BASH_TEST_COUNT bash test files"
if [ "$CENSUS_RUNS" -gt 0 ]; then
  say "  census: $CENSUS_GREEN/$CENSUS_RUNS green, $NOT_CLEAN test(s) not clean in every run"
fi

# The capture is a report, not a gate — but a red census must not be mistaken
# for a green one by a caller that checks only the exit status.
[ "$CENSUS_RED" -eq 0 ] || exit 1
