#!/usr/bin/env python3
"""Verifier-owned gate/QoS experiments, without certification or landing."""

import datetime
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

sys.dont_write_bytecode = True

from gate_measurement_context import manifest, validate
from gate_measurement_data import IdleWindow, parse_wall, schedule, summarize
from gate_measurement_runtime import capture, execution_active, journal, mirror_progress, normal_class, pending_sample, pressure, records, require_resource_limits, scheduling
from gate_measurement_setup import immutable, prepare, tools_identity
from verifier_state import Refusal, atomic, paths, read, save
from gate_measurement_bounds import Deadline, LIMITS, require_same_day, start_slot, validate_policy
from gate_measurement_storage import check_storage, pressure_level
from gate_measurement_inputs import inventory, observe

SCRIPTS = Path(__file__).resolve().parent


def today():
    """Use one recorded local calendar day for each accepted cohort."""
    return datetime.datetime.now().astimezone().date().isoformat()


def resume_index(cohort, events, day):
    """Resume only a valid complete prefix; never repair evidence by omission."""
    if cohort['day'] != day:
        raise Refusal('local day changed; retain this cohort and start a new one')
    if pending_sample(events):
        raise Refusal('interrupted sample retained; this cohort cannot be resumed')
    summary = summarize(cohort, events)
    attempted = sum(summary[c]['attempted'] for c in ('control', 'utility'))
    valid = sum(summary[c]['valid'] for c in ('control', 'utility'))
    if attempted != valid:
        raise Refusal('invalid or failed samples retained; this cohort cannot be resumed')
    return attempted


def progress(event):
    """Advance the existing watchdog only on concrete measurement observations."""
    journal(os.environ['STORYHOOK_GATE_PROGRESS'], event)


def idle(directory, cohort):
    """Wait for the declared continuous idle interval and keep sensor evidence."""
    window = IdleWindow()
    limit = Deadline(LIMITS['quiet_wait_seconds'], end=float(os.environ.get('STORYHOOK_MEASUREMENT_END', 'inf')))
    while True:
        limit.require('quiet admission')
        if today() != cohort['day']:
            raise Refusal('local day changed during idle admission')
        observed = pressure()
        journal(directory / 'pressure.jsonl', dict(observed, kind='idle'))
        progress({'kind': 'activity', 'path': 'measurement/idle', 'status': 'running',
                  'label': f"observed load/core {observed['load'][0] / observed['cores']:.3f}", 'at': observed['at']})
        if window.observe(time.monotonic(), observed['load'][0], observed['cores']):
            return observed
        time.sleep(min(LIMITS['sample_seconds'], limit.remaining()))


def gate_exec(path, condition, directory):
    """Record the actual class and timed command exit inside the supervised gate."""
    identity = validate(path)
    limits = require_resource_limits(identity)
    locale = capture(['locale'])
    if identity.get('tools', {}).get('locale') != locale:
        raise Refusal('measurement locale changed or is missing')
    observed = scheduling()
    valid = normal_class(observed) if condition == 'control' else observed['qos'] == 0x11 and observed['darwin_background'] == 0
    if not valid:
        raise Refusal(f'{condition} gate has the wrong scheduling class: {observed}')
    directory = Path(directory)
    save(directory / 'class.json', dict(observed, resource_limits=limits, locale=locale, version=1))
    # macOS time writes its measurement to stderr. Keep it separate from the
    # gate log without depending on GNU-only -o syntax.
    with (directory / 'time.txt').open('x') as timing, (directory / 'command.log').open('x') as log:
        command = ['/usr/bin/time', '-p', 'sh', '-c', 'exec "$@" >"$STORYHOOK_MEASUREMENT_COMMAND_LOG" 2>&1', 'measurement', *identity['gate']['argv']]
        env = dict(os.environ, STORYHOOK_MEASUREMENT_COMMAND_LOG=str(directory / 'command.log'))
        result = subprocess.run(command, stdout=log, stderr=timing, env=env)
    save(directory / 'exit.json', {'version': 1, 'exit_code': result.returncode})
    return result.returncode


