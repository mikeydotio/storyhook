"""Conservative byte identities for measurement inputs, without copying secrets.

All configured dependency roots are read as data and hashed; no discovery text is
executed. Missing optional config files remain part of the identity. Unknown file
types, symlinks inside roots, concurrent writes or a deadline refuse measurement.
"""

import hashlib
import os
from pathlib import Path
import shutil
import stat
import sys

from gate_measurement_cohorts import canonical, fingerprint
from gate_measurement_runtime import capture, resource_limits
from gate_measurement_storage import directory_identity
from verifier_state import Refusal

# These are independently represented source/target locations or lifecycle-only
# telemetry. All other environment variables, including unknown ones, are hashed.
CONTEXT_ENV = {
    'PWD', 'OLDPWD', '_', 'CARGO_TARGET_DIR', 'STORYHOOK_GATE_MEASUREMENT',
    'STORYHOOK_GATE_PROGRESS', 'STORYHOOK_GATE_EXECUTION_FILE',
    'STORYHOOK_MEASUREMENT_SLOT', 'STORYHOOK_MEASUREMENT_GATE_DEADLINE',
    'STORYHOOK_MEASUREMENT_END', 'STORYHOOK_MEASUREMENT_OPERATIONS',
    'STORYHOOK_VERIFIER_OWNER',
}
WORKERS = ('CARGO_BUILD_JOBS', 'STORYHOOK_TEST_THREAD_BUDGET',
           'STORYHOOK_PLUGIN_JOBS', 'STORYHOOK_E2E_JOBS')


def digest(value):
    return hashlib.sha256(canonical(value).encode()).hexdigest()


def stamp(path):
    try:
        value = Path(path).lstat()
    except FileNotFoundError:
        return None
    return (value.st_dev, value.st_ino, value.st_mode, value.st_size,
            value.st_mtime_ns, value.st_ctime_ns)


def check_audit(audit, deadline):
    for path, expected in audit.items():
        deadline.require('measurement input stability check')
        if stamp(path) != expected:
            raise Refusal('input changed during the complete inventory capture')


def snapshot(path, deadline, *, allowed=(), trail=()):
    audit = {}
    result = _snapshot(path, deadline, allowed=allowed, trail=trail, audit=audit)
    check_audit(audit, deadline)
    return result


def _snapshot(path, deadline, *, allowed, trail=(), audit):
    """Hash exact regular-file bytes and modes; never follow a substituted link."""
    path = Path(path)
    deadline.require('measurement input capture')
    if not path.is_absolute() or path.parent.resolve() != path.parent:
        raise Refusal('input path is not physical and absolute')
    observed_stamp = stamp(path)
    if path in audit and audit[path] != observed_stamp:
        raise Refusal('input changed between references in the same inventory')
    audit[path] = observed_stamp
    try:
        before = path.lstat()
    except FileNotFoundError:
        return {'kind': 'absent'}
    if stat.S_ISLNK(before.st_mode):
        target = path.resolve(strict=True)
        if (target in trail or not any(target == root or root in target.parents
                                       for root in allowed)):
            raise Refusal('input symlink escapes its declared dependencies or cycles')
        link = os.readlink(path)
        value = _snapshot(target, deadline, allowed=allowed, trail=(*trail, path), audit=audit)
        after = path.lstat()
        if (before.st_dev, before.st_ino, before.st_ctime_ns) != (after.st_dev, after.st_ino, after.st_ctime_ns) or os.readlink(path) != link:
            raise Refusal('input symlink changed during observation')
        return {'kind': 'symlink', 'link': link, 'target': value}
    if stat.S_ISREG(before.st_mode):
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        try:
            opened = os.fstat(fd)
            if (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino):
                raise Refusal('input file changed while opening')
            h = hashlib.sha256()
            while True:
                deadline.require('measurement input hashing')
                block = os.read(fd, 1024 * 1024)
                if not block:
                    break
                h.update(block)
            after = os.fstat(fd)
        finally:
            os.close(fd)
        fields = lambda s: (s.st_dev, s.st_ino, s.st_size, s.st_mtime_ns, s.st_ctime_ns, s.st_mode)
        if fields(before) != fields(after) or fields(path.lstat()) != fields(after):
            raise Refusal('input file changed while hashing')
        return {'kind': 'file', 'sha256': h.hexdigest(), 'mode': stat.S_IMODE(after.st_mode)}
    if not stat.S_ISDIR(before.st_mode):
        raise Refusal('input contains a symlink or nonregular entry: ' + str(path))
    names = sorted(os.listdir(path))
    children = {name: _snapshot(path / name, deadline, allowed=allowed, trail=(*trail, path), audit=audit) for name in names}
    after = path.lstat()
    if ((before.st_dev, before.st_ino, before.st_mtime_ns, before.st_ctime_ns)
            != (after.st_dev, after.st_ino, after.st_mtime_ns, after.st_ctime_ns)
            or sorted(os.listdir(path)) != names):
        raise Refusal('input directory changed while hashing')
    return {'kind': 'directory', 'sha256': digest(children), 'mode': stat.S_IMODE(after.st_mode)}


