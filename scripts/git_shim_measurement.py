#!/usr/bin/env python3
"""Bounded SH-797 local Git-shim measurement. Nothing runs without --run-shim.

The real-provider dispatch pair is deliberately a separate, unexecuted plan:
provider authentication, matching executables and cleanup need root review.
This program never launches story, tmux, a provider, Cargo or a network remote.
"""
import argparse
import fcntl
from gate_measurement_exposure import PROTOCOL, snapshot, cpu_interval
import hashlib
import json
import os
from pathlib import Path
import platform
import signal
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import types


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def exact_child_exit(pid, *, nonblocking, wait_api=os):
    flags = wait_api.WEXITED | wait_api.WNOWAIT
    if nonblocking:
        flags |= wait_api.WNOHANG
    result = wait_api.waitid(wait_api.P_PID, pid, flags)
    if result is None:
        if nonblocking:
            return None
        raise RuntimeError('blocking wait returned no exact-child exit proof')
    if type(result.si_pid) is not int or result.si_pid != pid:
        raise RuntimeError('non-reaping wait returned an unexpected child identity')
    return result


class PendingRootObservation(RuntimeError):
    def __init__(self, pid, stage):
        self.pid, self.stage = pid, stage
        super().__init__('retained direct child observation pending: PID %s at %s' % (pid, stage))


def retry_pending(operation, deadline, check=lambda: None, clock=time.monotonic, pause=time.sleep):
    while True:
        check()
        if clock() >= deadline:
            raise RuntimeError('original observation deadline exhausted')
        try:
            return operation()
        except PendingRootObservation as pending:
            if clock() >= deadline:
                raise RuntimeError('original observation deadline exhausted: ' + str(pending))
            pause(min(0.01, max(0, deadline - clock())))


def capture_retained_root(native, boot, pid, exited):
    if exited():
        return None
    try:
        row = native.process(pid, boot)
    except OSError as error:
        # Recheck the actual unreaped child after a native exit race. EPERM,
        # ESRCH, or elapsed time alone never proves that an owner exited.
        if exited():
            return None
        if isinstance(error, ProcessLookupError):
            raise PendingRootObservation(pid, 'initial native capture') from error
        raise
    if row['pid'] != pid or row['session'] != pid:
        raise RuntimeError('child did not establish its owned session')
    return row


def retained_session_members(native, boot, pid, exited, get_session=os.getsid):
    found = []
    for candidate in native.pids():
        if candidate == pid and exited():
            continue  # Do not query Darwin's unqueryable exact retained zombie.
        try:
            session = get_session(candidate)
            if session != pid:
                if candidate == pid:
                    raise RuntimeError('owned direct root changed session')
                continue
            row = native.process(candidate, boot)
        except OSError as error:
            if candidate == pid:
                if exited():
                    continue
                if isinstance(error, ProcessLookupError):
                    raise PendingRootObservation(pid, 'session census') from error
                raise
            if isinstance(error, ProcessLookupError):
                continue
            raise  # Unreadable descendants/other PIDs remain uncertainty.
        if row['live'] and row['session'] == pid:
            found.append(row)
    return found


PAIR_COUNT = 40
CAMPAIGN_SECONDS = 300


def remaining(end, clock=time.monotonic):
    left = end - clock()
    if left <= 0:
        raise RuntimeError('original five-minute measurement deadline exhausted')
    return left


