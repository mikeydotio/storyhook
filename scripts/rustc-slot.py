#!/usr/bin/env python3
"""The machine-wide bound on concurrent rustc processes (SH-655).

`.cargo/config.toml` names this file as `build.rustc-wrapper`, so cargo runs
it in place of every rustc it would otherwise spawn -- from `make test`, from
a bare `cargo test --test foo` typed in an agent's tmux pane, from
rust-analyzer, from a release build, from the Lima guest.  A wrapper reached
through the checkout's own config is the only door every one of those walks
through; the harness scripts can wrap what they run and nothing else, and it
was the invocation nobody wraps that saturated the machine (seven sessions,
load 33 on ten cores).

WHAT IT DOES.  A real compile -- an invocation carrying `--crate-name` --
takes one of K slots and then EXECS rustc in place, holding the slot's lock
on an inherited file descriptor.  The slot's lifetime is therefore rustc's
own lifetime, and the kernel is the liveness oracle: a rustc that exits,
crashes, or is SIGKILLed releases its slot the instant it dies, with no pid
file, no reaper and no stale-reclaim path to get wrong (the SH-528 rule at
the primitive level -- `machine-lock.sh` needs pid+lstart liveness because
its holder is a shell; this one's holder is the compiler itself).  A probe
(`rustc -vV`, `--print cfg`, anything without a crate name) passes straight
through: cargo and rust-analyzer ask those constantly, and a probe queued
behind a compile would be this story's own load-dependent verdict one layer
down.

K IS DERIVED, NEVER PICKED (SH-394).  On Darwin it is the performance-core
count, `hw.perflevel0.logicalcpu` (8 on the machine this was written on,
against `hw.ncpu` = 10): the two efficiency cores are markedly slower for
LLVM codegen, and counting them oversubscribes the fast ones.  With a loud
fallback to `hw.ncpu` when the perflevel key is absent (an Intel Mac), and
`sched_getaffinity` on Linux.  `STORYHOOK_BUILD_SLOTS` overrides it for the
tests that have to prove the bound with a small K.

THE SLOT ROOT DERIVES FROM $HOME, NEVER FROM $XDG_STATE_HOME -- exactly
`scripts/machine-lock.sh`'s rule, for its reason: `scripts/run-tests.sh`
re-exports `XDG_STATE_HOME` into a fresh per-run directory, so a root read
from it would be unique to each run, every concurrent build would take its
own slots, and the bound would bound nothing (the SH-364 shape: a harness
lying to the gate under it).  `STORYHOOK_LOCK_DIR` overrides the root, the
same seam `tests/machine_lock.rs` already uses; neither variable belongs in
the test-environment table (SH-531 -- they say nothing about which store a
process reaches).

A WAIT IS NEVER SILENT (SH-306).  When every slot is held, the wait is
reported on stderr as it begins (cargo forwards a wrapper's non-JSON stderr
verbatim -- measured under `cargo build`, `--message-format=json` and
`cargo clippy` before this was written), again every WAIT_REPORT_SECS, and
once more when it ends with how long it took.  Inside a gate-held `make
test` the same cadence appends an activity line to the SH-524 progress
journal, so the gate's silence watchdog (`machine-lock.sh --max-idle`, which
reads journal GROWTH) sees a compile queued behind other sessions' builds as
progress rather than as a wedged holder to reap; like every emitter of that
journal, this is a no-op when `STORYHOOK_GATE_PROGRESS` is unset.

FAIL OPEN, LOUDLY.  A slot root that cannot be created or written, a slot
file that cannot be opened, a lock call that fails for any reason but
contention: each prints one stderr line naming itself and the cause, and
then runs rustc UNBOUNDED.  A compiler that refuses to compile over a lock
directory is the SH-404/405 dead end; a wrapper that throttled nothing and
said nothing would be SH-306's.  The one failure this file cannot soften is
a missing `python3`, which stops cargo before any of this runs -- so the
release preflight refuses by name on a host or guest without it.

    rustc-slot.py --plan          print root, K and its derivation as JSON
    rustc-slot.py <rustc> args…   what cargo runs

Design of record: `docs/spec/test-tiers.md`, "The compile bound".
"""