def cargo_config_paths(worktree, env):
    """Cargo reads both names in each ancestor and in its explicit home."""
    home = env.get('CARGO_HOME') or str(Path(env['HOME']) / '.cargo')
    paths = [Path(home) / name for name in ('config', 'config.toml')]
    for ancestor in (Path(worktree), *Path(worktree).parents):
        paths.extend(ancestor / '.cargo' / name for name in ('config', 'config.toml'))
    return sorted(set(paths))


def inventory(worktree, env, *, query=capture):
    """Resolve mandatory tools and mutable dependency roots before pinning.

    Config files containing executable credential-provider or source replacement
    directives are still fingerprinted, but no credentials file is opened. The
    actual campaign must be offline: fetching/replacing dependencies during a
    measured run would invalidate the before/after snapshots.
    """
    home = Path(env.get('CARGO_HOME') or str(Path(env['HOME']) / '.cargo')).resolve()
    roots = {'cargo-registry-source': str(home / 'registry' / 'src'),
             'cargo-git-checkouts': str(home / 'git' / 'checkouts')}
    tools = {}
    for name in ('rustc', 'cargo', 'git', 'make', 'node', 'bash', 'sh', 'cc', 'ar'):
        found = shutil.which(name, path=env.get('PATH'))
        if not found:
            raise Refusal('measurement tool is unavailable: ' + name)
        tools[name] = str(Path(found).resolve(strict=True))
    tools['python'] = str(Path(sys.executable).resolve(strict=True))
    # rustup's proxy binary alone does not identify the actual selected compiler.
    sysroot = Path(query(['rustc', '--print', 'sysroot'])).resolve(strict=True)
    roots['rust-sysroot'] = str(sysroot)
    sdk = Path(query(['xcrun', '--show-sdk-path'])).resolve(strict=True)
    roots['apple-sdk'] = str(sdk)
    for name in ('xcrun', 'xcodebuild'):
        found = shutil.which(name, path=env.get('PATH'))
        if not found:
            raise Refusal('measurement Apple tool is unavailable: ' + name)
        tools[name] = str(Path(found).resolve(strict=True))
    # Node modules are mutable, ignored dependency input, independently of locks.
    roots['node-modules'] = str(Path(worktree) / 'node_modules')
    roots['browser-node-modules'] = str(Path(worktree) / 'e2e' / 'node_modules')
    return {'version': 1, 'tools': tools, 'dependency_roots': roots,
            'cargo_configs': [str(p) for p in cargo_config_paths(worktree, env)]}


def observe(manifest, inventory_record, env, target, deadline, *, validate_source,
            versions, resolve_inventory, limits=resource_limits):
    """Capture every declared input on each boundary; no mtime digest cache."""
    validate_source()
    if inventory_record.get('version') != 1:
        raise Refusal('measurement input inventory is missing')
    if resolve_inventory() != inventory_record:
        raise Refusal('selected toolchain or dependency locations changed')
    allowed = tuple(Path(p) for p in [*inventory_record['tools'].values(),
                                     *inventory_record['dependency_roots'].values(),
                                     *inventory_record['cargo_configs']])
    audit = {}
    tools = {name: _snapshot(path, deadline, allowed=allowed, audit=audit) for name, path in inventory_record['tools'].items()}
    if not tools or any(row['kind'] != 'file' for row in tools.values()):
        raise Refusal('measurement executable inventory is incomplete')
    dependencies = {name: _snapshot(path, deadline, allowed=allowed, audit=audit)
                    for name, path in inventory_record['dependency_roots'].items()}
    configs = [_snapshot(path, deadline, allowed=allowed, audit=audit) for path in inventory_record['cargo_configs']]
    workers = {}
    for name in WORKERS:
        raw = env.get(name)
        if not isinstance(raw, str) or not raw.isdecimal() or not 1 <= int(raw) <= 8:
            raise Refusal('measurement requires an explicit bounded worker limit: ' + name)
        workers[name] = int(raw)
    workers['resource_limits'] = limits()
    actual_target = directory_identity(env.get('CARGO_TARGET_DIR', ''))
    if actual_target != target:
        raise Refusal('measurement target identity changed')
    result = {
        'source_commit': manifest['commit'], 'source_tree': manifest['tree'],
        'toolchain': {'executables': tools, 'versions': versions(), 'dependencies': dependencies},
        'environment_digest': digest({k: v for k, v in env.items() if k not in CONTEXT_ENV}),
        'configuration_digest': digest({'external_cargo_configs': configs, 'gate_argv': manifest['gate']['argv'],
                                        'project_configuration': manifest['gate'].get('configuration')}),
        'worker_limits': workers, 'gate_argv': manifest['gate']['argv'],
        'target_identity': target, 'applicable_legs': manifest['applicable_legs'],
    }
    fingerprint(result)
    check_audit(audit, deadline)
    validate_source()
    return result
