#!/usr/bin/env bash
#
# Fails when a server this repo's test suite starts is still running at the
# START of a run, and reaps one still running at the END of a run (SH-412).
#
# Test-spawned servers that outlive their run are not a curiosity: they hold
# their ports, and a later run that lands on one of those ports talks to a
# stranger's registry, which answers 404 to everything it is asked. That is
# the failure mode of SH-51 -- 78 of 139 tests down, with nothing in the
# output pointing at the real cause. Ports are now kernel-assigned and daemons
# are stopped by a guard, so this should never fire; it exists to name the
# cause immediately if the class ever comes back.
#
# Two classes of process, on two different scoping keys (SH-493).
#
# THIS WORKING TREE's, matched on the binary's path (target/debug/...) and
# handled per phase below. Safe to reason about because the path proves whose
# they are.
#
# ABANDONED ones, matched on what the process IS rather than where its binary
# sits: a daemon whose --store-path names a file that no longer exists is
# serving nobody, whoever started it. Collected in every phase, never refused
# over. That class is checkout-agnostic on purpose -- it is the only way to
# see a daemon running from a copy of the binary somewhere else, which is what
# `tests/plugin_install.rs` makes on purpose and what accumulated to 672
# unnoticed.
#
# The developer's real dashboard daemon -- an installed `story` binary, port
# 3456 -- is out of scope of both: it is production, its store exists, and
# nothing here touches it. This is the fourth layer of the orphan defence,
# behind the test guards' Drop, the daemon's own process group, and the
# STORYHOOK_PARENT_PID suicide contract; it exists to name the cause
# immediately if all three fail.
#
# PREFLIGHT refuses, immediately, on any match of this worktree's class. A
# process already here before this run started makes THIS run lie about what
# it verified, and it may be a daemon the developer started on purpose -- not
# this script's business to stop.
#
# POSTLUDE waits, then REAPS rather than refusing (SH-412). `make test` is
# nine minutes nominal; the postlude is its last-but-one line, so a false
# refusal here throws away a suite that already went green and charges a full
# re-run to prove it again -- precisely the pressure that caused SH-306. And a
# postlude match is not ambiguous the way a preflight one is: the preflight is
# a hard prerequisite of `test` (see the Makefile), so it already proved this
# worktree was clean when the run began. Anything the pattern still matches at
# the postlude was spawned by *this* run, and this run is entitled to collect
# it. SIGTERM first, bounded wait, then SIGKILL, then verify -- and fail only
# if something survives SIGKILL, which is a genuinely different fact (a
# process this run cannot even end, not one it merely forgot to).
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# How long the postlude waits before treating a match as a leak rather than
# the defence still working -- not a bare literal (CLAUDE.md, SH-394).
#
# By the time the postlude runs, every test binary this run started has
# already exited (`make test`'s prior legs are done), so the only sanctioned
# process still capable of holding a daemon open is `watch_parent`
# (src/daemon/serve.rs), which polls its parent every `SHUTDOWN_CHECK` --
# 250ms -- and exits the instant it notices. `DaemonGuard`'s `STOP_DEADLINE`
# (15s) and `lifecycle::stop`'s `FORCE_GRACE` (2s) are both spent INSIDE a
# test binary's own teardown, before that binary exits, so neither is in play
# here. 10s is therefore already 40x the deadline it needs to clear, which is
# why the fix widens what the postlude DOES with a survivor rather than how
# long it waits for one. `tests/orphan_check.rs` pins the 40x relationship
# against `SHUTDOWN_CHECK`'s own source so the two cannot silently drift.
readonly ORPHAN_GRACE_SECS=10

# How long a SIGTERM gets before this escalates to SIGKILL. Generous on
# purpose -- distinguishing "slow to exit" from "ignoring the signal" is the
# entire point of a bounded wait, so it must not be so short that an ordinary
# shutdown reads as an escalation.
readonly ORPHAN_KILL_GRACE_SECS=5

die() {
    printf 'check-no-orphan-servers: %s\n' "$1" >&2
    exit 1
}

note() {
    printf 'check-no-orphan-servers: %s\n' "$1" >&2
}

usage() {
    die "usage: check-no-orphan-servers.sh preflight|postlude|check [label]"
}

