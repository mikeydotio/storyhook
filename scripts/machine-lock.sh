#!/usr/bin/env bash
#
# A pid-checked, stale-tolerant, machine-wide advisory lock — SH-456.
#
#   machine-lock.sh [--plan] [--max-wait <seconds>] <name> -- <command...>
#
# Runs <command...> with <name> held, and exits with the command's own status.
# Two names are reserved by later stories in the Full Auto epic (SH-452):
# `gate`, taken inside `scripts/run-tests.sh` so every `make test` on this
# machine serializes (D4), and `merge`, taken by `scripts/land-pr.sh` (D5).
# Neither caller exists yet; this ships the primitive alone.
#
# WHY. `make test` is 36.375s median warm and idle
# (`docs/rearch/baseline/timings.md`) and has been measured at 873s under the
# 3-4 concurrent worktree suites this machine routinely runs — contention that
# is the documented cause of an open class of load-sensitive failures (SH-347,
# SH-349, SH-375, SH-378, SH-401, SH-419). Full Auto's lanes multiply exactly
# that, so the suites have to queue rather than pile up.
#
# THE LOCK ROOT IS DERIVED FROM $HOME, AND DELIBERATELY NOT FROM
# $XDG_STATE_HOME. `src/env/mod.rs` resolves this project's state home as
# `$XDG_STATE_HOME/storyhook`, else `~/.local/state/storyhook`, and the path
# below matches that convention — but it must not read the variable, because
# `scripts/run-tests.sh` exports `XDG_STATE_HOME` into a fresh per-run
# `mktemp -d` directory. SH-457 takes the `gate` lock INSIDE that script: a
# root read from `XDG_STATE_HOME` would therefore be unique to each run, every
# concurrent suite would take a different lock, nothing would serialize, and
# the gate would pass having proved nothing. That is the SH-364 shape — a
# harness that lies to the gate running under it. `$HOME` is per-user (the
# collision SH-263 is about; never a fixed `/tmp` literal), identical in every
# worktree and every shell, and untouched by the harness the lock has to work
# inside. `STORYHOOK_LOCK_DIR` overrides it, which is how `tests/
# machine_lock.rs` runs without ever touching a real lock.
#
# HOLDER IDENTITY IS A PID *AND* A START TIME. A bare pid is the SH-239 trap
# one axis over — ask what a process IS, not what it is spelled. Pids are
# reused, this lock root survives a reboot, and a reused pid is exactly how a
# live holder gets its lock stolen. So the holder is judged live only when the
# pid is alive AND `ps -o lstart=` still reports the start time recorded when
# the lock was taken. Staleness stays a FACT, never a timeout — the rule
# `browser-watch.sh`'s own lock already states, since a timeout here would be
# a bare literal about how long a suite is allowed to take.
#
# WAITING IS REPORTED. A waiter names the holder immediately and again on a
# derived cadence (below). A gate that goes quiet reads as an all-clear
# (SH-306), and a lock silently stolen is the same shape.
#
# THE IDENTITY FILES ARE WRITTEN `started`, `meta`, THEN `pid` — pid LAST, on
# purpose. `mkdir` is the atomic primitive (macOS ships no `flock(1)`), so
# there is a window between taking the directory and describing it. Writing
# the pid last means a reader that sees a pid is guaranteed to see the start
# time beside it; a directory with no pid at all is a holder mid-write, or one
# killed inside that window, and is tolerated for IDENTITY_GRACE_POLLS before
# being reclaimed.
#
# EXIT CODES
#   <the command's own>   the command ran
#   2                     refused before anything ran (bad name, missing `--`,
#                         empty command, unknown flag, no resolvable lock root)
#   75                    --max-wait elapsed with the lock still held
#                         (EX_TEMPFAIL). The command did NOT run, and the
#                         stderr line says so.
#
# STATED LIMITS, named rather than glossed:
#   * Waiters are unordered. A `mkdir` lock has no queue, so a waiter can be
#     passed over under sustained contention. Fairness needs a ticket file,
#     which the design of record does not ask for; the reported wait duration
#     is what makes starvation visible if it ever happens.
#   * `ps -o lstart=` is whole-second granular, so a pid reused within the
#     same second across a reboot still defeats the identity check. The check
#     narrows the window to that; it does not close it.
#   * Forgery is not the threat model — the position `gate-receipt.sh` already
#     takes. Anyone who can hand-write a lock directory can also just not call
#     this script.
#
# Design of record: `docs/spec/full-auto-engine.md`, section "The machine
# locks".

set -uo pipefail

readonly USAGE="usage: machine-lock.sh [--plan] [--max-wait <seconds>] <name> -- <command...>"

die() {
    printf 'machine-lock: %s\n' "$1" >&2
    exit 2
}

note() {
    printf 'machine-lock: %s\n' "$1" >&2
}

