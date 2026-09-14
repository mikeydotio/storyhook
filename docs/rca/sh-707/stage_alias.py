"""Stage a separately named development plugin without changing existing entries."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import tempfile

from development_artifact import SKILL, validate_overlay
from packaging_evidence import inventory


def stage_alias(artifact_directory, target_home, expected_sha256):
    """Validate a sealed alias artifact, then append only its personal registration."""
    artifact_directory = Path(artifact_directory)
    target_home = Path(target_home).absolute()
    receipt = json.loads((artifact_directory / "provenance.json").read_text())
    name = receipt.get("plugin_name", "")
    if name == "greenlight" or not re.fullmatch(r"[a-z0-9]+(?:-[a-z0-9]+)*", name) or len(name) > 64:
        raise ValueError("refresh requires a separately identified development plugin")
    archive = artifact_directory / "greenlight-sh707-development.tar.gz"
    actual = hashlib.sha256(archive.read_bytes()).hexdigest()
    if actual != expected_sha256 or actual != receipt["artifact_sha256"]:
        raise ValueError("sealed archive identity mismatch")
    plugin = target_home / "plugins" / name
    marketplace = target_home / ".agents/plugins/marketplace.json"
    if plugin.exists() or plugin.is_symlink():
        raise ValueError(f"new source path already exists: {plugin}")
    for path in (plugin, marketplace):
        for ancestor in (path, *path.parents):
            if ancestor.is_symlink():
                raise ValueError(f"symlinked staging path: {ancestor}")
    before = json.loads(marketplace.read_text())
    entries = before.get("plugins")
    if before.get("name") != "personal" or not isinstance(entries, list):
        raise ValueError("existing personal marketplace identity is invalid")
    if any(item.get("name") == name for item in entries):
        raise ValueError("new plugin identity is already registered")
    env = {"PATH": os.environ["PATH"], "HOME": str(target_home), "TMPDIR": "/tmp",
           "LC_ALL": "C", "PYTHONDONTWRITEBYTECODE": "1"}
    commands = []

    def helper(script, *arguments):
        """Run the official registration helper and retain its exact result."""
        result = subprocess.run([sys.executable, str(SKILL / "scripts" / script),
            *map(str, arguments)], env=env, text=True, capture_output=True, timeout=30)
        commands.append({"helper": script, "arguments": list(map(str, arguments)),
            "exit_status": result.returncode, "stdout": result.stdout, "stderr": result.stderr})
        if result.returncode:
            raise ValueError(f"official helper failed: {commands[-1]!r}")
        return result.stdout.strip()

    if helper("read_marketplace_name.py", "--marketplace-path", marketplace) != "personal":
        raise ValueError("official helper selected a different marketplace")
    with tempfile.TemporaryDirectory(prefix="sh707-alias-stage-", dir="/tmp") as directory:
        scratch = Path(directory)
        with tarfile.open(archive) as bundle:
            bundle.extractall(scratch, filter="data")
        if inventory(scratch) != receipt["payload_files"]:
            raise ValueError("archive payload differs from sealed inventory")
        source = scratch / "plugins" / name
        if validate_overlay(receipt["source"], source, name) != receipt["metadata_delta"]:
            raise ValueError("development metadata differs from its receipt")
        proposed = json.loads((scratch / ".agents/plugins/marketplace.json").read_text())
        if proposed.get("name") != "personal" or len(proposed.get("plugins", [])) != 1:
            raise ValueError("artifact must register exactly its new personal identity")
        new_entry = proposed["plugins"][0]
        if new_entry.get("name") != name:
            raise ValueError("artifact registration selects a different plugin")
        helper("create_basic_plugin.py", name, "--with-marketplace")
        after = json.loads(marketplace.read_text())
        expected = dict(before)
        expected["plugins"] = [*entries, new_entry]
        if after != expected:
            raise ValueError("official registration did not preserve existing marketplace entries")
        if set(inventory(plugin)) != {".codex-plugin/plugin.json"}:
            raise ValueError("unexpected files in the newly created scaffold")
        for relative, metadata in receipt["metadata_delta"]["files"].items():
            destination = plugin / relative
            if destination.exists():
                if relative != ".codex-plugin/plugin.json":
                    raise ValueError(f"unexpected source collision: {destination}")
                destination.read_bytes()
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes((source / relative).read_bytes())
            destination.chmod(int(metadata["mode"][-3:], 8))
        if inventory(plugin) != receipt["metadata_delta"]["files"]:
            raise ValueError("new staged source differs from the exact artifact")
    return {"plugin_id": name + "@personal", "version": receipt["metadata_delta"]["version"],
            "source": str(plugin), "marketplace": str(marketplace), "artifact_sha256": actual,
            "existing_registration_preserved": True, "installed": False,
            "native_trust_changed": False, "helper_commands": commands}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifact-directory", type=Path, required=True)
    parser.add_argument("--home", type=Path, required=True)
    parser.add_argument("--expected-sha256", required=True)
    args = parser.parse_args()
    print(json.dumps(stage_alias(args.artifact_directory, args.home, args.expected_sha256), indent=2))
