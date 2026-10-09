"""Opt in only a newly-created dispatch with no existing product directory.

Old/reused lanes are never retroactively enrolled. No deletion occurs here.
The provider receives the managed-entry contract before its story charter.
"""
import json
import os
from pathlib import Path
import stat
import subprocess
import sys


def identity(path):
    info = path.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.geteuid():
        raise ValueError('unowned or symlinked dispatch directory')
    return {'dev': info.st_dev, 'ino': info.st_ino}


def enroll(worktree, fresh):
    if not fresh:
        return ''
    # A Python lacking TOML support preserves all products, without preventing
    # ordinary dispatch. It cannot attest a contract it did not parse.
    try:
        import tomllib
    except ImportError:
        raise ValueError('enrollment requires Python >=3.11; configure STORYHOOK_PYTHON')
    worktree = Path(worktree)
    with (worktree / '.storyhook.toml').open('rb') as stream:
        config = tomllib.load(stream).get('build_products')
    if not config or config.get('enabled') is not True:
        return ''
    if set(config) != {'enabled', 'path', 'managed_entry', 'hook', 'timeout_seconds'}:
        raise ValueError('invalid build product contract fields')
    name = config['path']
    if not isinstance(name, str) or name.startswith('.') or Path(name).name != name or not name:
        raise ValueError('product path must be one private directory name')
    if not isinstance(config['managed_entry'], str) or not config['managed_entry']:
        raise ValueError('managed entry is required')
    if not isinstance(config['hook'], list) or not config['hook'] or not all(isinstance(x, str) and x for x in config['hook']):
        raise ValueError('foreground purge argv is required')
    if type(config['timeout_seconds']) is not int or not 1 <= config['timeout_seconds'] <= 300:
        raise ValueError('purge timeout must be between 1 and 300 seconds')
    target = worktree / name
    if target.exists() or target.is_symlink():
        raise ValueError('pre-existing products have unknown ownership')
    private = Path(subprocess.run(['git', '-C', str(worktree), 'rev-parse', '--absolute-git-dir'],
                                 check=True, capture_output=True, text=True, timeout=10).stdout.strip())
    marker = private / 'storyhook-cleanup-lease-v1.json'
    fd = os.open(marker, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(fd) as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_uid != os.geteuid():
            raise ValueError('unsafe cleanup lease marker')
        lease = json.load(stream)
    if lease['version'] != 1 or lease['worktree_path'] != str(worktree.resolve()):
        raise ValueError('cleanup lease worktree mismatch')
    value = {'version': 1, 'lease': lease, 'config': config,
             'worktree': identity(worktree), 'private_git': identity(private)}
    fd = os.open(private / 'storyhook-products-enrollment-v1.json',
                 os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'w') as stream:
        json.dump(value, stream, sort_keys=True)
        stream.flush()
        os.fsync(stream.fileno())
    parent = os.open(private, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(parent)
    finally:
        os.close(parent)
    return ('Build product custody is enabled for this fresh worktree. '
            'Run every build or product-generating test through ' + config['managed_entry'] +
            '; do not use bare build commands or escape its inherited custody descriptors. '
            'After submission, old ' + name + ' products may be detached and removed; '
            'a returned story rebuilds through the same managed entry.')


if __name__ == '__main__':
    try:
        if sys.version_info < (3, 11):
            candidates = ([os.environ['STORYHOOK_PYTHON']] if os.environ.get('STORYHOOK_PYTHON')
                          else ['/opt/homebrew/bin/python3', '/usr/local/bin/python3'])
            for candidate in candidates:
                if not os.path.isabs(candidate) or not os.access(candidate, os.X_OK):
                    continue
                probe = subprocess.run([candidate, '-I', '-S', '-c',
                                        'import sys; raise SystemExit(0 if (3,11) <= sys.version_info < (4,) else 1)'],
                                       timeout=10, capture_output=True)
                if probe.returncode == 0:
                    os.execv(candidate, [candidate, '-B', str(Path(__file__).resolve()), *sys.argv[1:]])
            raise ValueError('enrollment requires Python >=3.11; configure STORYHOOK_PYTHON')
        print(enroll(sys.argv[1], sys.argv[2] == 'true'))
    except (OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        print('build products remain unenrolled: ' + str(error), file=sys.stderr)
        sys.exit(1)
