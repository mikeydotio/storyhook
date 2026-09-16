#!/bin/sh
set -eu

BINARY="story"
INSTALL_DIR="${STORYHOOK_INSTALL_DIR:-${HOME}/.local/bin}"
if [ "$#" -ne 2 ] || [ "$1" != --source ]; then
  echo 'usage: sh install.sh --source HOST/OWNER/REPO' >&2
  exit 2
fi
command -v python3 >/dev/null 2>&1 || { echo 'error: install Python 3 before running the installer' >&2; exit 1; }
command -v gh >/dev/null 2>&1 || { echo 'error: install gh and authenticate the selected host before running the installer' >&2; exit 1; }

# No story executable exists yet. This bootstrap implements only the explicit
# release-source protocol; project operations use the Rust boundary.
python3 - "$2" "$INSTALL_DIR/$BINARY" "${STORYHOOK_VERSION:-latest}" <<'PY'
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import signal
import subprocess
import sys
import tarfile
import tempfile


def fail(message):
    raise RuntimeError(message)


def source_identity(value):
    parts = value.split('/')
    if len(parts) != 3:
        fail('release source must be HOST/OWNER/REPO')
    host, owner, repo = parts
    if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9.-]*(?::[0-9]+)?', host):
        fail('invalid release source host')
    if ':' in host and not 0 < int(host.rsplit(':', 1)[1]) <= 65535:
        fail('invalid release source port')
    if repo.endswith('.git'):
        repo = repo[:-4]
    for part in (owner, repo):
        if not re.fullmatch(r'[A-Za-z0-9_.-]+', part) or part in ('.', '..'):
            fail('invalid release source owner or repository')
    return host.lower(), '/'.join((host, owner, repo)).lower()


try:
    host, source = source_identity(sys.argv[1])
except RuntimeError as error:
    print(f'error: {error}', file=sys.stderr)
    sys.exit(2)
# Only gh receives gh credentials. Destination/debug/proxy variables cannot
# override source selection, and subprocess output never streams to a log.
allowed = ('PATH', 'HOME', 'XDG_CONFIG_HOME', 'LANG', 'LC_ALL', 'TMPDIR',
           'GH_CONFIG_DIR', 'GH_TOKEN', 'GITHUB_TOKEN',
           'GH_ENTERPRISE_TOKEN', 'GITHUB_ENTERPRISE_TOKEN')
gh_env = {key: os.environ[key] for key in allowed if key in os.environ}
gh_env.update(GH_HOST=host, GH_REPO=source, GH_PROMPT_DISABLED='1',
              GH_PAGER='cat', GH_NO_UPDATE_NOTIFIER='1',
              GH_NO_EXTENSION_UPDATE_NOTIFIER='1', GIT_TERMINAL_PROMPT='0')


def run(args, env, timeout=120):
    with tempfile.TemporaryFile() as out, tempfile.TemporaryFile() as err:
        process = subprocess.Popen(args, cwd='/', env=env, stdin=subprocess.DEVNULL,
                                   stdout=out, stderr=err, start_new_session=True)
        try:
            process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
            fail('subprocess deadline exceeded; operation was not retried')
        out.seek(0)
        data = out.read(65537)
        err.seek(0)
        detail = err.read(65536).decode('utf-8', errors='replace')
        for key, value in gh_env.items():
            if key.endswith('TOKEN') and value:
                detail = detail.replace(value, '[REDACTED]')
        detail = re.sub(r'(?:github_pat_|gh[pousr]_)[A-Za-z0-9_]+', '[REDACTED]', detail)
        detail = re.sub(r'(?i)(authorization:\s*(?:bearer|token)\s+)\S+', r'\1[REDACTED]', detail)
        if process.returncode:
            guidance = f'; check gh auth status --hostname {host}' if process.returncode == 4 or '401' in detail else ''
            fail(f'{args[0]} for {source} failed ({process.returncode}): {detail}{guidance}')
        if len(data) > 65536:
            fail('subprocess exceeded the capture limit')
        return data


def gh(*args):
    return run(['gh', *args, '--repo', source], gh_env)