import ctypes
import fcntl
import json
import os
import signal
import sys
import time

# How often a blocked waiter re-scans every slot rather than staying parked on
# the one it chose.  Equal to `machine-lock.sh`'s LOCK_POLL_SECS, and for the
# same reason: the gate watchdog that reads this wrapper's journal lines polls
# once a second, so a re-check faster than that cannot be observed by it.
# `tests/build_slots.rs` pins the two equal rather than letting them drift.
RESCAN_SECS = 1

# How often a wait that is still going is reported again -- on stderr and, in
# a gate-held run, to the progress journal.  Equal to `machine-lock.sh`'s
# WAIT_REPORT_SECS (itself `make test`'s measured warm median), pinned by the
# same test, so one number governs "how long is a wait worth mentioning
# again" on both sides of the gate.
WAIT_REPORT_SECS = 36

SLOT_DIR = "build-slots"
PROGRAM = "rustc-slot"


def note(message):
    """One stderr line, prefixed so a reader can grep the wrapper out of cargo's own output."""
    sys.stderr.write(f"{PROGRAM}: {message}\n")
    sys.stderr.flush()


def slot_root():
    """`$STORYHOOK_LOCK_DIR`, else `~/.local/state/storyhook/locks` -- machine-lock.sh's root -- plus this wrapper's own subdirectory."""
    override = os.environ.get("STORYHOOK_LOCK_DIR")
    if override:
        base = override
    else:
        home = os.environ.get("HOME") or os.path.expanduser("~")
        base = os.path.join(home, ".local", "state", "storyhook", "locks")
    return os.path.join(base, SLOT_DIR)


def sysctl_u32(name):
    """One unsigned sysctl value by name, without forking a `sysctl` process per rustc."""
    libc = ctypes.CDLL(None, use_errno=True)
    value = ctypes.c_uint32(0)
    size = ctypes.c_size_t(ctypes.sizeof(value))
    rc = libc.sysctlbyname(name.encode(), ctypes.byref(value), ctypes.byref(size), None, 0)
    if rc != 0:
        raise OSError(ctypes.get_errno(), f"sysctlbyname({name}) failed")
    return value.value


def derive_slots():
    """(K, source): the performance-core count and the name of the fact it came from."""
    override = os.environ.get("STORYHOOK_BUILD_SLOTS")
    if override:
        try:
            k = int(override)
        except ValueError:
            k = 0
        if k >= 1:
            return k, "STORYHOOK_BUILD_SLOTS"
        note(f"STORYHOOK_BUILD_SLOTS={override!r} is not a positive integer; deriving from the machine instead")
    if sys.platform == "darwin":
        try:
            return sysctl_u32("hw.perflevel0.logicalcpu"), "hw.perflevel0.logicalcpu"
        except OSError:
            note("hw.perflevel0.logicalcpu is not available on this machine; falling back to hw.ncpu, which counts efficiency cores too")
            return sysctl_u32("hw.ncpu"), "hw.ncpu"
    if hasattr(os, "sched_getaffinity"):
        return len(os.sched_getaffinity(0)), "sched_getaffinity"
    return os.cpu_count() or 1, "os.cpu_count"


def plan():
    k, source = derive_slots()
    print(json.dumps({
        "root": slot_root(),
        "slots": k,
        "source": source,
        "rescan_secs": RESCAN_SECS,
        "report_secs": WAIT_REPORT_SECS,
    }, indent=2))


def crate_name(args):
    """The `--crate-name` operand, or None for a probe that compiles nothing."""
    for i, arg in enumerate(args):
        if arg == "--crate-name" and i + 1 < len(args):
            return args[i + 1]
    return None


def exec_rustc(argv):
    """Replace this process with rustc; nothing after this line runs on success."""
    os.execvp(argv[0], argv)


def try_lock(fd):
    """True if the slot was taken; False if another rustc holds it."""
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        return True
    except BlockingIOError:
        return False