# THE THREE DERIVATIONS (SH-394: a ceiling derives from what it is protecting,
# never a bare literal).
#
# LOCK_POLL_SECS is the RESOLUTION OF THE OBSERVATION, not a guess about
# speed: both clocks this script can read — `date +%s` and the `ps -o lstart=`
# it compares a holder against — are whole-second granular, so a re-check
# faster than one second cannot observe a different answer.
readonly LOCK_POLL_SECS=1
#
# GATE_MEDIAN_SECS is `make test`'s own measured warm, idle median from
# `docs/rearch/baseline/timings.md` (36.375s), floored onto the poll's
# whole-second grid. `tests/machine_lock.rs` re-reads that document and fails
# if the two drift apart, so this is a derived value that stays derived
# (SH-136: a number hand-copied to a second place is a number that will
# disagree with the first).
readonly GATE_MEDIAN_SECS=36
#
# A waiter is told immediately, then once per typical suite it is queued
# behind — so every follow-up line means "another whole nominal `make test`
# has passed", which is a unit a reader can act on.
readonly WAIT_REPORT_SECS=$GATE_MEDIAN_SECS
#
# How long a lock directory with no pid in it is tolerated before being
# reclaimed. A holder writes its identity in the statements immediately after
# `mkdir` — two `printf`s and one `ps` — so a directory still nameless after
# two whole observation periods is not mid-write, it is one whose writer died
# inside that window.
readonly IDENTITY_GRACE_POLLS=2

# There is no default wait ceiling, and that is the third derivation: waiting
# is bounded by a FACT — whether the recorded holder is still alive — rather
# than by a clock. A caller with a real budget passes `--max-wait` and states
# its own derivation at its own call site.

plan=0
max_wait=""
while [ "$#" -gt 0 ]; do
    case "$1" in
    (--plan)
        plan=1
        shift
        ;;
    (--max-wait)
        shift
        [ "$#" -gt 0 ] || die "--max-wait needs a whole number of seconds -- $USAGE"
        case "$1" in
        (*[!0-9]* | '')
            die "--max-wait takes a whole number of seconds, not '$1' -- $USAGE"
            ;;
        esac
        max_wait="$1"
        shift
        ;;
    (--)
        die "no lock name before '--' -- $USAGE"
        ;;
    (-*)
        die "unknown option '$1' -- $USAGE"
        ;;
    (*)
        break
        ;;
    esac
done

name="${1:-}"
[ -n "$name" ] || die "a lock name is required -- $USAGE"
# A refusal, not a sanitization: this is also what stops a name like `../../x`
# from placing the lock directory outside the lock root.
case "$name" in
(*[!A-Za-z0-9_-]* | [!A-Za-z0-9]*)
    die "lock name '$name' must match [A-Za-z0-9][A-Za-z0-9_-]* -- $USAGE"
    ;;
esac
shift

# SH-357: an argument that lands nowhere is refused, not dropped. The `--` is
# required even though the name is a single token, so that a command whose
# first word begins with `-` can never be read as this script's own option.
[ "${1:-}" = "--" ] || die "the command must follow a literal '--' -- $USAGE"
shift
[ "$#" -gt 0 ] || die "'--' must be followed by a command to run -- $USAGE"

lock_root="${STORYHOOK_LOCK_DIR:-}"
if [ -z "$lock_root" ]; then
    [ -n "${HOME:-}" ] \
        || die "neither STORYHOOK_LOCK_DIR nor HOME is set, so there is no per-user place to put a machine-wide lock"
    lock_root="$HOME/.local/state/storyhook/locks"
fi
lock="$lock_root/$name.lock"

if [ "$plan" = 1 ]; then
    printf 'name=%s\n' "$name"
    printf 'lock=%s\n' "$lock"
    printf 'max_wait=%s\n' "${max_wait:-none}"
    printf 'command=%s\n' "$*"
    exit 0
fi

# REENTRANCY. `make test` reaches `run-tests.sh` twice per run, and a caller
# who wraps a whole `make test` in `machine-lock.sh gate --` would, without
# this, wait on a lock its own process tree already holds — forever, since the
# holder is provably alive. The name list travels in the environment because
# that is exactly the boundary "this process tree" means.
held="${STORYHOOK_MACHINE_LOCKS:-}"
case ":$held:" in
(*":$name:"*)
    note "'$name' is already held by this process tree ($held) -- running without re-taking it"
    exec "$@"
    ;;
esac

# The identity half of "is this still the holder". Squeezed and trimmed so a
# recorded value and a live one normalize identically; empty for a pid that no
# longer exists.
process_started() {
    ps -o lstart= -p "$1" 2>/dev/null | tr -s ' ' | sed 's/^ *//; s/ *$//'
}

mkdir -p "$lock_root" || die "could not create the lock root at $lock_root"

