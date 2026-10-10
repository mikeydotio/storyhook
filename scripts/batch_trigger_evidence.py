#!/usr/bin/env python3
"""Read retained SH-830 previews without contacting or changing StoryHook.

This descriptive report is neither a gate receipt nor enablement authority.
Inputs must come from one project's preview log, including its retained rotation.
"""
import argparse
from collections import Counter
from datetime import datetime
import hashlib
import json
from pathlib import Path
import re
import sys


def timestamp(value):
    if not isinstance(value, str):
        raise ValueError("missing timestamp")
    parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        raise ValueError("timestamp must include timezone")
    return parsed


def required_text(value, field):
    if not isinstance(value, str) or not value:
        raise ValueError(f"missing or invalid {field}")
    return value


def validate(record):
    if not isinstance(record, dict):
        raise ValueError("record must be an object")
    required_text(record.get("attempt_id"), "attempt_id")
    required_text(record.get("story_id"), "story_id")
    required_text(record.get("verdict"), "verdict")
    preview = record.get("preview")
    if not isinstance(preview, dict):
        raise ValueError("preview must be an object")
    if record["story_id"] != preview.get("head"):
        raise ValueError("record story does not match preview head")
    if timestamp(record.get("finished_at")) < timestamp(preview.get("computed_at")):
        raise ValueError("attempt finished before its preview")
    if type(preview.get("cap")) is not int or preview["cap"] < 1:
        raise ValueError("invalid preview cap")
    members = preview.get("members", [])
    if not isinstance(members, list):
        raise ValueError("members must be an array")
    seen = set()
    for member in members:
        if not isinstance(member, dict):
            raise ValueError("member must be an object")
        story = required_text(member.get("story_id"), "member story_id")
        commit = required_text(member.get("commit"), "member commit")
        if not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", commit):
            raise ValueError("member commit must be a full object identity")
        if story in seen:
            raise ValueError("duplicate story in members")
        seen.add(story)
        if "smoothed" in member:
            paths = member["smoothed"]
            if (not isinstance(paths, list)
                    or any(not isinstance(path, str) or not path for path in paths)):
                raise ValueError("smoothed must be an array of nonempty paths")
    if members and members[0]["story_id"] != record["story_id"]:
        raise ValueError("first member must be the head")
    if preview.get("outcome") not in {"batch", "head-conflict", "unavailable"}:
        raise ValueError("unsupported preview outcome")


def judged(record):
    preview = record["preview"]
    tree = record.get("gate_tree")
    if (preview.get("outcome") == "batch" and isinstance(tree, str)
            and re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", tree)
            and tree == preview.get("head_tree")
            and record["verdict"] in {"certified", "tests-failed"}):
        return "green" if record["verdict"] == "certified" else "red"
    return "unknown"


def join_member(member, preview, own_records):
    start = timestamp(preview["computed_at"])
    candidates = []
    for record in own_records.get(member["story_id"], []):
        candidate = record["preview"]
        if timestamp(candidate["computed_at"]) < start:
            continue
        members = candidate.get("members", [])
        # An intervening identity-less attempt cannot be skipped in favor of a
        # later green: its commit is unknown, so the member stays unresolved.
        if not members or members[0]["commit"] == member["commit"]:
            candidates.append(record)
    if not candidates:
        return dict(member, outcome="unknown", reason="no-later-exact-commit-attempt")
    first_time = timestamp(candidates[0]["preview"]["computed_at"])
    first = [r for r in candidates if timestamp(r["preview"]["computed_at"]) == first_time]
    if len(first) != 1:
        return dict(member, outcome="unknown", reason="ambiguous-first-attempt")
    record = first[0]
    if not record["preview"].get("members"):
        return dict(member, outcome="unknown", reason="earlier-attempt-has-no-commit")
    return dict(member, outcome=judged(record), attempt_id=record["attempt_id"],
                verdict=record["verdict"], reason="first-exact-commit-attempt")


def analyze(records):
    unique = {}
    duplicates = 0
    for record in records:
        validate(record)
        identity = record["attempt_id"]
        if identity in unique:
            if unique[identity] != record:
                raise ValueError(f"conflicting duplicate attempt: {identity}")
            duplicates += 1
        unique[identity] = record
    ordered = sorted(unique.values(), key=lambda r: (
        timestamp(r["preview"]["computed_at"]), r["attempt_id"]))
    own_records = {}
    for record in ordered:
        own_records.setdefault(record["story_id"], []).append(record)
    pairs = []
    for record in ordered:
        preview = record["preview"]
        members = preview.get("members", [])
        if (preview["cap"] < 2 or preview["outcome"] != "batch"
                or len(members) < 2 or any(m.get("smoothed") for m in members[:2])):
            continue
        joined = [join_member(m, preview, own_records) for m in members[:2]]
        outcomes = [m["outcome"] for m in joined]
        outcome = "green" if outcomes == ["green", "green"] else (
            "red" if "red" in outcomes else "unknown")
        pairs.append({"attempt_id": record["attempt_id"], "members": joined,
                      "outcome": outcome, "recorded_cap": preview["cap"]})
    counts = Counter(pair["outcome"] for pair in pairs)
    unresolved = sum(any(m["outcome"] == "unknown" for m in p["members"]) for p in pairs)
    # Every eligible dequeue remains in the denominator. Incomplete member
    # evidence prevents a qualified result even when the known lower bound wins.
    met = len(pairs) >= 30 and unresolved == 0 and counts["green"] * 2 >= len(pairs)
    heads = Counter(judged(r) for r in ordered)
    return {
        "schema": "storyhook.batch-trigger-evidence.v1",
        "activation_authorized": False,
        "scope": "Historical offline evidence; not certification, current-cohort approval or rollout authority",
        "records": len(ordered), "identical_duplicate_records": duplicates,
        "last_finished_at": max((r["finished_at"] for r in ordered), key=timestamp, default=None),
        "pairs": {
            "cap": 2, "eligible_dequeues": len(pairs), "green": counts["green"],
            "red": counts["red"], "unknown": counts["unknown"],
            "pairs_with_unresolved_members": unresolved,
            "green_fraction": counts["green"] / len(pairs) if pairs else None,
            "observed_threshold_met": met, "minimum_dequeues": 30,
            "required_green_fraction": 0.5, "details": pairs,
        },
        "heads": {"green": heads["green"], "red": heads["red"], "unknown": heads["unknown"]},
        "submissions": {
            "status": "submission-provenance-unavailable", "observed_threshold_met": False,
            "required_green_fraction": 0.62,
            "reason": "Retained previews do not prove the first attempt at submission; rotation, missing attempts and retries must be reconciled with submission history before applying the bisection trigger.",
        },
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("inputs", nargs="+", type=Path, help="one project's explicit NDJSON preview files")
    args = parser.parse_args()
    try:
        records, sources = [], []
        for path in args.inputs:
            data = path.read_bytes()
            sources.append({"name": path.name, "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()})
            for line in data.splitlines():
                if line.strip():
                    records.append(json.loads(line))
        report = analyze(records)
        report["sources"] = sources
    except (OSError, ValueError, TypeError, KeyError) as error:
        print(f"batch evidence refused: {error}", file=sys.stderr)
        return 2
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
