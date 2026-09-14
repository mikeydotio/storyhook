"""Rehearse Greenlight packaging and rollback using Codex's normal installer.

Usage: python3 smoke_install.py --agentics-repo PATH --output-directory NEW_PATH
Only the newly created output directory is written. No active config is copied.
The retained evidence is a candidate packaging result, never release certification.
"""

import argparse
import json
import os
from pathlib import Path
import subprocess
import tomllib

from installed_contract import exercise
from packaging_evidence import export_source, inventory, validate_install

CANDIDATE = "af73549175847d158c5a59de772f6cfee7b256f5"
BASELINE = "c25f78e2337013370ac69bc2265d47f2431359c4"
# Local installer startup may take longer than hooks; no network or model is needed.
INSTALL_DEADLINE = 120


def main():
    """Install baseline, candidate, then baseline again into one disposable home."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--agentics-repo", type=Path, required=True)
    parser.add_argument("--output-directory", type=Path, required=True)
    args = parser.parse_args()
    output = args.output_directory.absolute()
    output.mkdir(parents=True, exist_ok=False)
    output = output.resolve()
    home = output / "home"
    home.mkdir()
    codex_home = output / "codex-home"
    codex_home.mkdir()
    env = {"PATH": os.environ["PATH"], "HOME": str(home), "CODEX_HOME": str(codex_home),
           "TMPDIR": "/tmp", "LC_ALL": "C", "PYTHONDONTWRITEBYTECODE": "1"}
    commands = []

    def command(*arguments):
        result = subprocess.run(arguments, text=True, capture_output=True, env=env,
                                cwd=output, timeout=INSTALL_DEADLINE)
        commands.append({"argv": list(arguments), "exit_status": result.returncode,
                         "stdout": result.stdout, "stderr": result.stderr})
        (output / "commands.json").write_text(json.dumps(commands, indent=2) + "\n")
        if result.returncode:
            raise ValueError(f"command failed ({result.returncode}): {arguments!r}: {result.stderr}")
        return result.stdout

    version = command("codex", "--version").strip()
    sources = {}
    for label, commit in (("baseline", BASELINE), ("candidate", CANDIDATE)):
        market = output / f"{label} marketplace"
        plugin = market / "plugins/greenlight"
        source = export_source(args.agentics_repo, commit, plugin)
        source["marketplace_root"] = str(market)
        sources[label] = source
        manifest_dir = market / ".agents/plugins"
        manifest_dir.mkdir(parents=True)
        (manifest_dir / "marketplace.json").write_text(json.dumps({
            "name": "sh707-install", "interface": {"displayName": "SH-707 packaging fixture"},
            "plugins": [{"name": "greenlight", "source": {"source": "local", "path": "./plugins/greenlight"},
                         "policy": {"installation": "AVAILABLE", "authentication": "ON_INSTALL"},
                         "category": "Developer Tools"}]}))
    (output / "sources.json").write_text(json.dumps(sources, indent=2) + "\n")
    receipts = []
    for stage, label in (("baseline", "baseline"), ("candidate", "candidate"), ("rollback", "baseline")):
        source = sources[label]
        if receipts:
            command("codex", "plugin", "marketplace", "remove", "sh707-install", "--json")
        command("codex", "plugin", "marketplace", "add", source["marketplace_root"], "--json")
        outcome = json.loads(command("codex", "plugin", "add", "greenlight@sh707-install", "--json"))
        installed = Path(outcome["installedPath"])
        if not installed.resolve().is_relative_to(codex_home):
            raise ValueError(f"installer escaped disposable home: {installed}")
        config = tomllib.loads((codex_home / "config.toml").read_text())
        receipt = validate_install(source, installed, outcome, config)
        receipt.update(stage=stage, codex_version=version,
                       label="unreleased candidate packaging test; isolated installation only")
        receipt["listing"] = json.loads(command("codex", "plugin", "list", "--marketplace", "sh707-install", "--json"))
        receipt["contracts"] = exercise(installed, repaired=label == "candidate")
        # Replay must not alter the installed payload (e.g. bytecode or logs).
        if inventory(installed) != source["files"]:
            raise ValueError("installed bytes changed during contract replay")
        receipts.append(receipt)
        (output / "receipt.json").write_text(json.dumps(receipts, indent=2) + "\n")
        print(f"PASS {stage}: exact {source['commit']}, {len(receipt['contracts'])} manifest contracts", flush=True)
    print(f"Packaging and rollback evidence: {output}", flush=True)


if __name__ == "__main__":
    main()
