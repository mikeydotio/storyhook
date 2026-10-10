"""Owner-bound measurement authority; an environment flag alone grants nothing."""

import os
import hashlib
import re
from pathlib import Path
import subprocess
import sys

sys.dont_write_bytecode = True
from verifier_state import Refusal, held, paths, read

VARIABLE = "STORYHOOK_GATE_MEASUREMENT"


def manifest(path):
    """Read the immutable experiment identity, rejecting substituted paths."""
    file = Path(path)
    if not file.is_absolute() or file.resolve() != file or file.name != "manifest.json":
        raise Refusal(f"measurement manifest must be a physical absolute manifest.json: {path}")
    value = read(file)
    if (not value or value.get("version") != 1
            or value.get("kind") not in ("gate-class-measurement", "gate-throughput-measurement")):
        raise Refusal("not a supported measurement manifest")
    if value.get("worktree") != str(file.parent / "worktree"):
        raise Refusal("measurement workspace is not bound to the output directory")
    if value['kind'] == 'gate-throughput-measurement':
        if (value.get('campaign_root') != str(file.parent.parent)
                or value.get('revision') != file.parent.name
                or value['revision'] not in ('baseline', 'optimization')):
            raise Refusal('throughput manifest is not bound to its campaign revision')
    for field in ("commit", "tree"):
        if not isinstance(value.get(field), str) or not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", value[field]):
            raise Refusal(f"measurement {field} is not a pinned object ID")
    common = value.get("common")
    if not isinstance(common, str) or not Path(common).is_absolute() or str(Path(common).resolve()) != common:
        raise Refusal("measurement common directory is not physical and absolute")
    return value


def validate(path=None):
    """Require the exact live measurement owner and its clean pinned workspace."""
    path = os.environ.get(VARIABLE) if path is None else path
    if not path:
        raise Refusal("measurement context is missing")
    value = manifest(path)
    common, worktree, key = paths(value["common"], value["worktree"])
    if not held(common, worktree, key) or (read(str(key) + ".owner") or {}).get("measurement") != path:
        raise Refusal("measurement context has no matching live verifier owner")
    owner = read(str(key) + ".owner")
    digest = hashlib.sha256(Path(path).read_bytes()).hexdigest()
    if owner.get("measurement_sha256") != digest:
        raise Refusal("measurement manifest changed after ownership admission")
    validate_workspace(value)
    return value


def validate_workspace(value):
    """Check pinned tracked inputs after the caller proves its owner capability."""
    common, worktree, _ = paths(value["common"], value["worktree"])
    observed = [subprocess.check_output(["git", *args], text=True, timeout=30).strip() for args in (
        ["rev-parse", "--show-toplevel"], ["rev-parse", "HEAD"],
        ["rev-parse", "HEAD^{tree}"], ["status", "--porcelain", "--untracked-files=no"],
        ["rev-parse", "--git-common-dir"])]
    if (observed[:4] != [str(worktree), value["commit"], value["tree"], ""]
            or str(Path(observed[4]).resolve()) != str(common)):
        raise Refusal(f"measurement workspace or tracked identity changed: {observed!r}")
    return value


if __name__ == "__main__":
    try:
        validate()
    except (Refusal, OSError, ValueError, subprocess.SubprocessError) as error:
        print(f"gate measurement context: {error}", file=sys.stderr)
        sys.exit(2)
