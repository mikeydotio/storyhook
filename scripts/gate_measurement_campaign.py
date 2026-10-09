"""Bounded SH-872 baseline/optimization campaign; explicit operator start only.

prepare ROOT CHECKOUT COMMIT BINARY baseline|optimization START_RECEIPT
The start receipt is local coordination evidence, not an inferred authorization.
This entry does not run from ordinary gates, the daemon, or project automation.
"""

import fcntl
import hashlib
import json
import os
from pathlib import Path
import signal
import sys
import time

from gate_measurement_bounds import Deadline
from gate_measurement_cohorts import Cohort, WINDOW_SECONDS, CAMPAIGN_SECONDS
from gate_measurement_context import validate, manifest, VARIABLE
from gate_measurement_data import IdleWindow
from gate_measurement_execution import OwnedGate, run_observation
from gate_measurement_inputs import inventory, observe, WORKERS
from gate_measurement_runtime import capture, journal, records, pressure, normal_class, scheduling, sha256
from gate_measurement_setup import immutable, prepare_progress, tools_identity
from gate_measurement_storage import check_storage, pressure_level, directory_identity
from gate_measurement_targets import TargetPool
from verifier_state import Refusal, boot, paths, read, save

LEGS = ['fmt', 'clippy', 'rust-suite', 'rust-contracts', 'build', 'plugin']
POLICY = {'version': 1, 'windows': 2, 'window_seconds': WINDOW_SECONDS,
          'campaign_seconds': CAMPAIGN_SECONDS, 'slots_per_window': 9,
          'cold_warm_seconds': 4500, 'reuse_seconds': 600,
          'quiet_seconds': 60, 'quiet_wait_seconds': 300}
SCRIPTS = Path(__file__).resolve().parent
TOOL_ENV = {'HOME', 'XDG_STATE_HOME', 'PATH', 'USER', 'LOGNAME', 'TMPDIR', 'TZ',
            'LANG', 'LC_ALL', 'LC_CTYPE', 'LC_COLLATE', 'LC_MESSAGES', 'LC_MONETARY',
            'LC_NUMERIC', 'LC_TIME', 'CARGO_HOME', 'RUSTUP_HOME', 'RUSTUP_TOOLCHAIN',
            'DEVELOPER_DIR', 'SDKROOT', 'TOOLCHAINS', 'STORYHOOK_PYTHON',
            'CARGO_INCREMENTAL', 'RUSTFLAGS', 'RUSTDOCFLAGS'}


def campaign_environment(source):
    env = {name: source[name] for name in TOOL_ENV if name in source}
    env.update({name: '1' for name in WORKERS})
    env.update(CARGO_NET_OFFLINE='true', PYTHONDONTWRITEBYTECODE='1')
    return env


def begin_window(root, revision, authorization, *, clock=time.monotonic, boot_id=None):
    """Persist each reservation before preparation; no implicit retries or resets."""
    root = Path(root)
    if revision not in ('baseline', 'optimization'):
        raise Refusal('only baseline and one optimization are supported')
    approval = read(authorization)
    if (not approval or approval.get('kind') != 'coordinated-measurement-start'
            or approval.get('story') != 'SH-872' or approval.get('revision') != revision
            or approval.get('campaign_root') != str(root)
            or not isinstance(approval.get('authority'), str) or not approval['authority'].strip()):
        raise Refusal('exact coordinated measurement start receipt is missing')
    now = clock(); actual_boot = boot() if boot_id is None else boot_id
    campaign = read(root / 'campaign.json')
    if campaign is None:
        if revision != 'baseline':
            raise Refusal('optimization has no original campaign')
        campaign = {'version': 1, 'root': directory_identity(root), 'policy': POLICY,
                    'boot': actual_boot, 'started': now, 'end': now + CAMPAIGN_SECONDS}
        immutable(root / 'campaign.json', campaign)
    if (campaign.get('policy') != POLICY or campaign.get('boot') != actual_boot
            or campaign.get('root') != directory_identity(root)
            or campaign.get('end') != campaign.get('started', -1) + CAMPAIGN_SECONDS
            or not campaign['started'] <= now < campaign['end']):
        raise Refusal('campaign identity, boot or immutable reservation changed/expired')
    rows = records(root / 'windows.jsonl')
    if any(row.get('revision') == revision for row in rows):
        raise Refusal('window already consumed; interrupted reservations are retained')
    if revision == 'optimization':
        baseline, pending, count = Cohort(root, 'baseline').history()
        if (pending or count != 9 or not baseline or any(r['kind']=='finish' and not r['accepted'] for r in baseline)
                or len(rows) != 1 or rows[0].get('revision') != 'baseline'):
            raise Refusal('optimization requires a complete accepted baseline')
    elif rows:
        raise Refusal('baseline must be the first campaign window')
    row = {'kind': 'window', 'revision': revision, 'started': now,
           'end': min(now + WINDOW_SECONDS, campaign['end']),
           'authorization_sha256': sha256(authorization)}
    if row['end'] - now < 4500:
        raise Refusal('remaining reservation cannot fit the first cold gate')
    journal(root / 'windows.jsonl', row)
    return campaign, row


