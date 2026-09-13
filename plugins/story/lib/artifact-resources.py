"""Reject resource mutations overlapping registered installed artifacts."""

import os
from pathlib import Path
import stat
import sys


def _resolved(path):
    # Path.resolve(strict=False) hides loops and permission errors. Resolve
    # existing ancestry strictly, allowing only a genuinely missing suffix.
    try:
        return str(Path(path).resolve(strict=True))
    except FileNotFoundError:
        if os.path.lexists(path):
            raise ValueError(f"unresolved resource symlink: {path}")
        parent, leaf = os.path.split(path)
        if not leaf or parent == path:
            raise ValueError(f"unresolved resource: {path}")
        resolved_parent = _resolved(parent)
        if os.path.exists(resolved_parent) and not os.path.isdir(resolved_parent):
            raise ValueError(f"resource parent is not a directory: {parent}")
        return os.path.join(resolved_parent, leaf)


def _identities(path):
    path = os.fspath(path)
    if not os.path.isabs(path) or "\x00" in path:
        raise ValueError(f"resource path must be absolute: {path!r}")
    return {os.path.normpath(path), _resolved(path)}


def _contains(parent, child):
    return os.path.commonpath([parent, child]) == parent


def check_resources(manifest, writes, removal):
    """Validate write locations and an optional recursive removal target."""
    try:
        # A missing registry is the hook's uninstalled-host contract. An
        # existing but unreadable registry is not evidence of absence.
        try:
            os.lstat(manifest)
        except FileNotFoundError:
            return
        descriptor = os.open(manifest, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW)
        with os.fdopen(descriptor, encoding="utf-8") as handle:
            if not stat.S_ISREG(os.fstat(handle.fileno()).st_mode):
                raise ValueError(f"managed-path manifest is not a regular file: {manifest}")
            prefixes = [line.strip() for line in handle if line.strip() and not line.lstrip().startswith("#")]
        if not prefixes:
            raise ValueError(f"managed-path manifest contains no paths: {manifest}")
        protected = [(prefix, _identities(prefix)) for prefix in prefixes]
        targets = [(path, False) for path in writes]
        if removal is not None:
            targets.append((removal, True))
        for target, recursive in targets:
            identities = _identities(target)
            for prefix, boundaries in protected:
                if any(_contains(boundary, identity) or (recursive and _contains(identity, boundary))
                       for boundary in boundaries for identity in identities):
                    raise ValueError(f"resource {target} overlaps installed artifacts registered at {prefix}")
    except (OSError, RuntimeError, UnicodeError) as error:
        raise ValueError(f"cannot establish installed artifact safety using {manifest}: {error}") from error


def main():
    """Check writes and a worktree; an empty worktree means no removal."""
    if len(sys.argv) != 4:
        raise ValueError("expected repository, common Git directory and worktree")
    data = os.environ.get("STORYHOOK_DATA_DIR")
    if not data:
        xdg = os.environ.get("XDG_DATA_HOME")
        home = os.environ.get("HOME", "")
        if not xdg and not os.path.isabs(home):
            raise ValueError("cannot resolve the managed-path manifest without absolute HOME")
        data = os.path.join(xdg or os.path.join(home, ".local/share"), "storyhook")
    if not os.path.isabs(data):
        raise ValueError(f"managed-path data directory must be absolute: {data}")
    check_resources(os.path.join(data, "managed-paths"), sys.argv[1:3], sys.argv[3] or None)


if __name__ == "__main__":
    try:
        main()
    except ValueError as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
