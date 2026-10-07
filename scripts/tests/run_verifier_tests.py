#!/usr/bin/env python3
"""Bounded, process-isolated execution of the verifier unittest suites."""

import argparse
import importlib
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest

sys.dont_write_bytecode = True

SUITES = {'lifecycle': 'test_verifier_lifecycle', 'verdict': 'test_verifier_verdict'}
# The host admission entry this case pool runs as (SH-869); its units are cases.
ENTRY = "verifier-python-workers"
ADMIT = Path(__file__).resolve().parents[1] / 'host-admit.py'
POLL_INTERVAL = 0.01


def discover(module, selectors=()):
    """Return exact selectors for tests defined by this module only."""
    loader = unittest.TestLoader()
    cases = []
    for cls in vars(module).values():
        if (isinstance(cls, type) and issubclass(cls, unittest.TestCase)
                and cls.__module__ == module.__name__):
            for case in loader.loadTestsFromTestCase(cls):
                cases.append(case.id().removeprefix(module.__name__ + '.'))
    if loader.errors:
        raise ValueError('discovery failed: ' + '\n'.join(loader.errors))
    if not cases:
        raise ValueError(f'{module.__name__}: no tests discovered')
    if len(set(cases)) != len(cases):
        raise ValueError(f'{module.__name__}: duplicate discovered case identities')
    if selectors:
        if len(set(selectors)) != len(selectors):
            raise ValueError('duplicate case selectors')
        unknown = sorted(set(selectors) - set(cases))
        if unknown:
            raise ValueError('unknown exact case selector(s): ' + ', '.join(unknown))
        return list(selectors)
    return sorted(cases)


def run_cases(script, cases, jobs=2, stream=None):
    """Run isolated cases with bounded admission and complete diagnostics."""
    if jobs < 1 or not cases or len(set(cases)) != len(cases):
        raise ValueError('require positive jobs and nonempty, unique cases')
    stream = sys.stdout if stream is None else stream
    active = []
    cancelled = None
    reported_cancel = False
    next_case = completed = failed = 0
    started = time.monotonic()

    def cancel(signum, _frame):
        nonlocal cancelled
        if cancelled is None:
            cancelled = signum

    signals = (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)
    previous = {sig: signal.signal(sig, cancel) for sig in signals}
    try:
        while next_case < len(cases) or active:
            while cancelled is None and next_case < len(cases) and len(active) < jobs:
                name = cases[next_case]
                next_case += 1
                # Files cannot fill a pipe while the admission loop waits for exits.
                capture = tempfile.TemporaryFile(mode='w+', encoding='utf-8', errors='replace', dir='/tmp')
                try:
                    process = subprocess.Popen([sys.executable, '-B', str(script), name, '-v'],
                                               stdout=capture, stderr=subprocess.STDOUT)
                except OSError as error:
                    capture.close()
                    completed += 1
                    failed += 1
                    print(f'case-runner: {name}: launch failed: {error}', file=stream, flush=True)
                else:
                    active.append((name, process, capture))
            if cancelled is not None and not reported_cancel:
                print(f'case-runner: cancelling ({signal.Signals(cancelled).name}); '
                      f'draining {len(active)} active cases', file=stream, flush=True)
                reported_cancel = True
            for entry in active[:]:
                name, process, capture = entry
                status = process.poll()
                if status is None:
                    continue
                completed += 1
                failed += status != 0
                print(f'case-runner: {name}: status={status}', file=stream)
                capture.seek(0)
                while True:
                    chunk = capture.read(65536)
                    if not chunk:
                        break
                    stream.write(chunk)
                print(file=stream, flush=True)
                capture.close()
                active.remove(entry)
            if cancelled is not None and not active:
                break
            if active:
                time.sleep(POLL_INTERVAL)
    finally:
        # Do not kill fixture parents before their registered cleanups run. On
        # group cancellation the enclosing verifier still owns every descendant.
        try:
            for _, process, capture in active:
                try:
                    process.wait()
                finally:
                    capture.close()
        finally:
            for sig, handler in previous.items():
                signal.signal(sig, handler)
    print(f'case-runner: selected={len(cases)} completed={completed} failed={failed} '
          f'jobs={jobs} elapsed={time.monotonic() - started:.3f}s', file=stream, flush=True)
    return 128 + cancelled if cancelled is not None else int(failed != 0)


def main(argv=None):
    """Parse the verifier suite selection and return its combined status."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('suite', choices=SUITES)
    parser.add_argument('cases', nargs='*', help='exact Class.method selectors; default: all local cases')
    parser.add_argument('--jobs', type=int, default=2, help='maximum simultaneous cases (default: 2)')
    parser.add_argument('--list', action='store_true', help='list selected cases without executing them')
    args = parser.parse_intermixed_args(argv)
    if args.jobs < 1:
        parser.error('--jobs must be positive')
    try:
        module = importlib.import_module(SUITES[args.suite])
        cases = discover(module, args.cases)
    except (ImportError, OSError, SyntaxError, ValueError) as error:
        parser.error(str(error))
    if args.list:
        print('\n'.join(cases))
        return 0
    jobs = admitted_jobs(args.jobs) if argv is None else args.jobs
    return run_cases(Path(module.__file__).resolve(), cases, jobs)


def admitted_jobs(requested):
    """Run as the case pool's host admission entry; return its admitted case count (SH-869).

    The adapter's marker ends the re-exec: this pid when it ran the pool in
    place, the parent's when it supervises it. A nested pool's count comes
    from its inherited share, so it cannot multiply the grant it runs in.
    """
    marker = os.environ.get('STORYHOOK_HOST_ENTRY', '')
    if marker not in (f'{ENTRY}:{os.getpid()}', f'{ENTRY}:{os.getppid()}'):
        os.execv(sys.executable, [sys.executable, '-B', str(ADMIT), '--entry', ENTRY, '--units', str(requested),
                                  '--', sys.executable, '-B', str(Path(__file__).resolve()), *sys.argv[1:]])
    return int(os.environ.pop('STORYHOOK_HOST_UNITS', requested))


if __name__ == "__main__":
    sys.exit(main())