def run_sample(identity, cohort, index, directory, probes, warmup=False):
    """Supervise one real gate and prove probes overlapped active test execution."""
    limit = Deadline(LIMITS['gate_seconds'], end=float(os.environ.get('STORYHOOK_MEASUREMENT_END', 'inf')))
    if float(os.environ.get('STORYHOOK_MEASUREMENT_END', 'inf')) - time.monotonic() < LIMITS['gate_seconds']:
        raise Refusal('gate admission exceeds remaining campaign allowance')
    require_same_day(cohort['day'], LIMITS['gate_seconds'])
    def observe_inputs():
        return observe(identity, identity['input_inventory'], os.environ,
                       identity['storage']['targets'][0], limit,
                       validate_source=lambda: validate(os.environ['STORYHOOK_GATE_MEASUREMENT']),
                       versions=tools_identity,
                       resolve_inventory=lambda: inventory(identity['worktree'], os.environ))
    inputs_before = observe_inputs()
    if 'storage' in identity:
        check_storage(identity['storage'])
        pressure_level()
    directory.mkdir()
    ledger = Path(os.environ['STORYHOOK_GATE_MEASUREMENT']).parent / 'gates.jsonl'
    slot = start_slot(records(ledger), 'warmup' if warmup else 'sample', index)
    journal(ledger, slot)
    condition = 'control' if warmup else schedule(cohort['pairs'])[index]
    path = os.environ['STORYHOOK_GATE_MEASUREMENT']
    command = [sys.executable, '-B', str(SCRIPTS / 'verifier-owner.py'), 'measurement-gate',
               identity['common'], identity['worktree'], '--', condition, sys.executable, '-B',
               str(SCRIPTS / 'gate_measurement.py'), 'gate-exec', path, condition, str(directory)]
    gate_progress = directory / 'progress.jsonl'
    # The launch supervisor records cost context before the gate can run.
    gate_progress.touch(mode=0o600, exist_ok=False)
    env = dict(os.environ, STORYHOOK_GATE_PROGRESS=str(gate_progress),
               STORYHOOK_MEASUREMENT_GATE_DEADLINE=str(limit.end))
    measured = []
    mirrored = 0
    last_pressure = 0
    with (directory / 'supervisor.log').open('x') as log:
        process = subprocess.Popen(command, env=env, stdout=log, stderr=subprocess.STDOUT)
        try:
            while process.poll() is None:
                limit.require('measurement gate')
                # Mirror complete real progress events to the outer gate lock's
                # append-only watchdog journal. Stdout is never a heartbeat.
                mirrored = mirror_progress(gate_progress, os.environ['STORYHOOK_GATE_PROGRESS'], mirrored)
                if not warmup and len(measured) < 2 and execution_active(gate_progress):
                    name = ('list', 'hook')[len(measured)]
                    result = probes.run(name, directory / name)
                    result['overlap'] = process.poll() is None and execution_active(gate_progress)
                    measured.append(result)
                    journal(directory / 'probes.jsonl', result)
                now = time.monotonic()
                if now - last_pressure >= LIMITS['sample_seconds']:
                    observed = pressure()
                    from gate_measurement_campaign import competing_work
                    observed['competing_pids'] = competing_work(observed['processes'], os.getpid())
                    if 'storage' in identity:
                        observed['storage'] = check_storage(identity['storage'])
                        observed['native_memory_pressure'] = pressure_level()
                    journal(directory / 'pressure.jsonl', dict(observed, kind='running'))
                    if observed['competing_pids']:
                        raise Refusal('competing external work invalidates the scheduling sample')
                    last_pressure = now
                time.sleep(.5)
        except BaseException:
            # Delegate descendant cleanup to the same supervisor used by normal
            # verification. Retain the started journal event on interruption.
            if process.poll() is None:
                process.send_signal(signal.SIGTERM)
            try:
                process.wait(timeout=LIMITS['cleanup_seconds'])
            except subprocess.TimeoutExpired as error:
                journal(ledger, {'kind': 'cleanup-excess', 'slot': slot['slot'], 'detail': str(error)})
                # The outer verifier retains custody and drains this session.
                # Never clear the durable start or admit another sample here.
                raise Refusal('measurement cleanup exceeded its observation allowance; owner retained') from error
            raise
    _, _, key = paths(identity['common'], identity['worktree'])
    owner = read(str(key) + '.owner')
    cleanup = 'complete' if owner and owner.get('gate_started') is False and owner.get('gate_session') is None else 'unknown'
    elapsed = time.monotonic() - limit.started
    if cleanup == 'complete':
        journal(ledger, {'kind': 'gate-settled', 'slot': slot['slot'], 'seconds': elapsed,
                         'production_target_breach': elapsed >= LIMITS['production_target_seconds']})
    exit_record = read(directory / 'exit.json')
    if exit_record is None:
        raise Refusal(f'gate has no observed command exit; supervisor={process.returncode}; evidence={directory}')
    wall = parse_wall((directory / 'time.txt').read_text())
    reasons = []
    if process.returncode != exit_record['exit_code'] or cleanup != 'complete':
        reasons.append('supervision or cleanup failed')
    if today() != cohort['day']:
        reasons.append('gate crossed the local day boundary')
    if not warmup and (len(measured) != 2 or not all(p['ok'] and p['overlap'] for p in measured)):
        reasons.append('interactive probes failed or did not overlap active tests')
    validate(path)
    if observe_inputs() != inputs_before:
        reasons.append('measurement inputs changed during execution')
    return {'kind': 'warmup' if warmup else 'sample', 'index': index, 'condition': condition,
            'tree': cohort['tree'], 'day': cohort['day'], 'wall_seconds': wall,
            'admission_to_settlement_seconds': elapsed,
            'production_target_breach': elapsed >= LIMITS['production_target_seconds'],
            'exit_code': exit_record['exit_code'], 'supervisor_exit': process.returncode,
            'cleanup': cleanup, 'valid': not reasons, 'reasons': reasons, 'probes': measured,
            'class': read(directory / 'class.json')}