def competing_work(raw, owner_pid):
    """Reject known external build/test executables; retain the complete census."""
    rows = {}
    for line in raw.splitlines():
        parts = line.split(None, 3)
        if len(parts) != 4 or not parts[0].isdigit() or not parts[1].isdigit():
            raise Refusal('process observation is unreadable')
        rows[int(parts[0])] = (int(parts[1]), parts[3])
    if owner_pid not in rows:
        raise Refusal('process observation omitted its measurement owner')
    owned = {owner_pid}
    while True:
        more = {pid for pid, (parent, _) in rows.items() if parent in owned}
        if more <= owned:
            break
        owned |= more
    names = {'cargo', 'rustc', 'rustdoc', 'make', 'gmake', 'xcodebuild',
             'swiftc', 'swift-frontend', 'clang', 'clang++', 'ninja', 'cmake',
             'pytest', 'jest', 'playwright'}
    return [pid for pid, (_, command) in rows.items() if pid not in owned
            and (Path(command).name in names or '/target/' in command or '/deps/' in command)]


def prepare(root, checkout, revision_commit, binary, revision, authorization):
    root, checkout, binary = map(lambda p: Path(p).resolve(strict=True), (root, checkout, binary))
    if not normal_class(scheduling()) or root == checkout or checkout in root.parents:
        raise Refusal('campaign requires a normal collector and separate physical output')
    # ROOT must be explicitly created empty by the operator. Never adopt data.
    if not (root / 'campaign.json').exists() and list(root.iterdir()):
        raise Refusal('new campaign root is not empty')
    fd = os.open(root / 'collection.lock', os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB); os.set_inheritable(fd, True)
    campaign, window = begin_window(root, revision, authorization)
    environment = campaign_environment(os.environ)
    os.environ.clear(); os.environ.update(environment)
    end = min(window['end'], time.monotonic() + 2400)
    os.environ['STORYHOOK_MEASUREMENT_END'] = str(end)
    os.environ['STORYHOOK_MEASUREMENT_OPERATIONS'] = str(root / 'operations')
    if not revision_commit or revision_commit.startswith('-'):
        raise Refusal('measurement source must be an explicit revision')
    git = lambda *args: capture(['git', '-C', str(checkout), *args])
    commit = git('rev-parse', '--verify', revision_commit + '^{commit}')
    tree = git('rev-parse', commit + '^{tree}')
    common = str((checkout / git('rev-parse', '--git-common-dir')).resolve(strict=True))
    gate = json.loads(capture([str(binary), 'verifier', 'gate-config', str(checkout), commit, commit, tree, '--json']))
    if gate.get('result') != 'gate-ready' or gate.get('argv') != ['make', 'test']:
        raise Refusal('throughput campaign supports only the complete current make test gate')
    for name in ('leg.sh', 'gate-receipt.sh', 'tree-receipt.sh'):
        if 'gate-measurement-context.sh' not in git('show', f'{commit}:scripts/{name}'):
            raise Refusal('source lacks the measurement receipt boundary')
    if 'gate_measurement_legs.py' not in git('show', f'{commit}:scripts/leg.sh'):
        raise Refusal('source lacks the measurement leg adapter')
    directory = root / revision; directory.mkdir(mode=0o700)
    manifest_path = directory / 'manifest.json'
    os.environ['CARGO_NET_OFFLINE'] = 'true'
    for name in WORKERS:
        os.environ[name] = '1'  # bounded, explicit and identical in both windows
    value = {'version': 1, 'kind': 'gate-throughput-measurement',
             'campaign_root': str(root), 'revision': revision, 'window': window,
             'campaign': campaign, 'source': str(checkout), 'commit': commit, 'tree': tree,
             'common': common, 'worktree': str(directory / 'worktree'), 'gate': gate,
             'applicable_legs': LEGS, 'binary_sha256': sha256(binary), 'policy': POLICY}
    immutable(manifest_path, value)
    prepare_progress(root / 'progress.jsonl')
    os.environ[VARIABLE] = str(manifest_path)
    os.environ['STORYHOOK_GATE_PROGRESS'] = str(root / 'progress.jsonl')
    os.environ['STORYHOOK_MEASUREMENT_END'] = str(window['end'])
    os.chdir(checkout)
    command = ['bash', str(SCRIPTS / 'machine-lock.sh'), '--max-wait', '60', '--termination-grace', '22',
               'gate', '--', sys.executable, '-B', str(SCRIPTS / 'verifier-owner.py'),
               'measurement-run', common, value['worktree'], '--', sys.executable, '-B',
               str(SCRIPTS / 'gate_measurement_campaign.py'), 'owned', str(manifest_path)]
    os.execvpe(command[0], command, os.environ)


