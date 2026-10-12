"""Version-two project quarantine; authority is independent of removed worktrees."""
import fcntl
import hashlib
import math
import os
from pathlib import Path
import re
import stat
import time
import uuid as uuid_module

ROOT = 'storyhook-retained-products-v2'


def project_name(uuid):
    if not isinstance(uuid, str) or str(uuid_module.UUID(uuid)) != uuid:
        raise ValueError('invalid stable project UUID')
    return 'project-' + hashlib.sha256(uuid.encode()).hexdigest()


def journal_path(path):
    path = Path(path)
    return (path.is_absolute() and '..' not in path.parts and path.name == 'journal.json'
            and re.fullmatch(r'generation-[1-9][0-9]*', path.parent.name)
            and re.fullmatch(r'source-[0-9a-fA-F]{32}', path.parent.parent.name)
            and re.fullmatch(r'project-[0-9a-f]{64}', path.parent.parent.parent.name)
            and path.parent.parent.parent.parent.name == ROOT)


def manifest(api, path, name):
    fd = api.open_directory(path)
    try:
        api.private(os.fstat(fd), stat.S_ISDIR)
        value = api.read_at(fd, name)
        if value.get('directory') != api.purge.identity(os.fstat(fd)):
            raise ValueError('registered retention directory changed')
        return value
    finally:
        os.close(fd)


def namespace(api, path, common, uuid, identity):
    row = manifest(api, path, 'namespace.json')
    fd = api.open_directory(common)
    try:
        if api.purge.identity(os.fstat(fd)) != identity:
            raise ValueError('common Git identity changed')
    finally:
        os.close(fd)
    if (row.get('version') != 2 or row.get('project_uuid') != uuid
            or path.name != project_name(uuid)
            or row.get('common_git') != {'path': str(common), 'identity': identity}):
        raise ValueError('registered project namespace mismatch')
    return row


def validate_config(config):
    def argv(value):
        return isinstance(value, list) and value and all(isinstance(s, str) and s for s in value)
    if not isinstance(config, dict) or set(config) != {'enabled','path','managed_entry','hook','timeout_seconds','retention'}:
        raise ValueError('incomplete native product configuration')
    name = config.get('path')
    policy = config.get('retention')
    if (config['enabled'] is not True or not isinstance(name, str) or not name
            or name.startswith('.') or Path(name).name != name
            or not isinstance(config['managed_entry'], str) or not config['managed_entry']
            or not argv(config['hook']) or type(config['timeout_seconds']) is not int
            or not 1 <= config['timeout_seconds'] <= 300
            or not isinstance(policy, dict) or set(policy) != {'mode','keep','min_age_days','runner'}
            or policy['mode'] not in ('dry-run','apply') or not argv(policy['runner'])
            or type(policy['keep']) is not int or not 1 <= policy['keep'] <= 2**64 - 1
            or type(policy['min_age_days']) is not int or not 1 <= policy['min_age_days'] <= 36500):
        raise ValueError('invalid native product configuration')


def validate_lease(lease):
    def absolute(value):
        return isinstance(value, str) and Path(value).is_absolute() and '..' not in Path(value).parts
    if (not isinstance(lease, dict) or type(lease.get('version')) is not int or lease['version'] != 1
            or not all(isinstance(lease.get(k), str) and lease[k] for k in ('project_slug','story_id','branch'))
            or not all(absolute(lease.get(k)) for k in ('repository_path','worktree_path'))
            or not isinstance(lease.get('tmux'), dict) or not absolute(lease['tmux'].get('socket_path'))):
        raise ValueError('incomplete native cleanup lease')
    # The pruner has no tmux authority. Unknown provenance is retained, not
    # discarded or interpreted as an unprotected legacy resource.
    revivify = lease['tmux'].get('revivify')
    if revivify is not None and (not isinstance(revivify, dict)
            or not absolute(revivify.get('logical_socket'))
            or not isinstance(revivify.get('origin_generation'), str)
            or not re.fullmatch(r'[0-9a-f]{32}', revivify['origin_generation'])):
        raise ValueError('invalid native cleanup provenance')


