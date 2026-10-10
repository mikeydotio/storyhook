#!/usr/bin/env python3
"""Retain or prune explicitly enrolled, already detached debug build products.

Default action is a read-only plan. No legacy targets, original paths, worktree
sweep, scheduler, or project enablement. See docs/spec/build-product-retention.md.
"""
import argparse
from contextlib import contextmanager
import fcntl
import importlib.util
import json
import math
import os
from pathlib import Path
import re
import stat
import sys
import time
import uuid

from build_products import ProductLease, git_path

_spec = importlib.util.spec_from_file_location('detached_purge', Path(__file__).with_name('purge-detached-products.py'))
purge = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(purge)
NAMESPACE = 'storyhook-detached-products-v1'
POLICY = 'reproducible-debug-v1'
DAY = 86400


def private(info, kind):
    if (not kind(info.st_mode) or info.st_uid != os.geteuid()
            or info.st_mode & 0o077 or (kind == stat.S_ISREG and info.st_nlink != 1)):
        raise ValueError('unsafe retention authority')


def open_directory(path):
    path = Path(path)
    if not path.is_absolute() or '..' in path.parts:
        raise ValueError('retention path must be absolute and canonical')
    fd = os.open(path.anchor, os.O_RDONLY | os.O_DIRECTORY)
    try:
        for part in path.parts[1:]:
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
            os.close(fd)
            fd = child
        return fd
    except BaseException:
        os.close(fd)
        raise


def read_at(fd, name):
    source = os.open(name, os.O_RDONLY | os.O_NOFOLLOW, dir_fd=fd)
    with os.fdopen(source) as stream:
        info = os.fstat(stream.fileno())
        private(info, stat.S_ISREG)
        if info.st_size > 1024 * 1024:
            raise ValueError('retention record exceeds 1 MiB')
        value = json.load(stream)
    if not isinstance(value, dict):
        raise ValueError('invalid retention record')
    return value


def publish(fd, record):
    name = '.retention-' + uuid.uuid4().hex
    out = os.open(name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=fd)
    try:
        with os.fdopen(out, 'w') as stream:
            json.dump(record, stream, sort_keys=True)
            stream.write('\n')
            stream.flush()
            os.fsync(stream.fileno())
        os.rename(name, 'journal.json', src_dir_fd=fd, dst_dir_fd=fd)
        os.fsync(fd)
    finally:
        try:
            os.unlink(name, dir_fd=fd)
        except FileNotFoundError:
            pass


@contextmanager
def job(journal, *, create_lock=False):
    path = Path(journal)
    if (path.name != 'journal.json' or path.parent.parent.name != NAMESPACE
            or not re.fullmatch(r'generation-[1-9][0-9]*', path.parent.name)):
        raise ValueError('not a managed detachment generation')
    fd = open_directory(path.parent)
    lock = None
    try:
        private(os.fstat(fd), stat.S_ISDIR)
        flags = os.O_RDWR | os.O_NOFOLLOW | (os.O_CREAT if create_lock else 0)
        lock = os.open('purge.lock', flags, 0o600, dir_fd=fd)
        private(os.fstat(lock), stat.S_ISREG)
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        row = read_at(fd, 'journal.json')
        if (row.get('version') != 1 or type(row.get('generation')) is not int
                or row.get('directory') != purge.identity(os.fstat(fd))
                or row.get('generation') != int(path.parent.name.split('-')[1])):
            raise ValueError('detached generation identity mismatch')
        yield fd, row
    finally:
        if lock is not None:
            os.close(lock)
        os.close(fd)


def products(fd, row):
    info = os.stat('products', dir_fd=fd, follow_symlinks=False)
    if (not stat.S_ISDIR(info.st_mode) or info.st_uid != os.geteuid()
            or purge.identity(info) != row.get('product') or info.st_dev != os.fstat(fd).st_dev):
        raise ValueError('detached product identity mismatch')
    return info


