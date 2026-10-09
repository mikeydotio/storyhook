"""Pin measurement inputs before entering the existing verifier ownership stack."""

import fcntl
import json
import os
from pathlib import Path
import shutil
import stat
import sys
import time

from gate_measurement_runtime import capture, normal_class, resource_limits, scheduling, sha256
from gate_measurement_context import VARIABLE
from verifier_state import Refusal, boot, read, save
from gate_measurement_bounds import LIMITS, Deadline
from gate_measurement_storage import reserve_description, check_storage, pressure_level

SCRIPTS = Path(__file__).resolve().parent


def tools_identity():
    """Record the actual compiler/runtime identities used by this experiment."""
    return {name: capture(command) for name, command in {
        'os': ['sw_vers'], 'rustc': ['rustc', '-vV'], 'cargo': ['cargo', '-V'],
        'python': [sys.executable, '--version'], 'git': ['git', '--version'],
        'make': ['make', '--version'], 'node': ['node', '--version'], 'locale': ['locale'],
    }.items()}


def immutable(path, expected):
    """Create an identity record once, or require an identical restart."""
    existing = read(path)
    if existing is None:
        save(path, expected)
    elif existing != expected:
        raise Refusal(f'measurement identity changed: {path}')


def prepare_progress(path):
    """Create the lock waiter's journal without replacing retained evidence."""
    fd = os.open(path, os.O_WRONLY | os.O_APPEND | os.O_CREAT | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600)
    try:
        if not stat.S_ISREG(os.fstat(fd).st_mode):
            raise Refusal(f'measurement progress is not a regular file: {path}')
        os.fsync(fd)
    finally:
        os.close(fd)


def prepare(checkout, revision, output, binary):
    """Enter project exclusion, then workspace ownership; never run a gate here."""
    checkout, output, binary = [Path(p).resolve(strict=True) for p in (checkout, output, binary)]
    if not normal_class(scheduling()):
        raise Refusal('measurement collector is already utility/background/niced; control would be invalid')
    # This descriptor survives exec and all owned participants. A second
    # collector cannot rewrite a manifest while the first owns its output.
    fd = os.open(output / 'collection.lock', os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    os.set_inheritable(fd, True)
    lifetime = output / 'lifetime.json'
    state = read(lifetime)
    if state is None:
        state = {'version': 1, 'boot': boot(), 'started': time.monotonic()}
        save(lifetime, state)
    if state.get('boot') != boot():
        raise Refusal('measurement campaign cannot resume across a reboot')
    end = state['started'] + LIMITS['campaign_seconds']
    preparation_end = min(state['started'] + LIMITS['preparation_seconds'], end)
    preparation = Deadline(LIMITS['preparation_seconds'], end=preparation_end)
    preparation.require('measurement preparation')
    os.environ['STORYHOOK_MEASUREMENT_END'] = str(preparation.end)
    os.environ['STORYHOOK_MEASUREMENT_OPERATIONS'] = str(output / 'operations')
    git = lambda *args: capture(['git', '-C', str(checkout), *args])
    if git('status', '--porcelain', '--untracked-files=no'):
        raise Refusal('measurement source has tracked changes; commit the approved work first')
    commit = git('rev-parse', '--verify', revision + '^{commit}')
    tree = git('rev-parse', commit + '^{tree}')
    common = str((checkout / git('rev-parse', '--git-common-dir')).resolve(strict=True))
    gate = json.loads(capture([str(binary), 'verifier', 'gate-config', str(checkout), commit, commit, tree, '--json']))
    if gate.get('result') != 'gate-ready':
        raise Refusal(f'gate configuration is not executable: {gate!r}')
    # Older trees cannot promise no certification or verdict reuse. Do not
    # run them even though their gate command would otherwise be executable.
    for name in ('leg.sh', 'gate-receipt.sh', 'tree-receipt.sh'):
        body = git('show', f'{commit}:scripts/{name}')
        if 'gate-measurement-context.sh' not in body:
            raise Refusal(f'pinned tree lacks the measurement boundary in {name}')
    probe = output / 'probe'
    (probe / 'bin').mkdir(parents=True, exist_ok=True)
    lease = probe / 'bin/story'
    digest = sha256(binary)
    if lease.exists():
        if lease.is_symlink() or sha256(lease) != digest:
            raise Refusal('leased probe binary differs from the requested binary')
    else:
        with binary.open('rb') as source, lease.open('xb') as dest:
            shutil.copyfileobj(source, dest)
            dest.flush()
            os.fsync(dest.fileno())
        lease.chmod(0o700)
    old = read(output / 'manifest.json')
    storage = old.get('storage') if old else reserve_description(output)
    check_storage(storage, initial=True)
    pressure_level()
    os.environ['CARGO_TARGET_DIR'] = storage['targets'][0]['path']
    from gate_measurement_inputs import WORKERS
    os.environ['CARGO_NET_OFFLINE'] = 'true'
    for name in WORKERS:
        os.environ[name] = '1'
    identity = {'version': 1, 'kind': 'gate-class-measurement', 'commit': commit, 'tree': tree,
                'common': common, 'worktree': str(output / 'worktree'), 'source': str(checkout),
                'gate': gate, 'binary_sha256': digest, 'tools': tools_identity(),
                'applicable_legs': ['fmt', 'clippy', 'rust-suite', 'rust-contracts', 'build', 'plugin'],
                'resource_limits': resource_limits(), 'limits': LIMITS, 'storage': storage,
                'preparation_end': preparation_end,
                'fixture': {'prefix': 'MB', 'stories': 10, 'title': 'Measurement fixture'},
                'protocol': {'pairs': 10, 'idle_seconds': 60, 'max_load_per_core': 0.5}}
    path = output / 'manifest.json'
    immutable(path, identity)
    # The lock records its wait before the collector can append any activity.
    prepare_progress(output / 'progress.jsonl')
    os.environ[VARIABLE] = str(path)
    os.environ['STORYHOOK_GATE_PROGRESS'] = str(output / 'progress.jsonl')
    os.environ['STORYHOOK_VERIFIER_CLEANUP_GRACE_MS'] = '30000'
    preparation.require('measurement preparation')
    os.environ['STORYHOOK_MEASUREMENT_END'] = str(end)
    os.chdir(checkout)
    command = ['bash', str(SCRIPTS / 'machine-lock.sh'), '--max-wait', str(LIMITS['lock_wait_seconds']), '--termination-grace', '22', 'gate', '--',
               sys.executable, '-B', str(SCRIPTS / 'verifier-owner.py'), 'measurement-run', common,
               identity['worktree'], '--', sys.executable, '-B', str(SCRIPTS / 'gate_measurement.py'), 'owned', str(path)]
    os.execvpe(command[0], command, os.environ)
