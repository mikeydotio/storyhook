"""Per-leg execution/reuse records inside a live SH-872 measurement owner."""

import hashlib
import os
from pathlib import Path
import sys

from gate_measurement_cohorts import Cohort, canonical
from gate_measurement_context import validate, VARIABLE
from gate_measurement_runtime import journal, records
from verifier_state import Refusal

# These name the enclosing attempt's telemetry or authority, not detector input.
# Every other inherited variable is hashed, including unknown variables. A
# differing incidental value conservatively refuses R rather than weakening it.
ATTEMPT_ENV = {
    'STORYHOOK_MEASUREMENT_SLOT', 'STORYHOOK_GATE_PROGRESS',
    'STORYHOOK_GATE_EXECUTION_FILE', 'STORYHOOK_MEASUREMENT_GATE_DEADLINE',
    '_',
}


def current_slot(identity, manifest_path, directory):
    directory = Path(directory)
    root = Path(identity.get('campaign_root', Path(manifest_path).parent))
    if (not directory.is_absolute() or directory.resolve() != directory
            or directory.parent.parent != root / 'measurement-results-v1'
            or identity.get('kind') != 'gate-throughput-measurement'):
        raise Refusal('measurement slot is not in its live manifest namespace')
    cohort = Cohort(root, directory.parent.name)
    _, pending, _ = cohort.history()
    if (pending is None or directory.name != f"slot-{pending['slot']:02}"
            or pending['identity']['source_commit'] != identity['commit']
            or pending['identity']['source_tree'] != identity['tree']
            or pending['identity']['gate_argv'] != identity['gate']['argv']):
        raise Refusal('measurement slot does not match its current source/gate identity')
    return cohort, pending


def command_key(slot, leg, argv, env):
    if leg not in slot['identity']['applicable_legs'] or not argv:
        raise Refusal('measurement command is not an applicable detector')
    if any(not isinstance(arg, str) or '\x00' in arg for arg in argv):
        raise Refusal('measurement detector has invalid arguments')
    inputs = {'identity': slot['key'], 'leg': leg, 'argv': argv,
              'environment': {k: v for k, v in env.items() if k not in ATTEMPT_ENV}}
    return hashlib.sha256(canonical(inputs).encode()).hexdigest()


def prepare(cohort, slot, directory, leg, argv, env):
    """Run C/W even if ordinary receipts exist; R needs this exact W command."""
    key = command_key(slot, leg, argv, env)
    path = Path(directory) / 'legs.jsonl'
    if any(row.get('leg') == leg for row in records(path)):
        raise Refusal('measurement leg has already started; no retry is allowed')
    if slot['mode'] == 'reuse':
        cohort.reuse_key(slot['identity'], leg)
        warm = cohort.root / f"slot-{slot['slot'] - 1:02}" / 'legs.jsonl'
        evidence = [row for row in records(warm) if row.get('leg') == leg]
        if (len(evidence) != 2 or evidence[0].get('kind') != 'start'
                or evidence[1].get('kind') != 'finish'
                or any(row.get('key') != key for row in evidence)
                or type(evidence[1].get('exit_code')) is not int
                or evidence[1]['exit_code'] != 0):
            raise Refusal('warm command/environment evidence is missing, changed or failed')
        journal(path, {'kind': 'reused', 'leg': leg, 'key': key})
        return 'reused'
    journal(path, {'kind': 'start', 'leg': leg, 'key': key})
    return 'run'


def finish(slot, directory, leg, argv, env, exit_code):
    key = command_key(slot, leg, argv, env)
    path = Path(directory) / 'legs.jsonl'
    evidence = [row for row in records(path) if row.get('leg') == leg]
    if (slot['mode'] == 'reuse' or len(evidence) != 1
            or evidence[0].get('kind') != 'start' or evidence[0].get('key') != key
            or type(exit_code) is not int or not 0 <= exit_code <= 255):
        raise Refusal('measurement leg has no identical unfinished execution')
    journal(path, {'kind': 'finish', 'leg': leg, 'key': key, 'exit_code': exit_code})


def main():
    try:
        operation, leg, *argv = sys.argv[1:]
        path = os.environ[VARIABLE]
        identity = validate(path)
        directory = os.environ['STORYHOOK_MEASUREMENT_SLOT']
        cohort, slot = current_slot(identity, path, directory)
        if operation == 'prepare':
            print(prepare(cohort, slot, directory, leg, argv, os.environ))
        elif operation == 'finish' and argv:
            status, *argv = argv
            finish(slot, directory, leg, argv, os.environ, int(status))
        else:
            raise Refusal('invalid measurement leg operation')
    except (Refusal, OSError, ValueError, KeyError) as error:
        print(f'measurement leg: {error}', file=sys.stderr)
        return 2
    return 0


if __name__ == '__main__':
    sys.exit(main())