def source(api, path, uuid):
    row = manifest(api, path, 'source.json')
    nonce = row.get('nonce')
    lease = row.get('lease')
    config = row.get('config')
    def identity(value):
        return (isinstance(value, dict) and set(value) == {'dev', 'ino'}
                and all(type(v) is int and v > 0 for v in value.values()))
    validate_config(config)
    validate_lease(lease)
    private = row.get('private_git')
    if (row.get('version') != 2 or row.get('project_uuid') != uuid
            or not isinstance(nonce, str) or path.name != 'source-' + nonce
            or not re.fullmatch(r'[0-9a-fA-F]{32}', nonce)
            or not isinstance(private, dict) or not identity(private.get('identity'))
            or not isinstance(private.get('path'), str) or not Path(private['path']).is_absolute()
            or not identity(row.get('worktree'))
            or not isinstance(lease, dict) or lease.get('version') != 1
            or not all(isinstance(lease.get(k), str) and lease[k] for k in
                       ('project_slug', 'story_id', 'repository_path', 'worktree_path', 'branch'))
            or not isinstance(config, dict) or config.get('enabled') is not True
            or not isinstance(config.get('retention'), dict)):
        raise ValueError('missing complete source enrollment')
    return row


def validate_job(api, path, row):
    project = path.parent.parent.parent
    common = project.parent.parent
    proof = row.get('retained')
    if not isinstance(proof, dict) or not isinstance(proof.get('namespace'), dict):
        raise ValueError('missing native retention proof')
    expected = proof['namespace']
    common_proof = expected.get('common_git')
    if not isinstance(common_proof, dict):
        raise ValueError('missing common Git authority')
    ns = namespace(api, project, common, expected.get('project_uuid'), common_proof.get('identity'))
    src = source(api, path.parent.parent, ns['project_uuid'])
    if expected != ns or proof.get('source') != src or row.get('lease') != src['lease']:
        raise ValueError('detached source proof does not match registered authority')
    custody = proof.get('custody')
    if not isinstance(custody, list) or not custody:
        raise ValueError('missing detached settlement evidence')
    tokens = set()
    for owner in custody:
        token = owner.get('token') if isinstance(owner, dict) else None
        if not isinstance(token, str) or not re.fullmatch(r'[0-9a-fA-F]{32}', token) or token in tokens:
            raise ValueError('invalid detached settlement identity')
        tokens.add(token)
        api.validate_settlement(owner, token)
    api.validate_retention(row)