def report(directory, cohort):
    """Publish derived statistics while retaining all attempted observations."""
    result = summarize(cohort, records(directory / 'samples.jsonl'))
    result['samples_complete'] = result['complete']
    result['probe_cleanup'] = read(directory / 'cleanup.json')
    result['complete'] = result['samples_complete'] and (result['probe_cleanup'] or {}).get('ok') is True
    save(directory / 'summary.json', result)
    lines = ['# Verifier scheduling measurement', '', f"Tree: `{cohort['tree']}`. Local date: {cohort['day']}.", '',
             f"Complete: {result['complete']}. Warmups excluded. No certification or merge authority.", '',
             '| Condition | Attempts | Valid | Failed | Unresolved | Gate median/min/max (s) | List median (s) | Hook median (s) |',
             '|---|---:|---:|---:|---:|---|---:|---:|']
    for name in ('control', 'utility'):
        row = result[name]
        gate, probes = row['gate'], row['probes']
        timing = '/'.join(f'{gate[k]:.3f}' for k in ('median', 'min', 'max')) if gate else 'unavailable'
        lines.append(f"| {name} | {row['attempted']} | {row['valid']} | {row['failed']} | {row['unresolved']} | {timing} | {probes['list'] if probes else 'unavailable'} | {probes['hook'] if probes else 'unavailable'} |")
    lines += ['', f"Gate median change: {result['median_change_percent']} percent.", '',
              'Admission required load/core < 0.5 continuously for 60 seconds. This is not host-wide exclusion.',
              'Raw pressure and process observations accompany each attempt. Linux and Xcode inheritance are unmeasured.',
              'Failures describe this matched experiment only; unmatched historical logs do not establish a causal red-rate change.', '']
    atomic(directory / 'report.md', '\n'.join(lines).encode())
    return result


