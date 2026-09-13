"""Stage only the reviewed personal source artifact; never install or grant trust.

Requires the supervisor-approved archive SHA256. Existing personal registration
or source paths cause refusal before the official scaffold writes anything.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile

from development_artifact import SKILL, validate_overlay
from packaging_evidence import inventory


def stage(artifact_directory, target_home, expected_sha256):
    """Validate in scratch, then scaffold new registration and copy exact source."""
    archive = artifact_directory / "greenlight-sh707-development.tar.gz"
    receipt = json.loads((artifact_directory / "provenance.json").read_text())
    actual = hashlib.sha256(archive.read_bytes()).hexdigest()
    if actual != expected_sha256 or actual != receipt["artifact_sha256"]:
        raise ValueError("archive identity mismatch")
    target_home = target_home.absolute()
    plugin = target_home / "plugins/greenlight"
    marketplace = target_home / ".agents/plugins/marketplace.json"
    for path in (plugin, marketplace):
        if path.exists() or path.is_symlink():
            raise ValueError(f"existing destination; refusing overwrite: {path}")
        for parent in path.parents:
            if parent.is_symlink():
                raise ValueError(f"symlinked destination ancestor: {parent}")
    with tempfile.TemporaryDirectory(prefix="sh707-stage-", dir="/tmp") as scratch:
        scratch = Path(scratch)
        with tarfile.open(archive) as bundle:
            bundle.extractall(scratch, filter="data")
        if inventory(scratch) != receipt["payload_files"]:
            raise ValueError("archive payload inventory differs")
        delta = validate_overlay(receipt["source"], scratch / "plugins/greenlight")
        if delta != receipt["metadata_delta"]:
            raise ValueError("metadata receipt differs from sealed archive")
        expected_marketplace = (scratch / ".agents/plugins/marketplace.json").read_bytes()
        env = {"HOME": str(target_home), "PATH": os.environ["PATH"], "TMPDIR": "/tmp",
               "PYTHONDONTWRITEBYTECODE": "1", "LC_ALL": "C"}
        result = subprocess.run([sys.executable, str(SKILL / "scripts/create_basic_plugin.py"),
                                 "greenlight", "--with-marketplace"], env=env,
                                text=True, capture_output=True)
        if result.returncode:
            raise ValueError(f"official scaffold failed: {result.stdout}{result.stderr}")
        if marketplace.read_bytes() != expected_marketplace:
            raise ValueError("helper-created personal registration differs from reviewed artifact")
        # The only existing source file is the scaffold this call just created.
        for relative, metadata in delta["files"].items():
            source = scratch / "plugins/greenlight" / relative
            destination = plugin / relative
            if destination.exists():
                if relative != ".codex-plugin/plugin.json":
                    raise ValueError(f"unexpected source collision: {destination}")
                destination.read_bytes()
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(source.read_bytes())
            destination.chmod(int(metadata["mode"][-3:], 8))
        if inventory(plugin) != receipt["metadata_delta"]["files"]:
            raise ValueError("staged artifact source parity differs")
    return {"source": str(plugin), "marketplace": str(marketplace),
            "artifact_sha256": actual, "installed": False, "native_trust_changed": False}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifact-directory", type=Path, required=True)
    parser.add_argument("--home", type=Path, required=True)
    parser.add_argument("--expected-sha256", required=True)
    args = parser.parse_args()
    print(json.dumps(stage(args.artifact_directory, args.home, args.expected_sha256), indent=2))
