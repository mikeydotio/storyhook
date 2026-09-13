"""Prove personal-artifact replacement and rollback preserve unrelated plugins.

All configuration and trust witnesses are synthetic, created in a fresh home.
The real Codex installer owns every subsequent configuration/cache mutation.
No model session is started and no native trust acceptance is simulated.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tomllib

from development_artifact import validate_overlay
from fixture_config_api import toggle
from installed_contract import exercise
from packaging_evidence import export_source, inventory
from smoke_install import BASELINE, INSTALL_DEADLINE
from stage_personal import stage

# Represent every unrelated enabled Agentics plugin observed for SH-707.
UNRELATED = ("council", "freshen", "agents", "rca", "semver", "reconcile-pr", "deployit", "hook-guard")


def rehearse(repo, artifact_directory, output, preserve_caches=False):
    """Return real installer transitions, file witnesses and trust preservation."""
    receipt = json.loads((artifact_directory / "provenance.json").read_text())
    archive = artifact_directory / "greenlight-sh707-development.tar.gz"
    if hashlib.sha256(archive.read_bytes()).hexdigest() != receipt["artifact_sha256"]:
        raise ValueError("artifact archive identity mismatch")
    output.mkdir(parents=True, exist_ok=False)
    output = output.resolve()
    home, codex_home, market = output / "home", output / "codex-home", output / "agentics"
    home.mkdir()
    codex_home.mkdir()
    baseline = export_source(repo, BASELINE, market / "plugins/greenlight")
    entries = []
    for name in ("greenlight", *UNRELATED):
        plugin = market / "plugins" / name
        if name != "greenlight":
            (plugin / ".claude-plugin").mkdir(parents=True)
            (plugin / ".claude-plugin/plugin.json").write_text(json.dumps({"name": name, "version": "3.9.1"}))
            (plugin / "witness.txt").write_text(f"Unrelated {name} fixture; must remain byte-identical.\n")
        entries.append({"name": name, "source": f"./plugins/{name}"})
    (market / ".claude-plugin").mkdir()
    (market / ".claude-plugin/marketplace.json").write_text(json.dumps({
        "name": "agentics", "owner": {"name": "fixture"}, "plugins": entries}))
    # Seed a new test config before installation, never patch an existing config.
    trust = {f"{name}@agentics:hooks/hooks.json:pre_tool_use:0:0": {"trusted_hash": "sha256:" + "a" * 64}
             for name in ("greenlight", *UNRELATED)}
    (codex_home / "config.toml").write_text("\n".join(
        f'[hooks.state.{json.dumps(key)}]\ntrusted_hash = {json.dumps(value["trusted_hash"])}\n'
        for key, value in trust.items()))
    env = {"PATH": os.environ["PATH"], "HOME": str(home), "CODEX_HOME": str(codex_home),
           "TMPDIR": "/tmp", "LC_ALL": "C", "PYTHONDONTWRITEBYTECODE": "1"}
    commands, states = [], []

    def run(*args):
        result = subprocess.run(args, text=True, capture_output=True, env=env, cwd=output,
                                timeout=INSTALL_DEADLINE)
        commands.append({"argv": list(args), "exit_status": result.returncode,
                         "stdout": result.stdout, "stderr": result.stderr})
        (output / "commands.json").write_text(json.dumps(commands, indent=2) + "\n")
        if result.returncode:
            raise ValueError(f"installer command failed: {commands[-1]!r}")
        return json.loads(result.stdout)

    run("codex", "plugin", "marketplace", "add", str(market), "--json")
    installs = {name: run("codex", "plugin", "add", f"{name}@agentics", "--json")
                for name in ("greenlight", *UNRELATED)}
    config_before = tomllib.loads((codex_home / "config.toml").read_text())
    witnesses = {name: inventory(Path(installs[name]["installedPath"])) for name in UNRELATED}

    def observe(stage, expected_producers):
        config = tomllib.loads((codex_home / "config.toml").read_text())
        if config["marketplaces"] != config_before["marketplaces"]:
            raise ValueError("Agentics registration changed")
        if config["hooks"]["state"] != trust:
            raise ValueError("existing native trust witness changed")
        for name in UNRELATED:
            key = f"{name}@agentics"
            if config["plugins"][key] != config_before["plugins"][key]:
                raise ValueError(f"unrelated enabled state changed: {key}")
            if inventory(Path(installs[name]["installedPath"])) != witnesses[name]:
                raise ValueError(f"unrelated installed bytes changed: {key}")
        producers = sorted(key for key, value in config["plugins"].items()
                           if key.startswith("greenlight@") and value.get("enabled"))
        if producers != sorted(expected_producers):
            raise ValueError(f"wrong active Greenlight producer set: {producers}")
        state = {"stage": stage, "enabled_producers": producers, "config": config,
                 "unrelated_file_witnesses": witnesses, "native_host_validated": False}
        states.append(state)
        (output / "states.json").write_text(json.dumps(states, indent=2) + "\n")
        print(f"PASS {stage}: {producers}; {len(UNRELATED)} unrelated plugins and trust preserved", flush=True)

    observe("baseline", ["greenlight@agentics"])
    # Exercise the exact proposed staging entrypoint, including refusal preflight.
    stage(artifact_directory, home, receipt["artifact_sha256"])
    if inventory(home) != receipt["payload_files"]:
        raise ValueError("extracted personal artifact inventory differs")
    delta = validate_overlay(receipt["source"], home / "plugins/greenlight")
    personal = run("codex", "plugin", "add", "greenlight@personal", "--json")
    installed = Path(personal["installedPath"])
    if personal["version"] != delta["version"] or personal["pluginId"] != "greenlight@personal":
        raise ValueError("Codex did not select the development overlay identity")
    if inventory(installed) != delta["files"]:
        raise ValueError("development installation byte/mode parity failed")
    contracts = exercise(installed, repaired=True)
    observe("personal-added-duplicates-exist", ["greenlight@agentics", "greenlight@personal"])
    toggles = []
    if preserve_caches:
        toggles.extend(toggle(env, output, "greenlight@agentics", False, reject_stale=True))
    else:
        # Historical packaging evidence; removal is unsuitable for running live sessions.
        run("codex", "plugin", "remove", "greenlight@agentics", "--json")
    observe("development-selected", ["greenlight@personal"])
    if preserve_caches:
        if inventory(Path(installs["greenlight"]["installedPath"])) != baseline["files"]:
            raise ValueError("original cache changed during native-style selection")
        toggles.extend(toggle(env, output, "greenlight@agentics", True))
        restored = installs["greenlight"]
    else:
        restored = run("codex", "plugin", "add", "greenlight@agentics", "--json")
    if inventory(Path(restored["installedPath"])) != baseline["files"]:
        raise ValueError("rollback baseline identity differs")
    observe("baseline-restored-duplicates-exist", ["greenlight@agentics", "greenlight@personal"])
    if preserve_caches:
        toggles.extend(toggle(env, output, "greenlight@personal", False))
    else:
        run("codex", "plugin", "remove", "greenlight@personal", "--json")
    observe("rollback-complete", ["greenlight@agentics"])
    if preserve_caches:
        if (inventory(installed) != delta["files"]
                or inventory(Path(restored["installedPath"])) != baseline["files"]):
            raise ValueError("rollback did not preserve both exact caches")
    report = {"artifact_sha256": receipt["artifact_sha256"], "version": delta["version"],
              "states": states, "contracts": contracts, "native_trust_required": True,
              "release_certified": False, "live_changes_performed": False,
              "both_caches_preserved": preserve_caches, "toggle_rpc_transcript": toggles}
    (output / "receipt.json").write_text(json.dumps(report, indent=2) + "\n")
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--agentics-repo", type=Path, required=True)
    parser.add_argument("--artifact-directory", type=Path, required=True)
    parser.add_argument("--output-directory", type=Path, required=True)
    parser.add_argument("--preserve-caches", action="store_true")
    args = parser.parse_args()
    rehearse(args.agentics_repo, args.artifact_directory, args.output_directory, args.preserve_caches)