def timed_operation(evidence, name, pair, arm, argv, cwd, command, observe, persist):
    """Persist an attempted arm before execution, including a failed observation."""
    row = {'case': name, 'pair': pair, 'arm': arm, 'status': 'starting'}
    evidence['samples'].append(row)
    persist()
    try:
        row['exposure_before'] = observe()
        result = command(argv, cwd)
        row['elapsed_ns'] = result.elapsed_ns
        row['stdout_sha256'] = hashlib.sha256(result.stdout).hexdigest()
        row['exposure_after'] = observe()
        row['cpu_interval'] = cpu_interval(row['exposure_before'], row['exposure_after'])
        row['status'] = 'complete'
        return row
    except BaseException as error:
        row.update(status='failed', error=repr(error))
        raise
    finally:
        persist()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--run-shim', action='store_true')
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--pin', required=True)
    parser.add_argument('--git', type=Path, required=True)
    parser.add_argument('--output-parent', type=Path, required=True)
    parser.add_argument('--samples', type=int, default=40)
    args = parser.parse_args()
    if not args.run_shim:
        print(json.dumps({'status': 'plan_only', 'runtime_effects': False,
                          'source': str(args.source), 'pin': args.pin,
                          'next': 'Obtain explicit bounded local measurement authority, then add --run-shim.'}))
        return
    if args.samples != PAIR_COUNT:
        parser.error('this reviewed protocol requires exactly 40 pairs per case')
    campaign_started = time.monotonic()
    campaign_end = campaign_started + CAMPAIGN_SECONDS
    real_git = args.git.resolve(strict=True)
    source = args.source.resolve(strict=True)
    if not real_git.is_file() or not os.access(real_git, os.X_OK):
        parser.error('--git must name the reviewed real Git executable')
    args.output_parent.mkdir(parents=True, exist_ok=True)
    lock = os.open(args.output_parent / 'shim-collection.lock', os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    run = Path(tempfile.mkdtemp(prefix='SH-797-shim-', dir=args.output_parent))
    home, bindir = run / 'home', run / 'bin'
    home.mkdir(); bindir.mkdir()
    (bindir / 'git').symlink_to(real_git)
    (bindir / 'python3').symlink_to(Path(sys.executable).resolve())
    env = {'HOME': str(home), 'PATH': str(bindir) + ':/usr/bin:/bin',
           'LANG': 'C', 'LC_ALL': 'C', 'GIT_CONFIG_NOSYSTEM': '1',
           'GIT_CONFIG_GLOBAL': '/dev/null', 'GIT_TERMINAL_PROMPT': '0',
           'GIT_ALLOW_PROTOCOL': 'file', 'PYTHONDONTWRITEBYTECODE': '1'}
    evidence = {'status': 'incomplete', 'source_pin': args.pin, 'samples': [],
                'scope': 'local Git-shim cost only; not production dispatch latency',
                'python': sys.version, 'python_path': sys.executable, 'python_sha256': digest(Path(sys.executable).resolve()),
                'host_load_protocol': PROTOCOL, 'campaign_started': campaign_started,
                'campaign_end': campaign_end, 'campaign_seconds': CAMPAIGN_SECONDS,
                'timed_operation_count': 240, 'warmup_operation_count': 24,
                'git_path': str(real_git), 'git_sha256': digest(real_git),
                'platform': platform.platform(), 'cpu_count': os.cpu_count(),
                'run_root': str(run), 'warmups_per_arm': 4,
                'timing_method': 'Popen entry to waiter-thread waitid(WNOWAIT) exit observation; post-exit native session census and output reads excluded',
                'observer_note': 'One non-reaping waiter thread per command; controller checks cancellation every 10ms. Thread scheduling can delay the exit observation.',
                'cleanup_policy': 'Own fresh session, unreaped leader pin, native PID/start/session recheck, 2s TERM then 2s repeated KILL; uncertainty aborts cohort and retains root'}
    # Even preflight/import failures retain an honest incomplete receipt.
    (run / 'result.json').write_text(json.dumps(evidence, indent=2) + '\n')
    required_wait_api = ('waitid', 'P_PID', 'WEXITED', 'WNOWAIT', 'WNOHANG')
    missing_wait_api = [name for name in required_wait_api if not hasattr(os, name)]
    if missing_wait_api:
        evidence['error'] = 'required non-reaping wait API absent: ' + ', '.join(missing_wait_api)
        (run / 'result.json').write_text(json.dumps(evidence, indent=2) + '\n')
        raise RuntimeError(evidence['error'])

    # Reuse kernel observations only, never admission or a fabricated grant.
    # Bootstrap bytes are pinned before executing any source-provided Python;
    # Git HEAD validation itself will run under this custody implementation.
    # Execute exactly those verified bytes, avoiding package/pycache fallback.
    bootstrap_hashes = {
        '__init__.py': 'a833c256fbc0abbd29439687d59cc200a946e54ebb8c782d86486ba4ebf70f4a',
        'policy.py': 'cbdbd7d8682b6ad23a69edad5800eb67b9980f4c8679a8c0ae309a84ad83b604',
        'native.py': '63bfdc122d95d4ad2d910bd83dde094804c3d20a7aadcc408e068bb55784c7e6',
    }
    bootstrap = {}
    for name, expected in bootstrap_hashes.items():
        path = source / 'scripts/host_admission' / name
        data = path.read_bytes()
        if hashlib.sha256(data).hexdigest() != expected:
            evidence['error'] = 'unreviewed custody bootstrap bytes: ' + name
            (run / 'result.json').write_text(json.dumps(evidence, indent=2) + '\n')
            raise RuntimeError(evidence['error'])
        bootstrap[name] = data
    evidence['custody_bootstrap_sha256'] = bootstrap_hashes
    sys.dont_write_bytecode = True
    package = '_sh797_reviewed_custody'
    for filename in ('__init__.py', 'policy.py', 'native.py'):
        name = package if filename == '__init__.py' else package + '.' + filename[:-3]
        module = types.ModuleType(name)
        module.__file__ = str(source / 'scripts/host_admission' / filename)
        module.__package__ = package
        if filename == '__init__.py':
            module.__path__ = []
        sys.modules[name] = module
        exec(compile(bootstrap[filename], module.__file__, 'exec'), module.__dict__)
    native = sys.modules[package + '.native']
    boot = native.boot_identity()
    cancelled = []
    prior_handlers = {}
    for signum in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        prior_handlers[signum] = signal.signal(signum, lambda number, _frame: cancelled.append(number))
    evidence['commands'] = []

    def persist():
        data = json.dumps(evidence, indent=2, allow_nan=False) + '\n'
        with (run / 'result.json.tmp').open('w') as stream:
            stream.write(data); stream.flush(); os.fsync(stream.fileno())
        os.replace(run / 'result.json.tmp', run / 'result.json')

    context = {'phase': 'preparation'}

    def check_cancelled():
        remaining(campaign_end)
        if cancelled:
            raise RuntimeError('measurement cancelled by signal ' + str(cancelled[0]))

    def command(argv, cwd=run):
        """Pin the owned session until every observed member has settled.

        Never call poll/communicate/wait before the session census is empty:
        the unreaped child pins its PID/session identity, including on timeout.
        Files keep an escaped pipe holder from blocking output collection.
        """
        check_cancelled()
        argv = [str(x) for x in argv]
        receipt = {'argv': argv, 'cwd': str(cwd), 'status': 'starting', **context}
        evidence['commands'].append(receipt)
        persist()
        index = len(evidence['commands'])
        out_path, err_path = run / f'command-{index}.out', run / f'command-{index}.err'
        child = None
        waiter_done = threading.Event()
        exit_observation = {}
        started = None
        cleanup_started = None
        terminated = set()

        def root_exited():
            return exact_child_exit(child.pid, nonblocking=True) is not None

        def members():
            return retained_session_members(native, boot, child.pid, root_exited)

        def signal_members(rows, signum):
            for row in rows:
                if row['pid'] == child.pid and root_exited():
                    continue
                try:
                    now = native.process(row['pid'], boot)
                    if now['start'] == row['start'] and now['session'] == child.pid and now['live']:
                        os.kill(row['pid'], signum)
                except OSError as error:
                    if row['pid'] == child.pid:
                        if root_exited():
                            continue
                        if isinstance(error, ProcessLookupError):
                            raise PendingRootObservation(child.pid, 'signal recheck') from error
                        raise
                    if isinstance(error, ProcessLookupError):
                        continue
                    raise

        def settle(cancel):
            # Two bounded cleanup phases. Repeated KILL censuses catch a child
            # forked after an earlier census; signals never target a bare PGID.
            nonlocal cleanup_started
            if cleanup_started is None:
                cleanup_started = time.monotonic()
            while True:
                rows = retry_pending(members, cleanup_started + 4)
                receipt['last_observed_members'] = rows
                if waiter_done.is_set() and rows:
                    receipt['descendants_outlived_command'] = True
                if waiter_done.is_set() and 'error' in exit_observation:
                    raise RuntimeError('leader observation failed: ' + exit_observation['error'])
                if waiter_done.is_set() and not rows:
                    # The first enumeration may precede the direct root's
                    # exit. Re-census after exact exit proof before reaping.
                    rows = retry_pending(members, cleanup_started + 4)
                    receipt['last_observed_members'] = rows
                    if rows:
                        receipt['descendants_outlived_command'] = True
                if waiter_done.is_set() and not rows:
                    child.wait()
                    receipt['cleanup'] = 'settled; owned session empty before leader reap'
                    return
                elapsed = time.monotonic() - cleanup_started
                if elapsed >= 4:
                    raise RuntimeError('owned session did not settle; retain root and identity receipt')
                if cancel or waiter_done.is_set() or cancelled:
                    if elapsed >= 2:
                        retry_pending(lambda: signal_members(rows, signal.SIGKILL), cleanup_started + 4)
                    else:
                        fresh = [r for r in rows if (r['pid'], r['start']) not in terminated]
                        retry_pending(lambda: signal_members(fresh, signal.SIGTERM), cleanup_started + 4)
                        terminated.update((r['pid'], r['start']) for r in fresh)
                time.sleep(0.01)

        try:
            with out_path.open('wb') as stdout, err_path.open('wb') as stderr:
                started = time.monotonic_ns()
                child = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                                         stdout=stdout, stderr=stderr, start_new_session=True)
            receipt['pid'] = child.pid

            def observe_exit():
                try:
                    result = exact_child_exit(child.pid, nonblocking=False)
                    exit_observation['wait_pid'] = result.si_pid
                    exit_observation['at_ns'] = time.monotonic_ns()
                except BaseException as error:
                    exit_observation['error'] = repr(error)
                finally:
                    waiter_done.set()

            # waitid(WNOWAIT) observes exit without releasing the ownership pin.
            threading.Thread(target=observe_exit, daemon=True).start()
            deadline = min(started / 1_000_000_000 + 10, campaign_end)
            leader = retry_pending(lambda: capture_retained_root(native, boot, child.pid, root_exited),
                                   deadline, check_cancelled)
            receipt['leader'] = leader
            if leader is None:
                receipt['leader_identity'] = 'exact retained exit; no native signal identity minted'
            while not waiter_done.wait(0.01):
                check_cancelled()
                if time.monotonic() >= deadline:
                    raise RuntimeError('command exceeded ten-second observation deadline')
            check_cancelled()
            if 'error' in exit_observation:
                raise RuntimeError(exit_observation['error'])
            if exit_observation['at_ns'] - started > 10_000_000_000:
                raise RuntimeError('command exit observation exceeded ten seconds')
            receipt['elapsed_ns'] = exit_observation['at_ns'] - started
            settle(False)
            if receipt.get('descendants_outlived_command'):
                raise RuntimeError('descendants outlived command; drained but sample is invalid')
            result = subprocess.CompletedProcess(argv, child.returncode,
                                                 out_path.read_bytes(), err_path.read_bytes())
            result.elapsed_ns = receipt['elapsed_ns']
            result.check_returncode()
            receipt['status'] = 'complete'
            return result
        except BaseException as error:
            receipt['status'] = 'failed'
            receipt['error'] = repr(error)
            if child is not None and child.returncode is None:
                try:
                    settle(True)
                except BaseException as cleanup:
                    receipt['cleanup'] = 'unproved; private root retained'
                    receipt['cleanup_error'] = repr(cleanup)
            raise
        finally:
            persist()

    def exposure():
        check_cancelled()
        result = snapshot()
        check_cancelled()
        return result

    try:
        head = command([real_git, '-C', source, 'rev-parse', 'HEAD']).stdout.decode().strip()
        if head != args.pin:
            raise RuntimeError('source pin mismatch: ' + head)
        if command([real_git, '-C', source, 'status', '--porcelain']).stdout:
            raise RuntimeError('source checkout must be clean')
        shim_source = source / 'scripts/test-git-endpoint.py'
        evidence['shim_source_sha256'] = digest(shim_source)
        evidence['git_version'] = command([real_git, '--version']).stdout.decode().strip()
        evidence['admission_exposure'] = exposure()
        bare, direct, mapped, shimdir = (run / name for name in ('remote.git', 'direct', 'mapped', 'shim'))
        command([real_git, 'init', '--bare', bare])
        logical = 'https://github.com/sh797-fixture/measurement.git'
        for repo, origin in [(direct, str(bare)), (mapped, logical)]:
            command([real_git, 'init', repo])
            command([real_git, '-C', repo, 'remote', 'add', 'origin', origin])
        command([sys.executable, '-B', shim_source, shimdir, json.dumps({logical: str(bare)})])
        shim = shimdir / 'git'
        evidence['generated_shim_sha256'] = digest(shim)
        # Fixed argv apart from endpoint mapping and the private equivalent cwd.
        cases = [
            ('metadata', [real_git, 'rev-parse', '--git-dir'], [shim, 'rev-parse', '--git-dir']),
            ('mapped_explicit', [real_git, 'ls-remote', bare], [shim, 'ls-remote', logical]),
            ('mapped_origin', [real_git, 'ls-remote', 'origin'], [shim, 'ls-remote', 'origin']),
        ]
        for name, direct_argv, shim_argv in cases:
            context = {'phase': 'warmup', 'case': name}
            for _ in range(4):
                a = command(direct_argv, direct); b = command(shim_argv, mapped)
                if a.stdout != b.stdout:
                    raise RuntimeError('fixture outputs differ for ' + name)
            for pair in range(args.samples):
                order = [('direct', direct_argv, direct), ('shim', shim_argv, mapped)]
                if pair % 2:
                    order.reverse()
                rows = []
                for arm, argv, cwd in order:
                    context = {'phase': 'timed', 'case': name, 'pair': pair, 'arm': arm}
                    row = timed_operation(evidence, name, pair, arm, argv, cwd,
                                          command, exposure, persist)
                    rows.append(row)
                if rows[0]['stdout_sha256'] != rows[1]['stdout_sha256']:
                    raise RuntimeError('paired outputs differ')
        summary = {}
        for name, _, _ in cases:
            rows = [r for r in evidence['samples'] if r['case'] == name]
            paired = {i: {} for i in range(args.samples)}
            for r in rows:
                paired[r['pair']][r['arm']] = r['elapsed_ns']
            deltas = [r['shim'] - r['direct'] for r in paired.values()]
            summary[name] = {'median_added_ns': statistics.median(deltas),
                             'min_added_ns': min(deltas), 'max_added_ns': max(deltas),
                             'pairs': len(deltas)}
        check_cancelled()
        if digest(real_git) != evidence['git_sha256'] or digest(Path(sys.executable).resolve()) != evidence['python_sha256']:
            raise RuntimeError('Git or Python bytes changed during measurement')
        if digest(shim_source) != evidence['shim_source_sha256'] or digest(shim) != evidence['generated_shim_sha256']:
            raise RuntimeError('shim source or generated bytes changed during measurement')
        context = {'phase': 'postflight'}
        if command([real_git, '-C', source, 'rev-parse', 'HEAD']).stdout.decode().strip() != args.pin:
            raise RuntimeError('source head changed during measurement')
        if command([real_git, '-C', source, 'status', '--porcelain']).stdout:
            raise RuntimeError('source checkout changed during measurement')
        check_cancelled()
        evidence.update(status='complete_local_shim_measurement', summary=summary,
                        inference='Descriptive paired local overhead only. Production dispatch and historical slowdown remain inconclusive.')
    except BaseException as error:
        evidence['error'] = str(error)
        raise
    finally:
        if cancelled:
            evidence['status'] = 'cancelled_incomplete'
            evidence['signals'] = cancelled
        evidence['total_elapsed_seconds'] = time.monotonic() - campaign_started
        evidence['deadline_exceeded_including_cleanup'] = time.monotonic() >= campaign_end
        persist()
        print(run / 'result.json')
        for signum, handler in prior_handlers.items():
            signal.signal(signum, handler)
        # Preserve fixture and evidence. No daemon, provider, tmux or deletion.


if __name__ == '__main__':
    main()
