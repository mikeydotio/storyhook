#!/usr/bin/env python3
"""Install a fixture-only Git endpoint adapter without changing Git config."""
import json
import pathlib
import shutil
import sys


def install(bin_dir, mappings):
    """Map network URLs to real local remotes; keep resolution reads unchanged."""
    real = shutil.which("git")
    if real is None:
        raise SystemExit("fixture requires git")
    bin_dir.mkdir(parents=True, exist_ok=True)
    program = f'''#!/usr/bin/env python3
import json, os, pathlib, subprocess, sys
real = {real!r}
mapping = {mappings!r}
args = sys.argv[1:]
i = 0
while i < len(args) and args[i] in ['-C', '-c', '--git-dir', '--work-tree']:
    i += 2
if i < len(args) and args[i] in ['fetch', 'push', 'clone', 'ls-remote'] and '--get-url' not in args:
    for n in range(i + 1, len(args)):
        value = args[n]
        if value == 'origin':
            value = subprocess.check_output([real, *args[:i], 'remote', 'get-url', 'origin'], text=True).strip()
        if value in mapping:
            args[n] = mapping[value]
        elif '://' in value or ('@' in value and ':' in value):
            raise SystemExit('fixture refuses unmapped network destination: ' + value)
os.execv(real, [real, *args])
'''
    target = bin_dir / "git"
    if target.exists():
        raise SystemExit(f"fixture endpoint already exists: {target}")
    target.write_text(program)
    target.chmod(0o755)


if __name__ == "__main__":
    if len(sys.argv) != 3:
        raise SystemExit("usage: test-git-endpoint.py BIN_DIR URL_TO_LOCAL_PATH_JSON")
    install(pathlib.Path(sys.argv[1]), json.loads(sys.argv[2]))
