#!/usr/bin/env python3
"""Run a Rust test battery's binaries concurrently under a thread budget.

Called by scripts/run-tests.sh after it has built every selected target
(SH-783). Cargo runs the test binaries of one invocation one after another,
so a battery of 330 binaries took the sum of their run times. This driver
runs one `cargo test` per binary, admitting a binary only while the sum of
the test threads in flight stays within --budget. A binary gets as many
threads as it has tests, capped by the battery's own --test-threads, so a
one-test binary takes one slot, not four.

Each binary writes its own capture file. A capture is copied to stdout and
to --log only after every binary before it in the given order has been, so
the combined log has the same shape as one serial `cargo test`: every
`Running` line is followed by that binary's own cases. scripts/test_output.py,
scripts/test-delta.sh, verify-pr.sh and the daemon's bundled parser therefore
need no change.

Binaries start longest first, from the run times this driver recorded last
time (--durations), so the slowest binary does not start last and set the
wall clock by itself. A binary with no record starts before every recorded
one, largest first.

Status: a binary's own status of 125 or more (a signal or a harness fault)
passes through, the highest one; any other failure is 101, as `cargo test`
reports it. Cancellation: the owner signals the whole process group, which
already includes every job, so a signal here stops new admissions and waits.
Jobs still running after a grace period get their process trees terminated,
for the case where only this process was signalled.
"""

import json
import os
import signal
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

sys.dont_write_bytecode = True

POLL_SECONDS = 0.1
DEFAULT_THREAD_CAP = 4
GROUP_SIGNAL_GRACE = 10
TERMINATE_GRACE = 5
LIST_TIMEOUT = 120
# Set by run-tests.sh for its pre-build; a job must not repeat that build.
BUILD_ONLY_ENV = ("STORYHOOK_COMPILER_DIAGNOSTICS", "STORYHOOK_GATE_BUILD_OUTCOME")


class Job:
    """One test target: `cargo test -p <package> --test <name>` or `--lib`."""

    def __init__(self, index, package, kind, name):
        self.index = index
        self.package = package
        self.kind = kind
        self.name = name
        self.threads = 1
        self.tests = None
        self.process = None
        self.started = None
        self.seconds = None
        self.status = None
        self.log = None

    @property
    def key(self):
        """The durations-file key: stable across runs and worktrees."""
        return f"{self.package}:{self.kind}:{self.name}"

    def selector(self):
        """Cargo's target selection for this job."""
        return ["--lib"] if self.kind == "lib" else ["--test", self.name]


def parse_arguments(argv):
    """Returns (options, jobs, cargo_extra, libtest_args)."""
    options = {"budget": None, "log": None, "work": None, "progress": None, "durations": None}
    jobs = []
    index = 0
    while index < len(argv) and argv[index] != "--":
        flag = argv[index]
        if flag == "--job" and index + 3 < len(argv):
            jobs.append(Job(len(jobs), *argv[index + 1:index + 4]))
            if jobs[-1].kind not in ("test", "lib"):
                raise ValueError(f"job kind must be test or lib, got {jobs[-1].kind!r}")
            index += 4
        elif flag.startswith("--") and flag[2:] in options and index + 1 < len(argv):
            options[flag[2:]] = argv[index + 1]
            index += 2
        else:
            raise ValueError(f"unexpected argument {flag!r}")
    if index == len(argv):
        raise ValueError("missing `--` before the cargo arguments")
    rest = argv[index + 1:]
    for required in ("budget", "log", "work"):
        if options[required] is None:
            raise ValueError(f"--{required} is required")
    budget = options["budget"]
    if not budget.isdigit() or int(budget) < 1:
        raise ValueError(f"--budget must be a positive integer, got {budget!r}")
    options["budget"] = int(budget)
    if not jobs:
        raise ValueError("no --job given")
    if "--" in rest:
        split = rest.index("--")
        return options, jobs, rest[:split], rest[split + 1:]
    return options, jobs, rest, []


def thread_cap(libtest_args, budget):
    """The battery's --test-threads (removed from the args), capped by budget."""
    cap = DEFAULT_THREAD_CAP
    kept = []
    index = 0
    while index < len(libtest_args):
        arg = libtest_args[index]
        if arg.startswith("--test-threads="):
            cap = int(arg.split("=", 1)[1])
        elif arg == "--test-threads" and index + 1 < len(libtest_args):
            cap = int(libtest_args[index + 1])
            index += 1
        else:
            kept.append(arg)
        index += 1
    return max(1, min(cap, budget)), kept


def job_environment():
    """This process's environment without the pre-build's diagnostics hooks."""
    env = os.environ.copy()
    for key in BUILD_ONLY_ENV:
        env.pop(key, None)
    return env


