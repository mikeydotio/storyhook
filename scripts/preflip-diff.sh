#!/usr/bin/env bash
# Old-versus-new `--json` diff of every command, on one seeded fixture.
#
# The flip's claim is that moving story data into a global SQLite store does not
# change what `story` answers. `tests/golden_cli.rs` proves that at snapshot
# granularity and `make test-store` runs it on both legs — but a snapshot suite
# reports "the corpus moved", not "these three commands moved", and the wave
# that performs the flip owes its reviewer the second form.
#
# So this builds the same fixture twice — once through the legacy stack, once
# through the store — runs the same command list against each, and reports, per
# command, whether the two answers are byte-identical after normalization.
#
# Normalized, and only this:
#   * timestamps      — both legs read the system clock, seconds apart
#   * absolute paths  — the two fixtures live in different directories
#
# Everything else is compared verbatim: ids, ordering, states, priorities,
# labels, relationships, derived relationships, progress, counts, error
# messages, exit codes.
#
# Usage: bash scripts/preflip-diff.sh [--out DIR]
# Exit:  0 whether or not commands diverge — divergence is the *finding*, and
#        the summary names each one. Non-zero only on a harness failure.
set -euo pipefail

OUT="${TMPDIR:-/tmp}/preflip-diff-$$"
while [ $# -gt 0 ]; do
    case "$1" in
        --out) OUT="$2"; shift 2 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STORY="$ROOT/target/debug/story"
[ -x "$STORY" ] || { echo "build first: cargo build" >&2; exit 2; }

# /private/tmp rather than $TMPDIR: Spotlight indexes the latter and this
# creates a few hundred small files (SH-53).
WORK="$(mktemp -d /private/tmp/preflip-XXXXXX)"
mkdir -p "$OUT"
trap 'rm -rf "$WORK"' EXIT

# --- one isolated environment per leg ---------------------------------------
# Each leg gets a private HOME and XDG tree, so neither can see the developer's
# real store or registry, and the store leg's database is created fresh.
leg_env() {
    local leg="$1"
    export HOME="$WORK/$leg/home"
    export XDG_DATA_HOME="$HOME/.local/share"
    export XDG_CONFIG_HOME="$HOME/.config"
    export XDG_STATE_HOME="$HOME/.local/state"
    export STORYHOOK_DATA_DIR="$XDG_DATA_HOME/storyhook"
    export STORYHOOK_INVOKER="$leg"
    export GIT_TERMINAL_PROMPT=0
    mkdir -p "$STORYHOOK_DATA_DIR" "$XDG_CONFIG_HOME" "$XDG_STATE_HOME"
}

# --- the fixture -------------------------------------------------------------
# Fourteen stories, so ids run past SH-9 where lexicographic and numeric
# ordering diverge. Deliberately the same shape as tests/golden_cli.rs's corpus:
# four states including a custom one, every priority, custom types, labels,
# relations, comments, assignees, members, archived and soft-deleted stories,
# an `awaiting` block, and phases.
seed() {
    local dir="$1"
    mkdir -p "$dir"
    cd "$dir"
    "$STORY" init >/dev/null
    "$STORY" member add "Ada Lovelace <ada@example.com>" >/dev/null
    "$STORY" member add -g grace-hopper >/dev/null
    "$STORY" state add review --super OPEN --description "Awaiting code review" >/dev/null
    "$STORY" type add spike --description "A timeboxed investigation" >/dev/null

    "$STORY" new "Design the storage engine" --type epic --priority critical >/dev/null
    "$STORY" new "Define the Store trait" --type story --priority high --label backend --label api >/dev/null
    "$STORY" new "Implement the SQLite engine" --type story --priority high --label backend >/dev/null
    "$STORY" new "Write the migration runner" --type task --priority medium --label backend >/dev/null
    "$STORY" new "Port the lifecycle services" --type story --priority medium >/dev/null
    "$STORY" new "Investigate WAL contention" --type spike --priority low >/dev/null
    "$STORY" new "Fix the doctor's repair loop" --type bug --priority critical --label doctor >/dev/null
    "$STORY" new "Tidy the changelog" --type chore >/dev/null
    "$STORY" new "Document the pointer file" --type task --priority low --label docs >/dev/null
    "$STORY" new "Retire the registry" --type chore --priority medium >/dev/null
    "$STORY" new "Ship the daemon" --type epic --priority high >/dev/null
    "$STORY" new "Measure the gate" --type task --priority none >/dev/null
    "$STORY" new "Delete the legacy leg" --type chore --priority low >/dev/null
    "$STORY" new "Write the postmortem" --type task --priority medium >/dev/null

    "$STORY" relate SH-1 parent-of SH-2 >/dev/null
    "$STORY" relate SH-1 parent-of SH-3 >/dev/null
    "$STORY" relate SH-2 blocks SH-3 >/dev/null
    "$STORY" relate SH-11 parent-of SH-10 >/dev/null
    "$STORY" relate SH-7 relates-to SH-14 >/dev/null

    "$STORY" assign SH-2 ada >/dev/null
    "$STORY" assign SH-3 grace-hopper >/dev/null
    "$STORY" comment SH-2 "Started on the trait definition" >/dev/null
    "$STORY" comment SH-2 "Second pass on the read side" >/dev/null
    "$STORY" comment SH-7 "Reproduced on the archived-story path" >/dev/null
    "$STORY" set SH-4 --description "The runner has to be crash-safe" >/dev/null
    "$STORY" block SH-6 "a decision on the pool size" >/dev/null

    "$STORY" move SH-2 in-progress >/dev/null
    "$STORY" move SH-3 review >/dev/null
    "$STORY" move SH-8 done >/dev/null
    "$STORY" move SH-12 done >/dev/null
    "$STORY" delete SH-13 "folded into SH-14" >/dev/null

    "$STORY" phase create 1 "Foundations" >/dev/null
    "$STORY" phase create 2 "The flip" >/dev/null
    "$STORY" phase add SH-2 1 >/dev/null
    "$STORY" phase add SH-3 1 >/dev/null
    "$STORY" phase add SH-11 2 >/dev/null
    "$STORY" epic create "Cross-cutting cleanup" >/dev/null
    cd - >/dev/null
}