def owned(path):
    value = manifest(path)
    root = Path(value['campaign_root']); revision = value['revision']
    if value.get('policy') != POLICY:
        raise Refusal('throughput containment policy differs from the approved protocol')
    window = Deadline(WINDOW_SECONDS, end=value['window']['end'])
    campaign = Deadline(CAMPAIGN_SECONDS, end=value['campaign']['end'])
    worktree = Path(value['worktree'])
    if worktree.exists():
        raise Refusal('campaign worktree already exists; no silent resume or adoption')
    capture(['git', '-C', value['source'], 'worktree', 'add', '--detach', str(worktree), value['commit']])
    os.chdir(worktree); validate(path)
    # Resolve local locked dependency sources before pinning mutable input bytes.
    # This is bounded offline preparation, not a full gate or warmup.
    capture(['bash', 'scripts/managed-cargo.sh', 'fetch', '--locked', '--offline'])
    pool = TargetPool(root); cohort = Cohort(root, revision)
    inputs = inventory(worktree, os.environ)
    immutable(Path(path).parent / 'inputs.json', inputs)
    initial = True

    def health():
        nonlocal initial
        window.require('campaign window'); campaign.require('campaign')
        observed = pressure()
        observed['storage'] = check_storage(pool.storage(), initial=initial)
        observed['competing_pids'] = competing_work(observed['processes'], os.getpid())
        journal(root / 'pressure.jsonl', dict(observed, kind='health'))
        if observed['competing_pids']:
            raise Refusal('competing external build/test work invalidates this measurement')
        initial = False
        return observed

    def settled():
        validate(path)
        _, _, key = paths(value['common'], worktree)
        owner = read(str(key) + '.owner')
        return bool(owner and owner.get('gate_started') is False and owner.get('gate_session') is None)

    def dispose(name):
        from build_products import ProductLease, namespace
        from gate_measurement_command import bounded
        def remove(target):
            result = bounded([sys.executable, '-B', str(SCRIPTS / 'gate_measurement_targets.py'), 'remove',
                              target['path'], str(target['device']), str(target['inode'])],
                             root=root / 'operations', seconds=min(35, window.remaining(), campaign.remaining()))
            if result.returncode:
                raise Refusal('target removal failed; retained partial lifecycle: ' + result.stderr)
        pool.dispose(name, settled=settled, lease=lambda: ProductLease(namespace(worktree), reclaim=True), remove=remove)

    previous = None
    for block in range(3):
        name = f'{revision}-{block}'
        target = pool.create(name)
        os.environ['CARGO_TARGET_DIR'] = target['path']
        health()
        if previous:
            dispose(previous)
        for _ in range(3):
            quiet = IdleWindow(); quiet_limit = window.child(300)
            while True:
                quiet_limit.require('quiet gate admission')
                sample = health()
                if quiet.observe(time.monotonic(), sample['load'][0], sample['cores']):
                    break
                time.sleep(min(5, quiet_limit.remaining()))
            def observer():
                return observe(value, inputs, os.environ, target, window.child(2400),
                               validate_source=lambda: validate(path), versions=tools_identity,
                               resolve_inventory=lambda: inventory(worktree, os.environ))
            result = run_observation(cohort, observe=observer,
                                     launch=OwnedGate(path, value['gate']['argv'], health=health),
                                     remaining_window=window.remaining(), remaining_campaign=campaign.remaining())
            if not result['accepted']:
                raise Refusal('failed measurement retained; campaign stopped')
        previous = name
    dispose(previous)
    save(Path(path).parent / 'complete.json', {'version': 1, 'revision': revision, 'slots': 9,
                                              'production_certification': False})
    return 0


def main():
    try:
        for sig in (signal.SIGHUP, signal.SIGINT, signal.SIGTERM):
            signal.signal(sig, lambda number, _frame: (_ for _ in ()).throw(InterruptedError(f'signal {number}')))
        mode, *args = sys.argv[1:]
        if mode == 'prepare' and len(args) == 6:
            return prepare(*args)
        if mode == 'owned' and len(args) == 1:
            return owned(*args)
        raise Refusal('invalid explicit throughput campaign invocation')
    except (Refusal, OSError, ValueError, KeyError) as error:
        print(f'measurement campaign: {error}', file=sys.stderr)
        return 2


if __name__ == '__main__': sys.exit(main())
