#!/usr/bin/env python3
"""Inventory or remove proven legacy fixture readers; never kill a tmux server."""

import argparse
import json
import os
from pathlib import PurePosixPath
import re
import shlex
import subprocess

FORMAT = "#{session_name}\t#{window_id}\t#{window_name}\t#{pane_id}\t#{pane_pid}\t#{pane_start_command}"
TIMEOUT = 5


def fixture_path(path):
    """Accept only known private test roots, not arbitrary temporary files."""
    path = str(PurePosixPath(path))
    return ".." not in PurePosixPath(path).parts and bool(re.match(
        r"^/(?:private/)?tmp/(?:storyhook-tests/storyhook-fixture-[^/]+|agentics-store\.[^/]+|storyhook-resource-probe-[^/]+)/", path))


def classify(row):
    """Require independent naming and reader-command evidence for deletion."""
    if len(row) != 6 or row[0] != "storyhook-verifier":
        return None
    if not re.fullmatch(r"@[0-9]+", row[1]) or not re.fullmatch(r"%[0-9]+", row[3]):
        return None
    try:
        argv = shlex.split(row[5])
    except ValueError:
        return None
    if re.fullmatch(r"activity-(?:storyhook|data)-[a-f0-9]{64}", row[2]):
        if (len(argv) == 6 and argv[1] == "--store-path" and fixture_path(argv[2])
                and argv[3:] == ["daemon", "logs", "--follow"] and os.path.basename(argv[0]) == "story"):
            return "fixture activity reader"
    if re.fullmatch(r"verification-storyhook-fixture-[A-Za-z0-9_-]+-[a-f0-9]{40}", row[2]):
        if (len(argv) == 5 and argv[:2] == ["bash", "-c"]
                and argv[2] == 'printf "%s\\n" "$1"; exec sleep 2147483647'
                and argv[3] == "verifier-banner"
                and argv[4].startswith("verifying https://github.com/acme/widgets/pull/")):
            return "fixture verification banner"
        if len(argv) == 5 and argv[:4] == ["tail", "-n", "+1", "-F"] and fixture_path(argv[4]):
            return "fixture verification tail"
    return None


def tmux(*args):
    """Run only bounded default-server control commands."""
    env = os.environ.copy()
    env.pop("TMUX", None)
    env.pop("TMUX_PANE", None)
    result = subprocess.run(["tmux", *args], env=env, capture_output=True, text=True,
                            timeout=TIMEOUT, check=True)
    return result.stdout


def inventory():
    """Keep full identity evidence for a later exact comparison."""
    return [line.split("\t", 5) for line in tmux("list-panes", "-a", "-F", FORMAT).splitlines()]


def cleanup(apply=False):
    """Recheck single-pane ownership immediately before each removal."""
    rows = inventory()
    result = []
    for row in rows:
        if row[0] != "storyhook-verifier":
            continue
        siblings = [other for other in rows if other[1] == row[1]]
        reason = classify(row) if len(siblings) == 1 else None
        item = dict(window=row[1], name=row[2], reason=reason, evidence=row, action="keep")
        if reason:
            item["action"] = "candidate"
            if apply:
                current = [other for other in inventory() if other[1] == row[1]]
                if current != [row]:
                    item["action"] = "changed; kept"
                else:
                    tmux("kill-window", "-t", row[1])
                    if any(other[1] == row[1] for other in inventory()):
                        raise RuntimeError(f"fixture window survived cleanup: {row[1]}")
                    item["action"] = "removed"
        result.append(item)
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--apply", action="store_true", help="remove identity-verified fixture windows")
    args = parser.parse_args()
    print(json.dumps(cleanup(args.apply), indent=2))