def executables(jobs, cargo_extra, env):
    """Maps (kind, name) to each job's built test executable.

    `--no-run` after run-tests.sh's pre-build only reports the artifacts; it
    builds nothing. A job whose executable is not found keeps the battery's
    full thread cap rather than failing: its test count is an optimization.
    """
    found = {}
    groups = {}
    for job in jobs:
        groups.setdefault(job.package, []).append(job)
    for package, members in groups.items():
        command = ["cargo", "test", "--no-run", "--message-format=json", "-p", package, *cargo_extra]
        for job in members:
            command += job.selector()
        result = subprocess.run(command, env=env, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, check=False)
        for line in result.stdout.splitlines():
            try:
                message = json.loads(line)
            except ValueError:
                continue
            if message.get("reason") != "compiler-artifact" or not message.get("executable"):
                continue
            target = message.get("target", {})
            kind = "lib" if "lib" in target.get("kind", []) else "test"
            found[(package, kind, target.get("name"))] = message["executable"]
    return found


def count_tests(executable, libtest_args, env):
    """The number of tests `executable` would run with these libtest args."""
    try:
        result = subprocess.run(
            [executable, "--list", *libtest_args],
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=LIST_TIMEOUT,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    if result.returncode != 0:
        return None
    return sum(1 for line in result.stdout.decode(errors="replace").splitlines() if line.endswith(": test"))


def read_durations(path):
    """Recorded seconds per job key; empty when there is no usable record."""
    durations = {}
    if not path:
        return durations
    try:
        with open(path, encoding="utf-8") as handle:
            for line in handle:
                key, _, seconds = line.rstrip("\n").partition("\t")
                try:
                    durations[key] = float(seconds)
                except ValueError:
                    continue
    except OSError:
        pass
    return durations


def write_durations(path, durations, jobs):
    """Merges this run's durations into the record, atomically."""
    if not path:
        return
    for job in jobs:
        if job.seconds is not None and job.status == 0:
            durations[job.key] = job.seconds
    try:
        Path(path).parent.mkdir(parents=True, exist_ok=True)
        temporary = f"{path}.{os.getpid()}.tmp"
        with open(temporary, "w", encoding="utf-8") as handle:
            for key in sorted(durations):
                handle.write(f"{key}\t{durations[key]:.2f}\n")
        os.replace(temporary, path)
    except OSError as error:
        print(f"test-pool: could not record durations in {path}: {error}", file=sys.stderr)


def schedule(jobs, durations):
    """Unrecorded jobs first (most tests first), then the longest recorded."""
    def rank(job):
        seconds = durations.get(job.key)
        if seconds is None:
            return (0, -(job.tests or 0), job.index)
        return (1, -seconds, job.index)

    return sorted(jobs, key=rank)


def job_command(job, options, cargo_extra, libtest_args):
    """The observed `cargo test` for one job, capturing to its own file."""
    here = Path(__file__).resolve().parent
    command = [sys.executable, str(here / "activity-run.py"), "--capture", job.log]
    if options["progress"]:
        command += ["--test-progress", options["progress"]]
    command += ["run-tests.sh/cargo", "--", "cargo", "test", "--no-fail-fast", "-p", job.package]
    command += job.selector() + cargo_extra
    command += ["--", *libtest_args, f"--test-threads={job.threads}"]
    return command


def rebuilt_after_prebuild(log_path):
    """Whether Cargo compiled anything in a job before it started running."""
    try:
        with open(log_path, encoding="utf-8", errors="replace") as handle:
            for line in handle:
                if line.startswith("     Running "):
                    return False
                if line.startswith("   Compiling "):
                    return True
    except OSError:
        return False
    return False


def process_tree(pid):
    """`pid` and all of its descendants, parents first."""
    tree = [pid]
    try:
        children = subprocess.run(
            ["pgrep", "-P", str(pid)], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, check=False
        ).stdout.split()
    except OSError:
        return tree
    for child in children:
        tree += process_tree(int(child))
    return tree


class Pool:
    """Admits, reaps and flushes jobs; owns cancellation."""

    def __init__(self, options, jobs, cargo_extra, libtest_args, env):
        self.options = options
        self.jobs = jobs
        self.cargo_extra = cargo_extra
        self.libtest_args = libtest_args
        self.env = env
        self.running = {}
        self.in_flight = 0
        self.flushed = 0
        self.cancelled = None

    def cancel(self, signum, _frame):
        """Stops admission; the loop then drains or terminates what runs."""
        if self.cancelled is None:
            self.cancelled = signum

    def admit(self, job):
        job.started = time.monotonic()
        # No new session or group: the owner's group signal must reach it.
        job.process = subprocess.Popen(
            job_command(job, self.options, self.cargo_extra, self.libtest_args),
            env=self.env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        self.running[job.process.pid] = job
        self.in_flight += job.threads

    def reap(self):
        """Collects finished jobs. Returns whether any finished.

        Polls each running job rather than waiting on any child: a blocking
        wait would hold off cancellation until some job happened to finish.
        """
        reaped = False
        for pid, job in list(self.running.items()):
            status = job.process.poll()
            if status is None:
                continue
            del self.running[pid]
            reaped = True
            job.seconds = time.monotonic() - job.started
            job.status = status if status >= 0 else 128 - status
            self.in_flight -= job.threads
            if job.status == 0 and rebuilt_after_prebuild(job.log):
                print(
                    f"test-pool: {job.key} compiled after the pre-build, so the tree changed "
                    "mid-run or the pre-build missed it; failing it rather than trusting it",
                    file=sys.stderr,
                )
                job.status = 101
        return reaped

    def flush(self, combined):
        """Writes finished captures in the given order, stopping at a gap."""
        while self.flushed < len(self.jobs) and self.jobs[self.flushed].status is not None:
            job = self.jobs[self.flushed]
            try:
                data = Path(job.log).read_bytes()
            except OSError as error:
                data = f"test-pool: capture of {job.key} unreadable: {error}\n".encode()
            sys.stdout.buffer.write(data)
            sys.stdout.buffer.flush()
            combined.write(data)
            combined.flush()
            self.flushed += 1

    def terminate_running(self):
        """After the group-signal grace, ends every running job's tree."""
        deadline = time.monotonic() + GROUP_SIGNAL_GRACE
        while self.running and time.monotonic() < deadline:
            if not self.reap():
                time.sleep(POLL_SECONDS)
        for sig, grace in ((signal.SIGTERM, TERMINATE_GRACE), (signal.SIGKILL, TERMINATE_GRACE)):
            if not self.running:
                break
            for pid in list(self.running):
                for member in reversed(process_tree(pid)):
                    try:
                        os.kill(member, sig)
                    except OSError:
                        pass
            deadline = time.monotonic() + grace
            while self.running and time.monotonic() < deadline:
                if not self.reap():
                    time.sleep(POLL_SECONDS)

    def run(self, order):
        pending = list(order)
        budget = self.options["budget"]
        with open(self.options["log"], "ab") as combined:
            while (pending or self.running) and self.cancelled is None:
                while pending and self.cancelled is None:
                    job = pending[0]
                    if self.running and self.in_flight + job.threads > budget:
                        break
                    self.admit(pending.pop(0))
                if not self.reap():
                    time.sleep(POLL_SECONDS)
                self.flush(combined)
            if self.cancelled is not None:
                self.terminate_running()
                self.flush(combined)


def main(argv):
    try:
        options, jobs, cargo_extra, libtest_args = parse_arguments(argv)
    except ValueError as error:
        print(f"test-pool: {error}", file=sys.stderr)
        return 2
    cap, libtest_args = thread_cap(libtest_args, options["budget"])
    work = Path(options["work"])
    work.mkdir(parents=True, exist_ok=True)
    env = job_environment()

    built = executables(jobs, cargo_extra, env)

    def listed(job):
        executable = built.get((job.package, job.kind, job.name))
        return count_tests(executable, libtest_args, env) if executable else None

    # Listing executes each binary once. Where run-tests.sh's discovery has not
    # already done that, the first exec of a freshly linked binary pays macOS's
    # code-signature check, so the listings share the budget rather than
    # queueing one behind another.
    with ThreadPoolExecutor(max_workers=options["budget"]) as executor:
        counts = list(executor.map(listed, jobs))
    for job, tests in zip(jobs, counts):
        job.log = str(work / f"{job.index}.log")
        job.tests = tests
        job.threads = cap if tests is None else max(1, min(cap, tests))

    durations = read_durations(options["durations"])
    pool = Pool(options, jobs, cargo_extra, libtest_args, env)
    for signum in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(signum, pool.cancel)
    started = time.monotonic()
    pool.run(schedule(jobs, durations))
    if pool.cancelled is not None:
        return 128 + pool.cancelled

    write_durations(options["durations"], durations, jobs)
    slowest = sorted(jobs, key=lambda job: -(job.seconds or 0))[:5]
    print(
        f"test-pool: {len(jobs)} binaries, thread budget {options['budget']}, "
        f"{time.monotonic() - started:.0f}s; slowest: "
        + ", ".join(f"{job.name} {job.seconds:.0f}s" for job in slowest),
        flush=True,
    )
    statuses = [job.status for job in jobs]
    signalled = [status for status in statuses if status >= 125]
    if signalled:
        return max(signalled)
    return 101 if any(statuses) else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