phase="${1:-}"
case "$phase" in
preflight | postlude) context="$phase" ;;
check) context="${2:-check}" ;;
*) usage ;;
esac

# `story daemon --serve` is the daemon; `story web --serve` is its alias, which
# a test may still be spelling. Both are matched, and so are the two test
# binaries that run servers in-thread.
#
# `([^ ]+ )*` between the binary and the verb is not decoration: a spawned
# daemon carries `--store-path <file>` ahead of its verb (SH-113), so a pattern
# anchored on `story daemon --serve` stopped matching the very processes this
# guard exists to find -- and a guard that matches nothing passes.
pattern="${repo_root}/target/debug/(deps/(web_test|daemon_lifecycle|daemon_invoke|storyhook_test_support)-|story ([^ ]+ )*(daemon|web) --serve)"

matches() {
    pgrep -f "$pattern" || true
}

# `ps`'s report for a set of pids, best-effort -- a pid that exited between
# the `pgrep` that found it and this call is not an error here, just gone.
report() {
    local heading="$1"
    shift
    note "${heading}:"
    # shellcheck disable=SC2046 # deliberate word splitting: one -p per pid
    ps -o pid,ppid,pgid,state,etime,command -p $(printf '%s' "$*" | tr '\n' ',' | sed 's/,$//') >&2 2>/dev/null || true
}

# Durable evidence, alongside the ephemeral stderr above. stderr from a
# nine-minute suite scrolls away, and the whole reason this class is hard to
# pin down is that a survivor's own portfile and daemon log are ALREADY GONE
# by the time anyone looks -- `scripts/run-tests.sh` deletes its isolated data
# root on EXIT. Lives beside `gate-receipt.sh`'s own receipts: same shared,
# per-clone directory, never committed, never cloned.
durable_report() {
    local common_dir
    # `git rev-parse --git-common-dir` prints a path relative to the CURRENT
    # directory, not to `-C`'s argument -- so this must actually be inside
    # `$repo_root` when it runs, the same way gate-receipt.sh resolves it.
    #
    # `cd ""` is a silent NO-OP in bash, not a failure -- it leaves the shell
    # exactly where it already was and returns success. So a git failure here
    # cannot be trusted to fail the `cd` that consumes its output; it has to
    # be checked for an empty result explicitly at every step, or a caller
    # whose cwd is some OTHER real repo (never this fixture's) would have
    # this land there instead, silently. Measured, not hypothetical: an
    # earlier, less careful version of exactly this line did this to this
    # worktree's own root during manual testing before this script shipped.
    common_dir="$(
        cd "$repo_root" || exit 1
        rel="$(git rev-parse --git-common-dir 2>/dev/null)"
        [ -n "$rel" ] || exit 1
        cd "$rel" || exit 1
        pwd
    )" || return 0
    [ -n "$common_dir" ] || return 0
    local dir="$common_dir/storyhook/orphan-reports"
    mkdir -p "$dir" 2>/dev/null || return 0
    {
        printf '=== %s -- %s ===\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$1"
        shift
        printf '%s\n' "$@"
    } >>"$dir/$(date -u +%Y%m%d).log" 2>/dev/null || true
}

# Ends every process the finder function named by $1 reports, and echoes
# whatever is still alive afterwards. SIGTERM first, a bounded wait, then
# SIGKILL, then verify -- a process that will not die even to SIGKILL is a
# genuinely different fact (a wedged syscall, a respawning supervisor) from
# one that was merely forgotten about, and only that one is worth failing on.
#
# Takes the NAME of a finder rather than a list of pids because the
# verification has to ask the question again rather than re-check a list
# captured before the signal was sent: a pid that has exited is gone from the
# answer, which is exactly the difference being waited for.
reap() {
    local finder="$1" pids remaining term_deadline
    pids="$("$finder")"
    [ -n "$pids" ] || return 0

    # shellcheck disable=SC2086
    kill $pids 2>/dev/null || true

    term_deadline=$((SECONDS + ORPHAN_KILL_GRACE_SECS))
    remaining="$pids"
    while [ "$SECONDS" -lt "$term_deadline" ]; do
        remaining="$("$finder")"
        [ -z "$remaining" ] && break
        sleep 0.25
    done

    if [ -n "$remaining" ]; then
        report "SIGTERM was not enough within ${ORPHAN_KILL_GRACE_SECS}s; escalating to SIGKILL" "$remaining"
        # shellcheck disable=SC2086
        kill -9 $remaining 2>/dev/null || true
        sleep 0.5
        remaining="$("$finder")"
    fi

    printf '%s' "$remaining"
}

