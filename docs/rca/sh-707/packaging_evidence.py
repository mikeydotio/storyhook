"""Strict identity checks for SH-707's disposable installer rehearsal."""

import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import subprocess


def inventory(root):
    """Describe every regular plugin file; refuse unsafe filesystem entries."""
    root = Path(root)
    if root.is_symlink() or not root.is_dir():
        raise ValueError(f"unsafe or missing plugin root: {root}")
    files = {}
    for path in sorted(root.rglob("*")):
        mode = path.lstat().st_mode
        if stat.S_ISDIR(mode):
            continue
        if not stat.S_ISREG(mode) or mode & (stat.S_ISUID | stat.S_ISGID):
            raise ValueError(f"unsafe plugin entry: {path}")
        files[path.relative_to(root).as_posix()] = {
            "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
            "mode": "100755" if mode & 0o111 else "100644",
        }
    return files


def _git(repo, *args):
    # Receipts identify original objects, never a repository's local replace view.
    return subprocess.check_output(["git", "--no-replace-objects", "-C", str(repo), *args],
                                   env={"PATH": os.environ["PATH"], "TMPDIR": "/tmp",
                                        "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": "/dev/null",
                                        "LC_ALL": "C"})


def export_source(repo, commit, destination):
    """Export an exact committed Greenlight tree, without using working files."""
    destination = Path(destination)
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("source identity requires a full immutable commit SHA")
    if destination.exists() or destination.is_symlink():
        raise ValueError(f"export destination already exists: {destination}")
    if _git(repo, "rev-parse", f"{commit}^{{commit}}").decode().strip() != commit:
        raise ValueError("source identity is not the requested commit")
    prefix = "plugins/greenlight/"
    entries = _git(repo, "ls-tree", "-rz", commit, "--", "plugins/greenlight").split(b"\0")
    blobs = []
    for entry in filter(None, entries):
        metadata, name = entry.decode().split("\t", 1)
        mode, kind, oid = metadata.split()
        relative = PurePosixPath(name.removeprefix(prefix))
        if (not name.startswith(prefix) or relative.is_absolute() or ".." in relative.parts
                or mode not in ("100644", "100755") or kind != "blob"):
            raise ValueError(f"unsafe committed plugin entry: {entry!r}")
        blobs.append((relative, mode, _git(repo, "cat-file", "blob", oid)))
    if not blobs:
        raise ValueError("source identity has no Greenlight files")
    destination.mkdir(parents=True)
    for relative, mode, data in blobs:
        path = destination / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        path.chmod(int(mode[-3:], 8))
    manifest = json.loads((destination / ".claude-plugin/plugin.json").read_text())
    if manifest.get("name") != "greenlight" or not manifest.get("version"):
        raise ValueError("source manifest identity is invalid")
    return {"commit": commit, "tree": _git(repo, "rev-parse", f"{commit}^{{tree}}").decode().strip(),
            "plugin_tree": _git(repo, "rev-parse", f"{commit}:plugins/greenlight").decode().strip(),
            "version": manifest["version"], "files": inventory(destination)}


def validate_install(source, installed, outcome, config):
    """Require source parity, installer identity, registration and enablement."""
    installed = Path(installed)
    expected = {"pluginId": "greenlight@sh707-install", "name": "greenlight",
                "marketplaceName": "sh707-install", "version": source["version"],
                "installedPath": str(installed)}
    if any(outcome.get(key) != value for key, value in expected.items()):
        raise ValueError(f"installer identity mismatch: {outcome!r}")
    if inventory(installed) != source["files"]:
        raise ValueError("installed file/mode parity mismatch")
    marketplace = config.get("marketplaces", {}).get("sh707-install", {})
    if (marketplace.get("source_type") != "local"
            or marketplace.get("source") != source["marketplace_root"]):
        raise ValueError("marketplace registration mismatch")
    plugin = config.get("plugins", {}).get("greenlight@sh707-install", {})
    if plugin.get("enabled") is not True:
        raise ValueError("installed plugin is not enabled")
    return {"status": "packaging-parity", "release_certified": False,
            "active_host_validated": False, "source": source, "installer": outcome,
            "registration": marketplace, "plugin": plugin,
            "hook_trust": config.get("hooks", {}).get("state", {})}