waited=0
nameless=0
announced=0
while :; do
    if mkdir "$lock" 2>/dev/null; then
        break
    fi

    holder="$(cat "$lock/pid" 2>/dev/null || true)"
    recorded="$(cat "$lock/started" 2>/dev/null || true)"
    meta="$(cat "$lock/meta" 2>/dev/null || true)"

    if [ -z "$holder" ]; then
        # Nameless: a holder mid-write, or one killed inside that window.
        nameless=$((nameless + 1))
        if [ "$nameless" -gt "$IDENTITY_GRACE_POLLS" ]; then
            note "clearing the '$name' lock at $lock: it has recorded no holder for ${nameless}s, so whoever took it died before naming itself"
            rm -rf "$lock" || die "could not clear the nameless '$name' lock at $lock"
            continue
        fi
    else
        nameless=0
        if ! kill -0 "$holder" 2>/dev/null; then
            note "clearing the '$name' lock left by pid $holder, which is no longer running"
            rm -rf "$lock" || die "could not clear the stale '$name' lock at $lock"
            continue
        fi

        live_started="$(process_started "$holder")"
        if [ -n "$recorded" ] && [ "$live_started" != "$recorded" ]; then
            note "clearing the '$name' lock recorded to pid $holder: that pid is alive but started '$live_started', not '$recorded' -- the holder is gone and its pid was reused"
            rm -rf "$lock" || die "could not clear the reused-pid '$name' lock at $lock"
            continue
        fi

        # SH-306: waiting is never silent. Immediately, then once per nominal
        # suite this waiter is queued behind.
        if [ "$announced" = 0 ]; then
            note "waiting for the '$name' lock, held by pid $holder ($meta)"
            announced=1
        elif [ "$waited" -gt 0 ] && [ $((waited % WAIT_REPORT_SECS)) -eq 0 ]; then
            note "still waiting for the '$name' lock after ${waited}s, held by pid $holder ($meta)"
        fi
    fi

    if [ -n "$max_wait" ] && [ "$waited" -ge "$max_wait" ]; then
        note "gave up after ${waited}s: the '$name' lock is still held by pid ${holder:-unknown} ($meta). The command did not run."
        exit 75
    fi

    sleep "$LOCK_POLL_SECS"
    waited=$((waited + LOCK_POLL_SECS))
done

# Only ever remove a lock this process still owns. A reclaim decided a moment
# ago can be overtaken by a new holder, and deleting that holder's directory
# would hand the lock to two processes at once.
release() {
    if [ "$(cat "$lock/pid" 2>/dev/null || true)" = "$$" ]; then
        rm -rf "$lock" 2>/dev/null || true
    fi
}

# THE SIGNAL TRAPS ARE NOT DECORATION, AND THIS IS THE FIRST SCRIPT IN THE
# REPO TO CARRY THEM. `trap ... EXIT` alone is not enough here: bash defers a
# trap until the current foreground command returns, so a SIGTERM arriving
# during a 14-minute `make test` would not release the lock until that suite
# finished — which is the whole failure this script exists to prevent. Hence
# the command runs in the BACKGROUND with an explicit `wait`, during which a
# trap does fire.
#
# The signal is FORWARDED to the command, never escalated. The command owns
# its own teardown — `make test`'s legs each carry their own EXIT traps —
# and SIGKILLing it here would defeat exactly that. Re-raising on self after
# resetting the trap is what keeps this script's own exit status a truthful
# 128+signal rather than a fabricated one.
on_signal() {
    if [ -n "${child:-}" ]; then
        kill -s "$1" "$child" 2>/dev/null || true
    fi
    release
    trap - "$1" EXIT
    kill -s "$1" $$
}

printf '%s\n' "$(process_started $$)" > "$lock/started" \
    || die "could not record the holder's start time in $lock"
printf '%s %s -- %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$PWD" "$*" > "$lock/meta" \
    || die "could not record the holder's description in $lock"
printf '%s\n' "$$" > "$lock/pid" \
    || die "could not record the holder's pid in $lock"

trap release EXIT
trap 'on_signal INT' INT
trap 'on_signal TERM' TERM
trap 'on_signal HUP' HUP

if [ "$waited" -gt 0 ]; then
    note "took the '$name' lock after waiting ${waited}s"
fi

if [ -n "$held" ]; then
    STORYHOOK_MACHINE_LOCKS="$held:$name"
else
    STORYHOOK_MACHINE_LOCKS="$name"
fi
export STORYHOOK_MACHINE_LOCKS

# Bash redirects a background command's stdin from /dev/null in a
# non-interactive shell, which would silently break any command that reads
# stdin. Handing the child this script's own stdin on fd 3 is what keeps the
# wrapper transparent.
exec 3<&0
"$@" <&3 &
child=$!
exec 3<&-

wait "$child"
exit $?
