"""Classify provider workspace consent without interpreting terminal text as code."""

import hashlib
import json
import pathlib
import re
import sys
import time


class TrustError(ValueError):
    """A workspace dialog cannot safely authorize an input event."""


def canonical(path):
    """Resolve an existing absolute directory, preserving significant whitespace."""
    if not path.startswith('/') or '…' in path or '\x00' in path:
        raise TrustError('trust-path-mismatch')
    try:
        resolved = pathlib.Path(path).resolve(strict=True)
        if not resolved.is_dir():
            raise TrustError('trust-path-mismatch')
        return str(resolved)
    except (OSError, RuntimeError) as error:
        raise TrustError('trust-path-mismatch') from error


def classify(provider, screen, worktree, repository_root):
    """Return the safe selection key and dialog identity, or reject unsafe consent."""
    screen = re.sub(r'\x1b\[[0-9;]*m', '', screen)
    lines = []
    for raw in screen.splitlines():
        line = raw.strip()
        if line.startswith(('╭', '╰', '┌', '└')):
            line = line.strip('╭╰┌└─━╮╯┐┘ ').strip()
        elif line.startswith(('│', '┃')):
            line = line[1:].strip()
            if line.endswith(('│', '┃')):
                line = line[:-1].strip()
        if line:
            lines.append(line)
    if not lines:
        return None
    detected = {'Folder access': 'codex', 'Accessing workspace:': 'claude'}.get(lines[0])
    if detected is None:
        return None
    if detected != provider:
        raise TrustError('trust-provider-mismatch')
    if len(lines) < 4 or canonical(lines[1]) != canonical(worktree):
        raise TrustError('trust-path-mismatch')
    if provider == 'codex':
        if not any(line.startswith('Trust this folder?') for line in lines[2:]):
            raise TrustError('trust-dialog-invalid')
        positive, negatives, cursor = 'Trust and continue', ('Quit',), '›'
        note = 'Note: You’re in a subdirectory of a Git project. Trusting will apply to the repository root:'
        notes = [index for index, line in enumerate(lines) if line.startswith('Note:')]
        if len(notes) > 1:
            raise TrustError('trust-dialog-invalid')
        if notes:
            index = notes[0]
            if lines[index] != note or index + 1 >= len(lines) or canonical(lines[index + 1]) != canonical(repository_root):
                raise TrustError('trust-path-mismatch')
    else:
        if not any(line.startswith('Quick safety check: Is this a project you created or one you trust?') for line in lines[2:]):
            raise TrustError('trust-dialog-invalid')
        positive, negatives, cursor = 'Yes, I trust this folder', ('No, exit', 'No, continue without these permissions'), '❯'
    choices = []
    normalized = []
    for index, line in enumerate(lines):
        selected = line.startswith(cursor)
        label = line[len(cursor):].strip() if selected else line
        label = re.sub(r'^\d+\.\s*', '', label)
        if label == positive or label in negatives:
            choices.append((index, label == positive, selected))
            normalized.append(label)
        else:
            if selected or re.match(r'^\d+\.\s', line):
                raise TrustError('trust-dialog-invalid')
            normalized.append(line)
    if len(choices) != 2 or sum(yes for _, yes, _ in choices) != 1 or sum(selected for _, _, selected in choices) != 1:
        raise TrustError('trust-dialog-invalid')
    affirmative = next(index for index, yes, _ in choices if yes)
    selected = next(index for index, _, focus in choices if focus)
    key = 'Enter' if affirmative == selected else ('Up' if affirmative < selected else 'Down')
    return {'key': key, 'fingerprint': hashlib.sha256('\n'.join(normalized).encode()).hexdigest()}


def main():
    """Expose classification and monotonic time to the shell effect owner."""
    try:
        if sys.argv[1] == 'same-path':
            return 0 if canonical(sys.argv[2]) == canonical(sys.argv[3]) else 1
        result = classify(*sys.argv[1:2], sys.stdin.read(), *sys.argv[2:4])
        # Separate parser processes need the shared OS epoch; macOS Python 3.9
        # gives time.monotonic() a process-local origin.
        print(json.dumps({'dialog': result, 'now': time.clock_gettime(time.CLOCK_MONOTONIC)}))
        return 0
    except (TrustError, OSError, ValueError) as error:
        print(json.dumps({'reason': str(error) if isinstance(error, TrustError) else 'trust-classifier-failed'}))
        return 1


if __name__ == '__main__':
    sys.exit(main())
