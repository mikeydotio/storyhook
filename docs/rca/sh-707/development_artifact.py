"""Prepare a separately identified Codex artifact without changing source bytes."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tarfile

from packaging_evidence import export_source, inventory
from smoke_install import CANDIDATE

SKILL = Path("/Users/mikey/.codex/skills/.system/plugin-creator")
OVERLAY = ".codex-plugin/plugin.json"


def validate_overlay(source, plugin):
    """Permit only an added, distinctly versioned Codex manifest over source."""
    files = inventory(plugin)
    original = source["files"]
    removed = sorted(set(original) - set(files))
    modified = sorted(name for name in original if name in files and original[name] != files[name])
    added = sorted(set(files) - set(original))
    if removed or modified:
        raise ValueError(f"source bytes/modes changed: removed={removed}, modified={modified}")
    if added != [OVERLAY]:
        raise ValueError(f"unexpected artifact addition: {added}")
    manifest = json.loads((Path(plugin) / OVERLAY).read_text())
    pattern = re.escape(source["version"].split("+")[0]) + r"\+codex\.[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*"
    if manifest.get("name") != "greenlight" or not re.fullmatch(pattern, manifest.get("version", "")):
        raise ValueError("development manifest identity is invalid")
    return {"added": added, "removed": removed, "modified": modified,
            "version": manifest["version"], "files": files}


def prepare(repo, output, helper_python=sys.executable):
    """Build a new home-shaped artifact with helper-created personal registration."""
    output = Path(output).absolute()
    output.mkdir(parents=True, exist_ok=False)
    output = output.resolve()
    home = output / "payload"
    plugin = home / "plugins/greenlight"
    source = export_source(repo, CANDIDATE, plugin)
    env = {"PATH": os.environ["PATH"], "HOME": str(home), "TMPDIR": "/tmp",
           "PYTHONDONTWRITEBYTECODE": "1", "LC_ALL": "C"}
    commands = []

    def helper(name, *arguments):
        result = subprocess.run([str(helper_python), str(SKILL / "scripts" / name), *map(str, arguments)],
                                env=env, cwd=home, text=True, capture_output=True)
        commands.append({"helper": name, "arguments": list(map(str, arguments)),
                         "exit_status": result.returncode, "stdout": result.stdout, "stderr": result.stderr})
        (output / "helper-commands.json").write_text(json.dumps(commands, indent=2) + "\n")
        if result.returncode:
            raise ValueError(f"official helper {name} failed ({result.returncode}): {result.stdout}{result.stderr}")

    helper("create_basic_plugin.py", "greenlight", "--with-marketplace")
    overlay = plugin / OVERLAY
    manifest = json.loads(overlay.read_text())
    original = json.loads((plugin / ".claude-plugin/plugin.json").read_text())
    manifest.update(version=source["version"], author=original["author"],
                    description=f"Unreleased SH-707 development artifact from Agentics {CANDIDATE}.")
    manifest["interface"].update(displayName="Greenlight (SH-707 development)",
        shortDescription="Temporary Codex compatibility repair; not a release.",
        longDescription=f"Original Greenlight source at {CANDIDATE}; separate Codex metadata only.",
        developerName=original["author"]["name"])
    overlay.write_text(json.dumps(manifest, indent=2) + "\n")
    helper("read_marketplace_name.py")
    helper("update_plugin_cachebuster.py", plugin)
    helper("validate_plugin.py", plugin)
    delta = validate_overlay(source, plugin)
    # The archive is the sealed handoff artifact; helper-created source remains reviewable.
    archive = output / "greenlight-sh707-development.tar.gz"
    with tarfile.open(archive, "w:gz") as bundle:
        for path in sorted(home.rglob("*")):
            bundle.add(path, arcname=path.relative_to(home).as_posix(), recursive=False)
    archive.chmod(0o444)
    receipt = {"kind": "SH-707 development artifact", "release_certified": False,
               "active_host_validated": False, "source": source, "metadata_delta": delta,
               "artifact": str(archive), "artifact_sha256": hashlib.sha256(archive.read_bytes()).hexdigest(),
               "payload_files": inventory(home), "helper_commands": commands,
               "manifest_precedence": "Codex overlay is authoritative for this artifact; original Claude manifest is retained unchanged as source provenance.",
               "lasting_owner": "AGE-103", "retirement": "Replace after a containing release is installed and byte/manifest/native-host validation passes; remove only greenlight@personal."}
    (output / "provenance.json").write_text(json.dumps(receipt, indent=2) + "\n")
    return receipt


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--agentics-repo", type=Path, required=True)
    parser.add_argument("--output-directory", type=Path, required=True)
    parser.add_argument("--helper-python", type=Path, default=sys.executable)
    arguments = parser.parse_args()
    print(json.dumps(prepare(arguments.agentics_repo, arguments.output_directory, arguments.helper_python), indent=2))