def owned(path):
    """Collect a fixed cohort only while the project gate and workspace are owned."""
    identity = manifest(path)
    validate_policy(identity)
    if 'storage' not in identity:
        raise Refusal('measurement storage policy is missing')
    check_storage(identity['storage'], initial=True)
    pressure_level()
    if 'STORYHOOK_MEASUREMENT_END' not in os.environ:
        raise Refusal('bounded campaign deadline is missing')
    Deadline(LIMITS['campaign_seconds'], end=float(os.environ['STORYHOOK_MEASUREMENT_END'])).require('campaign')
    output = Path(path).parent
    worktree = Path(identity['worktree'])
    if not worktree.exists():
        capture(['git', '-C', identity['source'], 'worktree', 'add', '--detach', str(worktree), identity['commit']])
    os.chdir(worktree)
    validate(path)
    capture(['bash', 'scripts/managed-cargo.sh', 'fetch', '--locked', '--offline'])
    identity['input_inventory'] = inventory(worktree, os.environ)
    immutable(output / 'inputs.json', identity['input_inventory'])
    require_resource_limits(identity)
    if tools_identity() != identity['tools'] or not normal_class(scheduling()):
        raise Refusal('toolchain or collector class changed before collection')
    day = today()
    immutable(output / 'day.json', {'version': 1, 'day': day})
    directory = output / 'cohorts' / day
    directory.mkdir(parents=True, exist_ok=True)
    cohort = {'version': 1, 'day': day, 'tree': identity['tree'], 'pairs': 10}
    immutable(directory / 'cohort.json', cohort)
    sample_log = directory / 'samples.jsonl'
    index = resume_index(cohort, records(sample_log), day)
    from gate_measurement_probes import Probes
    probes = Probes(output, identity)
    save(directory / 'cleanup.json', {'version': 1, 'ok': False, 'stage': 'running'})
    try:
        probes.start()
        warmup = read(directory / 'warmup.json')
        if warmup is None:
            idle(directory, cohort)
            print('measurement: warmup starting', flush=True)
            warmup = run_sample(identity, cohort, -1, directory / 'warmup', probes, warmup=True)
            save(directory / 'warmup.json', dict(warmup, version=1))
        if not warmup['valid'] or warmup['exit_code']:
            raise Refusal('warmup did not complete successfully; retain its evidence')
        for index in range(index, len(schedule())):
            require_resource_limits(identity)
            if tools_identity() != identity['tools']:
                raise Refusal('toolchain changed during collection')
            observed = idle(directory, cohort)
            journal(sample_log, {'kind': 'start', 'index': index, 'at': observed['at'], 'condition': schedule()[index]})
            print(f'measurement: sample {index + 1}/20 {schedule()[index]} starting', flush=True)
            try:
                sample = run_sample(identity, cohort, index, directory / f'sample-{index:02}', probes)
            except BaseException as error:
                journal(sample_log, {'kind': 'interruption', 'index': index, 'detail': str(error)})
                raise
            journal(sample_log, sample)
            report(directory, cohort)
            print(f"measurement: sample {index + 1}/20 finished in {sample['wall_seconds']}s, exit {sample['exit_code']}", flush=True)
            if not sample['valid'] or sample['exit_code']:
                raise Refusal(f'invalid or failed sample retained: {sample}')
    finally:
        cleanup = {'version': 1, 'ok': False, 'stage': 'cleanup'}
        try:
            probes.stop()
            cleanup['ok'] = True
        except BaseException as error:
            cleanup['error'] = str(error)
            raise
        finally:
            save(directory / 'cleanup.json', cleanup)
            report(directory, cohort)
    return 0


def main():
    """Execute an explicitly owned measurement operation."""
    try:
        mode, *args = sys.argv[1:]
        if mode == 'prepare' and len(args) == 4:
            prepare(*args)
        elif mode == 'owned' and len(args) == 1:
            def interrupted(signum, _frame):
                raise InterruptedError(f'measurement interrupted by signal {signum}')
            for signum in (signal.SIGHUP, signal.SIGINT, signal.SIGTERM):
                signal.signal(signum, interrupted)
            return owned(*args)
        elif mode == 'gate-exec' and len(args) == 3:
            return gate_exec(*args)
        else:
            raise Refusal('invalid internal measurement invocation')
    except (Refusal, OSError, ValueError, subprocess.SubprocessError) as error:
        print(f'gate measurement: {error}', file=sys.stderr, flush=True)
        return 2


if __name__ == '__main__':
    sys.exit(main())