# --- the second class: a daemon serving a store that is gone (SH-493) --------
#
# `pattern` above is anchored at THIS checkout, which is what makes a match
# safe to act on -- and is also why it structurally cannot see the population
# SH-493 counted. `tests/plugin_install.rs` copies the binary into its own
# fixture and runs `<fixture>/package/story`, on purpose, to prove path
# resolution from an installed layout; 672 of those were alive on one machine
# across three days, invisible to both ends of this bracket while the four
# siblings that happened to run `target/debug/story` were reaped every run.
#
# Widening the pattern to "any `story` daemon" is not available: `pgrep -f` is
# global, this machine runs three or four concurrent worktree suites, and
# every one of them builds its fixtures under the same shared root. A pattern
# that matched them would refuse this run for a stranger's live suite at the
# preflight, and murder one at the postlude.
#
# So the scoping key is not WHERE the binary sits but WHAT the process is --
# the SH-239 lesson, one layer over. A daemon whose `--store-path` names a
# file that no longer exists is serving nobody, whoever started it: daemon
# runtime state is keyed off the canonical store path (SH-113), so its
# portfile went with the fixture directory, and a client resolving that path
# would find no database and start its own. It cannot be a daemon a developer
# is using, because theirs exists. That makes it collectable by whoever finds
# it, in either phase, with no reference to which worktree spawned it --
# measured on the machine SH-493 was filed from, where it separated 718
# abandoned daemons from exactly two live ones (the developer's own, and a
# concurrent gate run's).
#
# The one shape this must not catch is a daemon that has not opened its store
# YET, which is why the age floor below exists rather than a bare existence
# check.

# How old an abandoned-looking daemon must be before its missing store is
# read as abandonment rather than as a start-up it interrupted. Derived from
# the deadline it disproves (SH-394), never picked: `SPAWN_DEADLINE`
# (src/daemon/lifecycle.rs) is how long a client waits for a daemon it just
# spawned to answer, so a daemon still storeless past it has already been
# given up on by the only process that was waiting. Twice it, for margin on a
# machine running three or four suites at once. `tests/orphan_check.rs` pins
# the relationship against `SPAWN_DEADLINE`'s own source so the two cannot
# silently drift apart.
readonly ABANDONED_STORE_MIN_AGE_SECS=10

# The pids of every `story` daemon, from any binary anywhere, that is old
# enough to have opened its store and has not got one.
#
# One `ps` pass rather than a `ps` per pid: on the machine this was written
# for that is the difference between one process and seven hundred. `awk`
# does the age arithmetic and pulls `--store-path`'s argument out of the
# argv; the existence test stays in the shell, where `-e` means what this
# comment says it means.
#
# `etime` rather than `etimes`: macOS `ps` has no `etimes` keyword at all, so
# the `[[dd-]hh:]mm:ss` form has to be parsed rather than read.
abandoned_candidates() {
    ps -eo pid=,etime=,command= 2>/dev/null | awk -v min_age="$ABANDONED_STORE_MIN_AGE_SECS" '
        $0 ~ /(daemon|web) --serve/ {
            n = split($2, t, /[-:]/)
            if (n == 4)      age = t[1]*86400 + t[2]*3600 + t[3]*60 + t[4]
            else if (n == 3) age = t[1]*3600 + t[2]*60 + t[3]
            else if (n == 2) age = t[1]*60 + t[2]
            else             next
            if (age < min_age) next

            # Delimited by the VERB that follows it, never taken as the next
            # whitespace field. A store path may contain a space -- macOS
            # hands out home directories like `/Users/Ada Lovelace` without
            # comment -- and a field-split read of `--store-path
            # "/Users/Ada Lovelace/.../store.db"` yields `/Users/Ada`, which
            # does not exist, which classifies the DEVELOPER OWN RUNNING
            # DAEMON as abandoned and kills it. The verb is the one thing
            # after the path whose spelling this script already knows.
            head = index($0, "--store-path ")
            if (head == 0) next
            rest = substr($0, head + length("--store-path "))
            cut = index(rest, " daemon --serve")
            alias = index(rest, " web --serve")
            if (alias > 0 && (cut == 0 || alias < cut)) cut = alias
            if (cut == 0) next
            print $1 "\t" substr(rest, 1, cut - 1)
        }'
}