class NamespaceLock:
    def __init__(self, api, path):
        self.api, self.path, self.fd = api, path, None
        parent = api.open_directory(path)
        try:
            self.fd = os.open('retention.lock', os.O_RDWR | os.O_NOFOLLOW, dir_fd=parent)
            api.private(os.fstat(self.fd), stat.S_ISREG)
            fcntl.flock(self.fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            self.identity = api.purge.identity(os.fstat(self.fd))
        except BaseException:
            self.close()
            raise
        finally:
            os.close(parent)

    def check(self):
        fd = self.api.open_directory(self.path)
        try:
            if self.api.purge.identity(os.stat('retention.lock', dir_fd=fd, follow_symlinks=False)) != self.identity:
                raise ValueError('namespace lock changed')
        finally:
            os.close(fd)

    def close(self):
        if self.fd is not None:
            os.close(self.fd)
            self.fd = None


def inventory(api, path, uuid):
    fd = api.open_directory(path)
    try:
        names = set(os.listdir(fd))
    finally:
        os.close(fd)
    if not {'namespace.json', 'retention.lock'} <= names or len(names) > 1026:
        raise ValueError('incomplete or oversized project inventory')
    jobs = {}
    for name in sorted(names - {'namespace.json', 'retention.lock'}):
        if not re.fullmatch(r'source-[0-9a-fA-F]{32}', name):
            raise ValueError('unknown project retention entry')
        child = path / name
        source(api, child, uuid)
        fd = api.open_directory(child)
        try:
            generations = set(os.listdir(fd)) - {'source.json'}
        finally:
            os.close(fd)
        for generation in sorted(generations):
            if not re.fullmatch(r'generation-[1-9][0-9]*', generation):
                raise ValueError('unknown source retention entry')
            key = name + '/' + generation
            jobs[key] = api.snapshot(child / generation / 'journal.json')
            if len(jobs) > 1024:
                raise ValueError('retention inventory exceeds 1024 jobs; manual review required')
    return jobs


def prune(api, common, uuid, identity, *, keep=2, min_age_days=7, apply=False, now=None):
    """One project pool, including retired sources; never infer legacy ownership."""
    if (type(keep) is not int or keep < 1 or not math.isfinite(min_age_days)
            or not 1 <= min_age_days <= 36500):
        raise ValueError('invalid common retention policy')
    common = Path(common)
    path = common / ROOT / project_name(uuid)
    result = dict(version=2, project_uuid=uuid, mode='apply' if apply else 'dry-run',
                  keep=keep, min_age_days=min_age_days, jobs=[])
    # Absence is a no-op, without creating authority. Symlinks fail closed.
    common_fd = api.open_directory(common)
    try:
        if api.purge.identity(os.fstat(common_fd)) != identity:
            raise ValueError('common Git identity changed')
    finally:
        os.close(common_fd)
    try:
        root_fd = api.open_directory(path)
    except FileNotFoundError:
        return result
    os.close(root_fd)
    lock = NamespaceLock(api, path)
    try:
        original = namespace(api, path, common, uuid, identity)
        try:
            rows = inventory(api, path, uuid)
        except (OSError, ValueError) as error:
            result.update(action='retain', reason='incomplete inventory: ' + str(error))
            return result
        clock = time.time() if now is None else now
        # Submission sequences are project-store monotonic; nonce disambiguates
        # source provenance. Pins retain additional generations, not the floor.
        intact = [key for key, row in rows.items() if row['state'] == 'detached']
        newest = set(sorted(intact, key=lambda k: (rows[k]['generation'], k), reverse=True)[:keep])
        eligible = []
        for key, row in rows.items():
            value = row['retention']
            item = dict(job=key, action='keep')
            result['jobs'].append(item)
            if row['state'] == 'purged':
                item['reason'] = 'already purged; receipt retained'
            elif value['pinned']:
                item['reason'] = 'pinned'
            elif key in newest:
                item['reason'] = 'newest retained generation'
            elif clock - value['staged_at'] < min_age_days * api.DAY:
                item['reason'] = 'minimum retention age (including future clock)'
            else:
                item.update(action='would-purge', reason='obsolete detached debug products')
                eligible.append((key, item))
        if not apply or not eligible:
            return result
        selected, item = min(eligible, key=lambda kv: (rows[kv[0]]['generation'], kv[0]))
        for key, other in eligible:
            if key != selected:
                other.update(action='keep', reason='one generation per apply; preview next pass')
        journal = path / selected / 'journal.json'
        # All other job locks are short-lived. Pins may change at any time; the
        # selected permanent lock and exact row recheck make a late pin win.
        if inventory(api, path, uuid) != rows:
            raise ValueError('retention inventory changed; rerun preview')
        def authorize(current, fd):
            lock.check()
            if namespace(api, path, common, uuid, identity) != original:
                raise ValueError('namespace changed before purge')
            validate_job(api, journal, current)
            if current != rows[selected] or current['retention']['pinned']:
                raise ValueError('retention decision changed; products retained')
            api.products(fd, current)
            api.debug_layout(fd)
            # Native detachment can now publish other sources while this one
            # job remains protected throughout recursive I/O. No source lock
            # is needed: copied native settlement proves this detached inode.
            lock.close()
        try:
            api.purge.purge(journal, authorize=authorize)
            item['action'] = 'purged'
        except (OSError, ValueError) as error:
            action, state = api.failure_action(journal)
            item.update(action=action, journal_state=state, reason=str(error))
            if action == 'purge-incomplete':
                item['recovery'] = 'bytes may be missing; exact manual recovery required'
        return result
    finally:
        lock.close()
