"""Bridge SH-872 slots to owned gates and their actual execution evidence.

The campaign caller supplies a complete identity observer. There is deliberately
no command-line entry until configuration capture and target turnover are ready.
No result here is a production receipt or permission to merge.
"""

import os
from pathlib import Path
import signal
import subprocess
import sys
import time

from gate_measurement_cohorts import fingerprint
from gate_measurement_context import validate
from gate_measurement_runtime import journal, mirror_progress, records
from verifier_result import EXECUTION_FILE, execution
from verifier_state import Refusal, paths, read, save


def coverage(rows, expected, mode):
    """Require each applicable leg exactly once; an exit-zero log is insufficient."""
    states = {}
    executed, reused = [], []
    for row in rows:
        if row.get('kind') != 'item':
            continue
        path = row.get('path', '')
        if not isinstance(path, str) or not path.startswith('release gate/'):
            continue
        leg = path.removeprefix('release gate/')
        # Nested checklist entries do not stand for a top-level gate detector.
        if '/' in leg:
            continue
        status = row.get('status')
        if leg not in expected:
            if status == 'skipped':
                continue
            raise Refusal('measurement ran an unplanned gate leg: ' + leg)
        if mode == 'reuse':
            if status != 'reused' or leg in states:
                raise Refusal('reuse observation executed, duplicated or skipped a detector')
            states[leg] = 'reused'
            reused.append(leg)
        elif status == 'running' and leg not in states:
            states[leg] = 'running'
        elif status == 'passed' and states.get(leg) == 'running':
            states[leg] = 'passed'
            executed.append(leg)
        else:
            raise Refusal('measurement detector did not execute once and pass: ' + leg)
    if set(states) != set(expected) or any(s not in ('passed', 'reused') for s in states.values()):
        raise Refusal('measurement has incomplete detector evidence')
    return ([leg for leg in expected if leg in executed],
            [leg for leg in expected if leg in reused])


def run_observation(cohort, *, observe, launch, remaining_window,
                    remaining_campaign, clock=time.monotonic):
    """Consume a slot before launch; never turn an interrupted attempt into a retry.

    `launch` must return the owned supervisor's exit, settlement and progress.
    The native implementation below uses verifier-owner's execution channel.
    Exceptions leave the durable start pending and append a diagnostic; they
    never manufacture an exit or a successful cleanup observation.
    """
    started = clock()
    before = observe()
    key = fingerprint(before)
    captured = clock() - started
    if captured < 0:
        raise Refusal('measurement clock moved backwards during input capture')
    slot = cohort.begin(before, remaining_window=remaining_window - captured,
                        remaining_campaign=remaining_campaign - captured)
    attempt = cohort.root / f"slot-{slot['slot']:02}"
    try:
        attempt.mkdir(mode=0o700)  # retained/foreign attempts cannot be replaced
        result = launch(slot, attempt, started + slot['ceiling_seconds'])
        if (not isinstance(result, dict) or type(result.get('exit_code')) is not int
                or type(result.get('settled')) is not bool):
            raise Refusal('owned gate returned no exact exit/settlement observation')
        if fingerprint(observe()) != key:
            raise Refusal('measurement inputs changed during gate execution')
        executed, reused = coverage(result['progress'], before['applicable_legs'], slot['mode'])
        return cohort.finish(exit_code=result['exit_code'], settled=result['settled'],
                             executed=executed, reused=reused, elapsed=clock() - started)
    except BaseException as error:
        journal(cohort.root / 'failures.jsonl', {
            'kind': 'interruption', 'slot': slot['slot'],
            'error_type': type(error).__name__, 'detail': str(error),
            'production_certification': False,
        })
        raise


class OwnedGate:
    """Use current verifier admission, session custody and bounded cancellation.

    `gate_command` is the prepared measurement gate entry. It must equal the
    command pinned in the owner-bound manifest; arbitrary commands are refused.
    Native invocation is not a supported campaign entry point yet.
    """

    def __init__(self, manifest_path, gate_command, *, health):
        self.manifest_path = manifest_path
        self.gate_command = gate_command
        self.health = health

    def __call__(self, slot, directory, deadline):
        identity = validate(self.manifest_path)
        if (identity.get('kind') != 'gate-throughput-measurement'
                or self.gate_command != identity.get('gate', {}).get('argv')
                or slot['identity']['source_commit'] != identity['commit']
                or slot['identity']['source_tree'] != identity['tree']):
            raise Refusal('owned throughput gate does not match the pinned manifest')
        self.health()
        directory = Path(directory)
        if directory.resolve() != directory:
            raise Refusal('measurement attempt directory was substituted')
        manifest_root = Path(identity['campaign_root'])
        if directory.parent.parent.parent != manifest_root or directory.parent.parent.name != 'measurement-results-v1':
            raise Refusal('measurement attempt is outside its owned result namespace')
        progress_path = directory / 'progress.jsonl'
        progress_path.touch(mode=0o600, exist_ok=False)
        result_path = directory / 'execution.json'
        if result_path.exists():
            raise Refusal('measurement execution record already exists')
        nonce = os.environ.get('STORYHOOK_VERIFIER_OWNER')
        if not nonce:
            raise Refusal('measurement owner identity is missing')
        save(result_path, {'version': 1, 'owner': nonce, 'attempt': result_path.name,
                           'state': 'pending', 'tree': identity['tree'],
                           'base': identity['commit'], 'head': identity['commit']})
        outer_progress = os.environ['STORYHOOK_GATE_PROGRESS']
        env = dict(os.environ, STORYHOOK_GATE_PROGRESS=str(progress_path),
                   STORYHOOK_MEASUREMENT_GATE_DEADLINE=str(deadline))
        env[EXECUTION_FILE] = str(result_path)
        # The result path is bound to the current slot; the leg adapter must
        # validate that slot before deciding whether any verdict can be reused.
        env['STORYHOOK_MEASUREMENT_SLOT'] = str(directory)
        command = [sys.executable, '-B', str(Path(__file__).with_name('verifier-owner.py')),
                   'measurement-gate', identity['common'], identity['worktree'], '--',
                   'control', *self.gate_command]
        offset = 0
        last_health = time.monotonic()
        with (directory / 'supervisor.log').open('x') as log:
            process = subprocess.Popen(command, env=env, stdout=log, stderr=subprocess.STDOUT)
            try:
                while process.poll() is None:
                    if time.monotonic() >= deadline:
                        raise Refusal('measurement gate exhausted its allowance')
                    now = time.monotonic()
                    if now - last_health >= 5:
                        self.health()
                        last_health = now
                    offset = mirror_progress(progress_path, outer_progress, offset)
                    time.sleep(.5)
            except BaseException:
                if process.poll() is None:
                    process.send_signal(signal.SIGTERM)
                try:
                    process.wait(timeout=35)
                except subprocess.TimeoutExpired as error:
                    raise Refusal('gate cleanup observation expired; owner and evidence retained') from error
                raise
        mirror_progress(progress_path, outer_progress, offset)
        result = execution(result_path)
        if (result.get('state') != 'completed' or type(result.get('exit_status')) is not int
                or not 0 <= result['exit_status'] < 128
                or result['exit_status'] != process.returncode):
            raise Refusal('gate was refused, interrupted or has mismatched execution evidence')
        _, _, key = paths(identity['common'], identity['worktree'])
        owner = read(str(key) + '.owner')
        settled = bool(owner and owner.get('gate_started') is False
                       and owner.get('gate_session') is None)
        validate(self.manifest_path)
        return {'exit_code': result['exit_status'], 'settled': settled,
                'progress': records(progress_path)}