# --- the command list --------------------------------------------------------
# Every read-surface command, in `--json` form, plus the error cases whose
# envelope a script consumes. Written one per line so the summary can name them.
read -r -d '' COMMANDS <<'EOF' || true
list
list --state todo
list --state in-progress
list --priority high
list --priority critical
list --label backend
list --assignee ada
list --flagged
list --blocked
list --ready
list --type epic
list --phase 1
show SH-1
show SH-2
show SH-3
show SH-7
show SH-13
search engine
search the
next
next --count 3
next --phase 1
summary
report
graph
graph --critical-path
graph --parallel-groups
graph --blocked-by SH-2
load-context
load-context --format json
handoff
phase list
phase show 1
epic list
type list
state list
epic show SH-17
export
show SH-404
show nonsense
move SH-404 todo
move SH-1 no-such-state
relate SH-1 no-such-relation SH-2
delete SH-404 gone
comment SH-404 hello
assign SH-1 no-such-member
phase show 99
prioritize SH-404 high
reopen SH-404
EOF

# --- run one leg -------------------------------------------------------------
run_leg() {
    # Two statements, not one `local a=… b=…`: bash 3.2 (what macOS ships)
    # expands every word of a `local` before assigning any of them, so `dir`
    # would be built from an unset `leg`.
    local leg="$1"
    local dir="$WORK/$leg/project"
    if ! ( leg_env "$leg"; seed "$dir" ) >"$OUT/$leg.seed.log" 2>&1; then
        echo "seeding the $leg leg failed; see $OUT/$leg.seed.log" >&2
        tail -5 "$OUT/$leg.seed.log" >&2
        exit 1
    fi
    while IFS= read -r cmd; do
        [ -n "$cmd" ] || continue
        local slug
        slug="$(printf '%s' "$cmd" | tr -c 'A-Za-z0-9' '_')"
        (
            leg_env "$leg"
            cd "$dir"
            # A failing command is data here, not a harness fault: the error
            # cases in the list above are exactly the rows whose exit code and
            # envelope the comparison is about.
            set +e
            # shellcheck disable=SC2086
            "$STORY" $cmd --json >"$OUT/$leg.$slug.out" 2>"$OUT/$leg.$slug.err"
            printf 'exit=%s\n' "$?" >>"$OUT/$leg.$slug.out"
        )
    done <<<"$COMMANDS"
}

# --- normalization -----------------------------------------------------------
# Timestamps and the two fixtures' own paths. Nothing else.
normalize() {
    sed -E \
        -e 's/[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]+)?(Z|[+-][0-9]{2}:[0-9]{2})/<TS>/g' \
        -e "s#$WORK/[a-z]*/project#<PROJECT>#g" \
        -e "s#$WORK/[a-z]*/home#<HOME>#g" \
        "$1"
}

echo "seeding and running both legs…"
run_leg legacy
run_leg local

same=0; differs=0
: >"$OUT/summary.txt"
while IFS= read -r cmd; do
    [ -n "$cmd" ] || continue
    slug="$(printf '%s' "$cmd" | tr -c 'A-Za-z0-9' '_')"
    for stream in out err; do
        normalize "$OUT/legacy.$slug.$stream" >"$OUT/legacy.$slug.$stream.norm"
        normalize "$OUT/local.$slug.$stream" >"$OUT/local.$slug.$stream.norm"
    done
    if diff -q "$OUT/legacy.$slug.out.norm" "$OUT/local.$slug.out.norm" >/dev/null &&
       diff -q "$OUT/legacy.$slug.err.norm" "$OUT/local.$slug.err.norm" >/dev/null; then
        same=$((same + 1))
        printf 'SAME     story %s --json\n' "$cmd" >>"$OUT/summary.txt"
    else
        differs=$((differs + 1))
        printf 'DIFFERS  story %s --json\n' "$cmd" >>"$OUT/summary.txt"
        {
            printf '\n=== story %s --json ===\n' "$cmd"
            diff -u "$OUT/legacy.$slug.out.norm" "$OUT/local.$slug.out.norm" || true
            diff -u "$OUT/legacy.$slug.err.norm" "$OUT/local.$slug.err.norm" || true
        } >>"$OUT/differences.txt"
    fi
done <<<"$COMMANDS"

printf '\n%s identical, %s divergent (of %s commands)\n' \
    "$same" "$differs" "$((same + differs))" | tee -a "$OUT/summary.txt"
echo "artifacts: $OUT"
[ "$differs" -eq 0 ] || echo "differences: $OUT/differences.txt"