try:
    destination = Path(sys.argv[2]).expanduser().resolve()
    destination.parent.mkdir(parents=True, exist_ok=True)
    with open(str(destination) + '.install.lock', 'a') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            fail('another installation holds the update lock')
        tag = sys.argv[3]
        if tag == 'latest':
            tag = json.loads(gh('release', 'view', '--json', 'tagName'))['tagName']
        if not isinstance(tag, str) or not tag or tag.startswith('-') or any(c.isspace() or ord(c) < 32 or ord(c) == 127 for c in tag):
            fail('release metadata contains an invalid tag')
        arch = {'x86_64': 'x86_64', 'amd64': 'x86_64', 'aarch64': 'aarch64', 'arm64': 'aarch64'}.get(platform.machine())
        system = {'Linux': 'unknown-linux-gnu', 'Darwin': 'apple-darwin'}.get(platform.system())
        if not arch or not system:
            fail('unsupported platform: no prebuilt story release')
        artifact = f'story-{arch}-{system}.tar.gz'
        print(f'Installing story {tag} from {source} to {destination}...')
        with tempfile.TemporaryDirectory(prefix='.story-install-', dir=destination.parent) as work:
            archive = Path(work) / artifact
            gh('release', 'download', tag, '--pattern', artifact, '--output', str(archive))
            staged = Path(work) / 'story'
            with tarfile.open(archive, 'r:gz') as bundle:
                members = [m for m in bundle.getmembers() if m.name in ('story', './story')]
                if len(members) != 1 or not members[0].isfile():
                    fail('release archive must contain one regular story executable')
                with bundle.extractfile(members[0]) as src, open(staged, 'xb') as dst:
                    import shutil
                    shutil.copyfileobj(src, dst)
            staged.chmod(0o755)
            run([str(staged), '--help'], {k: v for k, v in gh_env.items() if not k.endswith('TOKEN')}, timeout=30)
            metadata = Path(work) / 'source.json'
            with open(staged, 'rb') as binary:
                digest = hashlib.sha256()
                for chunk in iter(lambda: binary.read(1024 * 1024), b''):
                    digest.update(chunk)
            metadata.write_text(json.dumps({'version': 1, 'source': source, 'sha256': digest.hexdigest()}))
            os.replace(staged, destination)
            try:
                os.replace(metadata, str(destination) + '.source.json')
            except OSError as error:
                fail(f'binary replaced but source metadata publication failed: {error}; recover with story update --source {source} --force')
        print(f'Installed story to {destination}')
except (OSError, ValueError, KeyError, RuntimeError, tarfile.TarError) as error:
    print(f'error: {error}', file=sys.stderr)
    sys.exit(1)
PY

# Reinstall the plugin for every provider (Claude Code, Codex) that has the
# storyhook marketplace registered, from the binary just installed: the plugin
# travels inside the binary, so a new binary is a new plugin (SH-667). A
# provider that was never installed is left alone. Not fatal: the binary is
# already in place, and a pinned STORYHOOK_VERSION older than the verb exits 2
# here -- name it and the retry rather than fail an install that succeeded.
if ! "${INSTALL_DIR}/${BINARY}" plugin reinstall; then
  echo "warning: the provider plugins were not reinstalled; run: story plugin reinstall" >&2
fi

# --- Interactive setup starts here ---

# Detect if we can prompt interactively
if [ -t 0 ]; then
  TTY_IN="/dev/stdin"
elif [ -e /dev/tty ]; then
  TTY_IN="/dev/tty"
else
  TTY_IN=""
fi

if [ -n "$TTY_IN" ]; then
  # Git hooks (only if in a git repo)
  if git rev-parse --git-dir >/dev/null 2>&1; then
    echo ""
    echo "=== Git Hooks ==="
    echo "Available hooks:"
    echo "  - post-commit: link commits to referenced stories"
    echo "  - post-merge: auto-close stories on merge to main"
    echo "  - prepare-commit-msg: show top story in commit template"
    echo ""
    printf "  Install git hooks in this repository? [Y/n] " > /dev/tty
    read -r ans < "$TTY_IN" || ans=""
    case "$ans" in
      [Nn]*) ;;
      *)
        if "${INSTALL_DIR}/${BINARY}" hooks install 2>&1; then
          echo "    done."
        else
          echo "    skipped (could not install hooks)."
        fi
      ;;
    esac
  fi
else
  # Non-interactive fallback
  echo ""
  echo "To install git hooks: story hooks install"
fi

# Check if install dir is in PATH
case ":${PATH}:" in
  *":${INSTALL_DIR}:"*) ;;
  *)
    echo ""
    echo "WARNING: ${INSTALL_DIR} is not in your PATH."
    echo "Add it by appending this to your shell profile:"
    echo ""
    echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
    ;;
esac
