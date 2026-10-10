#!/usr/bin/env python3
"""Configured SH-835 foreground hook and explicit interrupted-purge recovery.

The sole argument is a detached journal, NEVER an original product directory.
No directory scanning, age inference, orphan inference, or live-cache sweeping.
"""
import fcntl
import json
import os
from pathlib import Path
import stat
import sys
import uuid


def identity(info):
    return {'dev': info.st_dev, 'ino': info.st_ino}


def remove_contents(fd, device):
    # Walk the checked open root. A replacement name can never redirect recursion.
    for name in os.listdir(fd):
        info = os.stat(name, dir_fd=fd, follow_symlinks=False)
        if stat.S_ISDIR(info.st_mode):
            child = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
            try:
                actual = os.fstat(child)
                if identity(actual) != identity(info) or actual.st_dev != device:
                    raise ValueError('detached descendant changed or crosses a filesystem')
                remove_contents(child, device)
                if identity(os.stat(name, dir_fd=fd, follow_symlinks=False)) != identity(actual):
                    raise ValueError('detached descendant name changed')
                os.rmdir(name, dir_fd=fd)
            finally:
                os.close(child)
        else:
            # unlink never follows symlinks or modifies another hardlink's bytes.
            os.unlink(name, dir_fd=fd)


def purge(journal, *, authorize=None):
    path = Path(journal)
    if path.name != 'journal.json' or path.parent.parent.name != 'storyhook-detached-products-v1':
        raise ValueError('not a detached product journal')
    # Walk every ancestor without following links, then retain the actual job
    # inode throughout deletion. Reset may rename/remove these names meanwhile.
    fd = os.open(path.anchor, os.O_RDONLY | os.O_DIRECTORY)
    try:
        for part in path.parent.parts[1:]:
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
            os.close(fd)
            fd = child
        info = os.fstat(fd)
        if info.st_uid != os.geteuid() or info.st_mode & 0o077:
            raise ValueError('unsafe detached job directory')
        owner = os.open('purge.lock', os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600, dir_fd=fd)
        try:
            lock_info = os.fstat(owner)
            if not stat.S_ISREG(lock_info.st_mode) or lock_info.st_nlink != 1 or lock_info.st_uid != os.geteuid():
                raise ValueError('unsafe purge lock')
            fcntl.flock(owner, fcntl.LOCK_EX | fcntl.LOCK_NB)
            record_fd = os.open('journal.json', os.O_RDONLY | os.O_NOFOLLOW, dir_fd=fd)
            with os.fdopen(record_fd) as stream:
                record_info = os.fstat(stream.fileno())
                if not stat.S_ISREG(record_info.st_mode) or record_info.st_nlink != 1 or record_info.st_uid != os.geteuid():
                    raise ValueError('unsafe detached journal')
                record = json.load(stream)
            if record.get('version') != 1 or record.get('directory') != identity(info):
                raise ValueError('detached directory identity mismatch')
            if 'retention' in record and authorize is None:
                raise ValueError('retention-enrolled products require the retention policy')
            if record.get('state') not in ('prepared', 'detached', 'purging', 'purged'):
                raise ValueError('unknown detached journal state')
            # Retention decisions run under the same permanent job lock as deletion.
            # A stale preview or a pin added before lock acquisition cannot authorize it.
            if authorize is not None:
                authorize(record, fd)
            def publish(state):
                record['state'] = state
                temp = '.journal-' + uuid.uuid4().hex
                out = os.open(temp, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=fd)
                with os.fdopen(out, 'w') as stream:
                    json.dump(record, stream, sort_keys=True)
                    stream.flush()
                    os.fsync(stream.fileno())
                os.rename(temp, 'journal.json', src_dir_fd=fd, dst_dir_fd=fd)
                os.fsync(fd)
            try:
                products = os.stat('products', dir_fd=fd, follow_symlinks=False)
            except FileNotFoundError:
                if record['state'] in ('purging', 'purged'):
                    publish('purged')
                    return
                raise ValueError('detachment was not proved; original path is never inspected')
            if not stat.S_ISDIR(products.st_mode) or identity(products) != record.get('product'):
                raise ValueError('detached product identity mismatch')
            if record['state'] == 'purged':
                raise ValueError('products appeared after completed purge')
            # Durable intent before recursive work: cancellation retains the job;
            # retry is authorized only against this same detached root identity.
            root = os.open('products', os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
            try:
                if identity(os.fstat(root)) != record['product']:
                    raise ValueError('opened product root differs from journal')
                publish('purging')
                remove_contents(root, products.st_dev)
                if identity(os.stat('products', dir_fd=fd, follow_symlinks=False)) != record['product']:
                    raise ValueError('detached product name changed during purge')
                os.rmdir('products', dir_fd=fd)
                publish('purged')
            finally:
                os.close(root)
        finally:
            os.close(owner)
    finally:
        os.close(fd)


if __name__ == '__main__':
    try:
        if len(sys.argv) != 2:
            raise ValueError('supply exactly one detached journal path')
        purge(sys.argv[1])
    except (OSError, ValueError, KeyError) as error:
        print('detached products retained: ' + str(error), file=sys.stderr)
        sys.exit(1)