def holders(root, k):
    """Best-effort `pid crate` of each slot's current holder, for the wait report."""
    found = []
    for i in range(k):
        try:
            with open(os.path.join(root, f"slot-{i}.holder"), encoding="utf-8") as f:
                found.append(f.read().strip())
        except OSError:
            found.append("?")
    return found


def journal(label, status):
    """One SH-524 activity line, when a gate-held run is watching; a no-op otherwise."""
    path = os.environ.get("STORYHOOK_GATE_PROGRESS")
    if not path:
        return
    item = os.environ.get("STORYHOOK_GATE_PROGRESS_PATH") or "release gate"
    line = json.dumps({
        "kind": "activity",
        "path": item,
        "label": label,
        "status": status,
        "at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    })
    try:
        with open(path, "a", encoding="utf-8") as f:
            f.write(line + "\n")
    except OSError as error:
        note(f"could not append to the gate progress journal {path}: {error}")


class Rescan(Exception):
    """Raised from the SIGALRM handler so a blocking flock returns to the re-scan loop
    (Python otherwise restarts the syscall on EINTR, PEP 475)."""


def acquire(root, k, crate):
    """Take one slot, waiting loudly; returns the locked fd."""
    fds = []
    for i in range(k):
        fds.append(os.open(os.path.join(root, f"slot-{i}"), os.O_RDWR | os.O_CREAT, 0o644))
    order = [(os.getpid() + n) % k for n in range(k)]
    for i in order:
        if try_lock(fds[i]):
            return finish(fds, i, root, crate, waited=None)

    parked = order[0]
    started = time.monotonic()
    last_report = started
    label = f"waiting for a build slot ({crate})"
    note(f"all {k} build slots under {root} are held ({'; '.join(holders(root, k))}); {crate} is waiting")
    journal(label, "running")

    def alarm(_signo, _frame):
        raise Rescan()

    previous = signal.signal(signal.SIGALRM, alarm)
    try:
        while True:
            signal.setitimer(signal.ITIMER_REAL, RESCAN_SECS)
            try:
                fcntl.flock(fds[parked], fcntl.LOCK_EX)
                signal.setitimer(signal.ITIMER_REAL, 0)
                return finish(fds, parked, root, crate, waited=time.monotonic() - started)
            except Rescan:
                pass
            for i in order:
                if i != parked and try_lock(fds[i]):
                    return finish(fds, i, root, crate, waited=time.monotonic() - started)
            now = time.monotonic()
            if now - last_report >= WAIT_REPORT_SECS:
                last_report = now
                note(f"{crate} has waited {int(now - started)}s for a build slot ({'; '.join(holders(root, k))})")
                journal(label, "running")
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        signal.signal(signal.SIGALRM, previous)


def finish(fds, held, root, crate, waited):
    """Record the holder, drop the other slot fds, report a wait that ended."""
    for i, fd in enumerate(fds):
        if i != held:
            os.close(fd)
    try:
        with open(os.path.join(root, f"slot-{held}.holder"), "w", encoding="utf-8") as f:
            f.write(f"{os.getpid()} {crate}\n")
    except OSError:
        pass
    if waited is not None:
        note(f"{crate} waited {waited:.1f}s for build slot {held}")
        journal(f"build slot acquired ({crate})", "passed")
    os.set_inheritable(fds[held], True)
    return fds[held]


def main():
    argv = sys.argv[1:]
    if argv == ["--plan"]:
        plan()
        return
    if not argv:
        note("usage: rustc-slot.py --plan | <rustc> <args...>")
        sys.exit(2)
    crate = crate_name(argv[1:])
    if crate is None:
        exec_rustc(argv)

    root = slot_root()
    try:
        os.makedirs(root, exist_ok=True)
        k, _source = derive_slots()
        acquire(root, k, crate)
    except OSError as error:
        note(f"could not take a build slot under {root} ({error}); running rustc UNBOUNDED for {crate}")
    exec_rustc(argv)


if __name__ == "__main__":
    main()
