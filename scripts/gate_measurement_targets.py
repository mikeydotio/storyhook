"""At most two fresh campaign targets, with exact-owned, settled disposal.

This never adopts an existing directory or touches a target outside the campaign.
An incomplete creation/deletion journal requires review; it is not auto-repaired.
"""

import os
from pathlib import Path
import re
import shutil
import sys

from gate_measurement_runtime import journal, records
from gate_measurement_setup import immutable
from gate_measurement_storage import (directory_identity, check_identity, usage,
                                      INITIAL_FREE, SYSTEM_HEADROOM, TARGET_CAP, EVIDENCE_CAP)
from verifier_state import Refusal, read


class TargetPool:
    def __init__(self, root):
        self.root = Path(root)
        self.identity = directory_identity(root)
        info = self.root.stat()
        if info.st_uid != os.getuid() or info.st_mode & 0o077:
            raise Refusal('campaign output must be private and owned by this account')
        self.targets = self.root / 'targets'
        self.targets.mkdir(mode=0o700, exist_ok=True)
        self.container = directory_identity(self.targets)
        immutable(self.root / 'target-pool.json', {
            'version': 1, 'root': self.identity,
            'targets': self.container, 'maximum_live': 2,
        })
        self.path = self.root / 'target-lifecycle.jsonl'

    def state(self):
        check_identity(self.identity)
        check_identity(self.container)
        active, pending, consumed = {}, None, set()
        for row in records(self.path):
            name = row.get('name')
            if not isinstance(name, str) or not re.fullmatch(r'(baseline|optimization)-[0-2]', name):
                raise Refusal('invalid campaign target name')
            if row['kind'] == 'create-start':
                if pending or name in consumed or len(active) >= 2:
                    raise Refusal('invalid or concurrent target creation')
                pending = ('create', name); consumed.add(name)
            elif row['kind'] == 'created':
                target = row.get('identity', {})
                if pending != ('create', name) or target.get('path') != str(self.targets / name):
                    raise Refusal('target creation is not bound to its namespace')
                active[name] = target; pending = None
            elif row['kind'] == 'delete-start':
                if pending or name not in active or row.get('identity') != active[name]:
                    raise Refusal('target removal lacks exact prior ownership')
                pending = ('delete', name)
            elif row['kind'] == 'deleted':
                if pending != ('delete', name) or row.get('identity') != active[name]:
                    raise Refusal('target removal has no matching start')
                del active[name]; pending = None
            else:
                raise Refusal('unknown target lifecycle record')
        if pending:
            raise Refusal('target lifecycle is incomplete; preserve it for review')
        if set(os.listdir(self.targets)) != set(active):
            raise Refusal('target namespace has unknown, missing or substituted entries')
        for target in active.values():
            check_identity(target)
        return active

    def create(self, name):
        active = self.state()
        if not re.fullmatch(r'(baseline|optimization)-[0-2]', name):
            raise Refusal('invalid cold target slot')
        if name in active or any(row.get('name') == name for row in records(self.path)) or len(active) >= 2:
            raise Refusal('cold target was consumed or two targets are already retained')
        journal(self.path, {'kind': 'create-start', 'name': name})
        target = self.targets / name
        target.mkdir(mode=0o700)  # existing/shared data is never adopted
        observed = directory_identity(target)
        journal(self.path, {'kind': 'created', 'name': name, 'identity': observed})
        return observed

    def storage(self):
        return {'version': 1, 'root': self.identity, 'targets': list(self.state().values()),
                'disposable': True, 'initial_free_required': INITIAL_FREE,
                'headroom': SYSTEM_HEADROOM, 'target_cap': TARGET_CAP,
                'evidence_cap': EVIDENCE_CAP}

    def dispose(self, name, *, settled, lease, remove):
        """Hold real build-product exclusion across exact-directory disposal.

        The native caller supplies ProductLease(reclaim=True), a live owner
        settlement check, and bounded child removal. Tests use private fixtures.
        """
        target = self.state().get(name)
        if target is None:
            raise Refusal('cannot remove an unowned target')
        with lease():
            if settled() is not True:
                raise Refusal('target still has an unresolved verifier owner')
            check_identity(target)
            # Refuse symlinks/nonregular entries before deleting any byte.
            usage(target['path'])
            journal(self.path, {'kind': 'delete-start', 'name': name, 'identity': target})
            remove(target)
            if os.path.lexists(target['path']):
                raise Refusal('target deletion did not finish; preserve lifecycle evidence')
            journal(self.path, {'kind': 'deleted', 'name': name, 'identity': target})


def remove_exact(path, device, inode):
    """Only a fixed physical target child; shutil's fd-safe implementation required."""
    path = Path(path)
    if (path.parent.name != 'targets' or not re.fullmatch(r'(baseline|optimization)-[0-2]', path.name)
            or not shutil.rmtree.avoids_symlink_attacks):
        raise Refusal('unsafe target removal path or platform')
    check_identity({'path': str(path), 'device': device, 'inode': inode})
    marker = read(path.parent.parent / 'target-pool.json')
    if not marker or marker.get('version') != 1:
        raise Refusal('target lacks its campaign ownership marker')
    check_identity(marker['root']); check_identity(marker['targets'])
    rows = records(path.parent.parent / 'target-lifecycle.jsonl')
    if (not rows or rows[-1].get('kind') != 'delete-start'
            or rows[-1].get('identity') != {'path': str(path), 'device': device, 'inode': inode}):
        raise Refusal('target has no exact pending removal record')
    usage(path)  # no links or unknown types, even after parent admission
    shutil.rmtree(path)


if __name__ == '__main__':
    try:
        if len(sys.argv) != 5 or sys.argv[1] != 'remove':
            raise Refusal('invalid internal target removal operation')
        remove_exact(sys.argv[2], int(sys.argv[3]), int(sys.argv[4]))
    except (Refusal, OSError, ValueError) as error:
        print(f'measurement target: {error}', file=sys.stderr)
        sys.exit(2)