def debug_layout(fd):
    """Never enroll mixed release, fixture, evidence, or unknown top-level output."""
    root = os.open('products', os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
    try:
        allowed = {'debug', 'CACHEDIR.TAG', '.rustc_info.json'}
        names = set(os.listdir(root))
        if 'debug' not in names or not names <= allowed:
            raise ValueError('only a dedicated debug-only Cargo target may be retained')
        for name in names - {'debug'}:
            info = os.stat(name, dir_fd=root, follow_symlinks=False)
            if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
                raise ValueError('unexpected debug cache metadata')
        debug = os.open('debug', os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=root)
        try:
            # Cargo root executable names are project-specific. For this first
            # StoryHook policy only known products are admitted. Fixtures and
            # external evidence must use another target or pin the entire job.
            directories = {'.fingerprint', 'build', 'deps', 'examples', 'incremental'}
            files = {'.cargo-lock', 'story', 'story.d', 'libstoryhook.rlib', 'libstoryhook.d'}
            for name in os.listdir(debug):
                info = os.stat(name, dir_fd=debug, follow_symlinks=False)
                if not ((name in directories and stat.S_ISDIR(info.st_mode))
                        or (name in files and stat.S_ISREG(info.st_mode) and info.st_nlink == 1)):
                    raise ValueError('unknown debug output; retain for manual review')
        finally:
            os.close(debug)
    finally:
        os.close(root)


def stage(journal, *, now=None):
    """Opt-in foreground hook: preserve the native quarantine, never delete it."""
    with job(journal, create_lock=True) as (fd, row):
        if row.get('state') != 'detached':
            raise ValueError('only completed atomic detachment may enter retention')
        products(fd, row)
        debug_layout(fd)
        if 'retention' not in row:
            row['retention'] = dict(version=1, policy=POLICY, staged_at=time.time() if now is None else now,
                                    pinned=False)
            publish(fd, row)
        else:
            validate_retention(row)
        return {'generation': row['generation'], 'action': 'retained', 'retention': row['retention']}


def validate_retention(row):
    value = row.get('retention')
    if (not isinstance(value, dict) or value.get('version') != 1
            or value.get('policy') != POLICY or type(value.get('pinned')) is not bool
            or type(value.get('staged_at')) not in (int, float)
            or not math.isfinite(value['staged_at']) or value['staged_at'] <= 0):
        raise ValueError('missing or unknown retention enrollment')
    return value


def pin(journal, pinned):
    with job(journal) as (fd, row):
        value = validate_retention(row)
        if row.get('state') != 'detached':
            raise ValueError('only intact detached products may change pins')
        products(fd, row)
        value['pinned'] = pinned
        publish(fd, row)
        return {'generation': row['generation'], 'pinned': pinned}


def snapshot(journal):
    with job(journal) as (fd, row):
        validate_retention(row)
        if row.get('state') == 'purged':
            try:
                os.stat('products', dir_fd=fd, follow_symlinks=False)
            except FileNotFoundError:
                return row
            raise ValueError('products appeared after completed purge')
        if row.get('state') != 'detached':
            raise ValueError('non-intact job requires exact manual recovery')
        products(fd, row)
        return row


def prune(private_git, *, keep=2, min_age_days=7, apply=False, now=None):
    """One explicit private Git namespace; no worktree discovery or legacy adoption."""
    if type(keep) is not int or keep < 1 or not math.isfinite(min_age_days) or min_age_days < 1:
        raise ValueError('retain at least one generation and one day')
    private_git = Path(private_git)
    namespace = private_git / NAMESPACE
    result = {'version': 1, 'mode': 'apply' if apply else 'dry-run', 'keep': keep,
              'min_age_days': min_age_days, 'jobs': []}
    if not namespace.exists() and not namespace.is_symlink():
        return result
    clock = time.time() if now is None else now
    fd = open_directory(namespace)
    try:
        private(os.fstat(fd), stat.S_ISDIR)
        names = os.listdir(fd)
        if len(names) > 1024:
            raise ValueError('retention inventory exceeds 1024 jobs; manual review required')
        namespace_identity = purge.identity(os.fstat(fd))
    finally:
        os.close(fd)
    rows = {}
    for name in sorted(names):
        item = {'job': name, 'action': 'keep'}
        result['jobs'].append(item)
        try:
            row = snapshot(namespace / name / 'journal.json')
            rows[name] = (row, item)
        except (OSError, ValueError) as error:
            item['reason'] = str(error)
    # Unknown/locked jobs could be a newer retained generation; never make a
    # destructive choice from a partial inventory.
    if len(rows) != len(names):
        for _, item in rows.values():
            item['reason'] = 'incomplete inventory; all products retained'
        return result
    intact = [name for name in rows if rows[name][0]['state'] == 'detached']
    newest = set(sorted(intact, key=lambda name: rows[name][0]['generation'], reverse=True)[:keep])
    for name, (row, item) in rows.items():
        value = row['retention']
        age = clock - value['staged_at']
        if row['state'] == 'purged':
            item['reason'] = 'already purged; receipt retained'
        elif value['pinned']:
            item['reason'] = 'pinned'
        elif name in newest:
            item['reason'] = 'newest retained generation'
        elif age < min_age_days * DAY:
            item['reason'] = 'minimum retention age (including future clock)'
        else:
            item.update(action='would-purge', reason='obsolete detached debug products', age_days=age / DAY)
    if not apply or not any(item['action'] == 'would-purge' for item in result['jobs']):
        return result
    # Detached products already passed native enrollment/state/custody fences.
    # Also defer when this checkout has any current build or unsettled owner.
    custody = private_git / 'storyhook-build-products-v1'
    if not (custody / 'products.lock').is_file():
        raise ValueError('no managed product custody; preserve all jobs')
    with ProductLease(custody, reclaim=True) as lease:
        check = open_directory(namespace)
        try:
            if (purge.identity(os.fstat(check)) != namespace_identity
                    or set(os.listdir(check)) != set(names)):
                raise ValueError('quarantine namespace changed')
            for name, (expected, _) in rows.items():
                if snapshot(namespace / name / 'journal.json') != expected:
                    raise ValueError('retention inventory changed; rerun preview')
        finally:
            os.close(check)
        selected = min((name for name, (_, item) in rows.items()
                        if item['action'] == 'would-purge'),
                       key=lambda name: rows[name][0]['generation'])
        for name, (expected, item) in rows.items():
            if item['action'] != 'would-purge':
                continue
            if name != selected:
                item.update(action='keep', reason='one generation per apply; preview next pass')
                continue
            def authorize(current, fd, expected=expected):
                # Called inside purge.lock, before any journal mutation. Includes
                # pins and original enrollment timestamp; stale plans refuse.
                if current != expected or current['retention']['pinned']:
                    raise ValueError('retention decision changed; products retained')
                products(fd, current)
                debug_layout(fd)
                # The job lock now protects this exact detached inode. Release
                # build exclusion before recursive I/O so a returned story can
                # rebuild its separate original target. Only one job per apply
                # prevents another decision after this reservation is released.
                lease.close()
            try:
                purge.purge(namespace / name / 'journal.json', authorize=authorize)
                item['action'] = 'purged'
            except (OSError, ValueError) as error:
                item.update(action='keep', reason=str(error))
    return result


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    hold = sub.add_parser('stage', help='foreground hook; keep an already detached debug target')
    hold.add_argument('journal', type=Path)
    for name in ('pin', 'unpin'):
        child = sub.add_parser(name)
        child.add_argument('journal', type=Path)
    clean = sub.add_parser('prune', help='inventory this checkout only; default dry-run')
    clean.add_argument('--keep', type=int, default=2)
    clean.add_argument('--min-age-days', type=float, default=7)
    clean.add_argument('--apply', action='store_true', help='irreversibly purge eligible detached products')
    args = parser.parse_args(argv)
    if args.command == 'stage':
        result = stage(args.journal)
    elif args.command in ('pin', 'unpin'):
        result = pin(args.journal, args.command == 'pin')
    else:
        result = prune(git_path(Path.cwd(), '--absolute-git-dir'), keep=args.keep,
                       min_age_days=args.min_age_days, apply=args.apply)
    print(json.dumps(result, indent=2))


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, RuntimeError) as error:
        print(json.dumps({'error': str(error), 'action': 'retain'}), file=sys.stderr)
        sys.exit(1)