abandoned() {
    local pid store
    while IFS="$(printf '\t')" read -r pid store; do
        [ -n "$pid" ] || continue
        [ -n "$store" ] || continue
        [ -e "$store" ] || printf '%s\n' "$pid"
    done < <(abandoned_candidates)
}

# Collects them, in EVERY phase, and never refuses over them.
#
# Reaping rather than refusing is not the postlude's argument borrowed early
# (SH-412's "the preflight already proved this worktree clean"). It is a
# different and simpler one that holds in both phases at once: this class is
# provably nobody's, so there is no run whose verification it can be making a
# lie, and nothing for a developer to have started on purpose. Refusing would
# block this run over a mess another worktree made -- and over 672 of them at
# a time, which is precisely the pressure SH-306 was filed for.
#
# Reported every time, never silently: a detector whose whole subject is a
# population that accumulated unnoticed for three days does not get to be the
# next quiet thing (SH-306).
collect_abandoned() {
    local found survivors count
    found="$(abandoned)"
    [ -n "$found" ] || return 0

    count="$(printf '%s\n' "$found" | wc -l | tr -d ' ')"
    note "${count} daemon(s) are serving a store that no longer exists (${context}); collecting"
    durable_report "${context} abandoned-store reap: SIGTERM" "$found"

    survivors="$(reap abandoned)"
    if [ -n "$survivors" ]; then
        # Deliberately not fatal, unlike the postlude's own SIGKILL survivor:
        # that one is provably this run's, and this one is provably not
        # anyone's in particular. A process that will not die to SIGKILL is
        # worth naming loudly; it is not worth failing a stranger's suite over.
        report "abandoned-store daemon(s) survived SIGKILL" "$survivors"
        durable_report "${context} abandoned-store reap FAILED -- survived SIGKILL" "$survivors"
        note "left ${survivors} alive; nothing here depends on them being gone"
    else
        durable_report "${context} abandoned-store reap: cleared" "$found"
    fi
    return 0
}

# Ahead of both phases' own questions, so that a daemon this collects is never
# also reported by them: a `target/debug/story` on a store that is gone is
# this class, not a live process this run has to reason about.
collect_abandoned

if [ "$phase" = "postlude" ]; then
    deadline=$((SECONDS + ORPHAN_GRACE_SECS))
    while [ "$SECONDS" -lt "$deadline" ]; do
        orphans="$(matches)"
        [ -z "$orphans" ] && exit 0
        sleep 0.25
    done

    # Still here after the grace window -- collect it. The preflight already
    # proved this worktree was clean when the run began, so this is provably
    # something the run just finished spawned, not a stranger's process.
    pids="$(matches)"
    report "reaping test-spawned server process(es) this run leaked" "$pids"
    durable_report "postlude reap: SIGTERM" "$pids"

    remaining="$(reap matches)"

    if [ -n "$remaining" ]; then
        report "survived SIGKILL -- a real leak, not the defence working" "$remaining"
        durable_report "postlude reap FAILED -- survived SIGKILL" "$remaining"
        die "a test-spawned server process from this worktree would not die \
even after SIGKILL. Something is keeping it alive (a wedged syscall, a \
respawning supervisor). Stop it by hand, then re-run:

  kill -9 $remaining"
    fi

    note "reaped a leaked test-spawned server process; tree still certifies clean"
    durable_report "postlude reap: cleared" "$pids"
    exit 0
fi

# preflight and check: refuse on sight, never kill.
orphans="$(matches)"

if [ -z "$orphans" ]; then
    exit 0
fi

report "test-spawned server process(es) from this worktree are still running (${context})" "$orphans"
durable_report "${context} refused" "$orphans"
cat >&2 <<EOF

They hold ports that a later run can be handed, and a test that reaches one of
them sees an empty registry (spurious 404s everywhere). Stop them, then re-run:

  kill $(printf '%s' "$orphans" | tr '\n' ' ')
EOF
exit 1
