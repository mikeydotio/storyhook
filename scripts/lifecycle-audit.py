#!/usr/bin/env python3
"""Collect immutable project evidence or regenerate the offline SH-560 report."""

import argparse
import hashlib
import json
from pathlib import Path
import sqlite3
import sys

# macOS's system Python predates tomllib; collection must also work offline there.
if sys.version_info >= (3, 11):
    import tomllib
else:
    from _vendor import tomli as tomllib

from lifecycle_audit import analyze, archived_json, embedded_json, timestamp

ROOT = Path(__file__).resolve().parents[1]
REPORTS = ROOT / "docs/reports"
START = "2026-09-06T04:45:28Z"
END = "2026-09-08T04:45:28Z"
WATERMARK = 22571


def collect(store, pointer):
    """Read only this project's frozen event prefix in one SQLite transaction."""
    project_uuid = tomllib.loads(pointer.read_text())["uuid"]
    connection = sqlite3.connect(store.resolve().as_uri() + "?mode=ro", uri=True)
    try:
        connection.execute("PRAGMA query_only=ON")
        connection.execute("BEGIN")
        project = connection.execute("SELECT id FROM projects WHERE uuid=?", (project_uuid,)).fetchone()
        if not project:
            raise ValueError(f"project UUID {project_uuid} absent from store")
        rows = connection.execute(
            "SELECT story_no,seq,global_seq,at,kind,payload,command,actor FROM events "
            "WHERE project_id=? AND global_seq<=? ORDER BY global_seq", (project[0], WATERMARK)).fetchall()
    finally:
        connection.close()
    events = [dict(story_no=n, seq=s, global_seq=g, at=at, kind=k,
                   payload=json.loads(p), command=command, actor=actor)
              for n, s, g, at, k, p, command, actor in rows
              if timestamp(at) is None or timestamp(at) <= timestamp(END)]
    cohort = analyze(events, START, END)
    ids = {int(s["id"][3:]) for s in cohort}
    selected = [e for e in events if e["story_no"] in ids or e["story_no"] == 560]
    return dict(schema=1, project_uuid=project_uuid, window_start=START, window_end=END,
                global_seq_cutoff=WATERMARK, collection="SQLite read-only transaction",
                events=selected, cohort_ids=[s["id"] for s in cohort])


def render(evidence, findings):
    """Validate findings against frozen evidence and embed the deterministic model."""
    stories = analyze(evidence["events"], evidence["window_start"], evidence["window_end"])
    if [s["id"] for s in stories] != evidence["cohort_ids"]:
        raise ValueError("cohort manifest differs from event reconstruction")
    event_ids = {e["global_seq"] for e in evidence["events"]}
    finding_ids = set()
    for finding in findings["findings"]:
        if finding["id"] in finding_ids:
            raise ValueError("duplicate finding ID")
        finding_ids.add(finding["id"])
        for reference in finding["evidence"]:
            if reference not in event_ids:
                raise ValueError(f"{finding['id']}: missing evidence event {reference}")
    payload = dict(metadata={k: v for k, v in evidence.items() if k != "events"},
                   stories=stories, supplemental=[e for e in evidence["events"] if e["story_no"] == 560],
                   assessment=findings)
    template = Path(__file__).with_suffix(".html").read_text()
    return template.replace("__AUDIT_DATA__", embedded_json(payload))


def main():
    """Run an explicit collection or deterministic report generation operation."""
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    capture = sub.add_parser("collect")
    capture.add_argument("--store", type=Path, required=True)
    capture.add_argument("--pointer", type=Path, default=ROOT / ".storyhook.toml")
    capture.add_argument("--output", type=Path, required=True)
    generate = sub.add_parser("render")
    generate.add_argument("--evidence", type=Path, default=REPORTS / "SH-560-evidence.json")
    generate.add_argument("--findings", type=Path, default=REPORTS / "SH-560-findings.json")
    generate.add_argument("--output", type=Path, default=REPORTS / "SH-560-lifecycle-audit.html")
    args = parser.parse_args()
    if args.command == "collect":
        result = collect(args.store, args.pointer)
        content = archived_json(result)
    else:
        result = json.loads(args.evidence.read_text())
        findings = json.loads(args.findings.read_text())
        content = render(result, findings)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(content)
    print(f"{args.output}: {len(content.encode())} bytes; sha256 {hashlib.sha256(content.encode()).hexdigest()}")


if __name__ == "__main__":
    main()
