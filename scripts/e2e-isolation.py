#!/usr/bin/env python3
"""Plan exact file slices and validate their durable execution evidence (SH-814)."""
import hashlib
import json
from pathlib import Path
import re
import shlex
import sys


def plan(text, directory, projects):
    """Validate the whole selection before writing one test-list per project/file."""
    rows = []
    seen = set()
    if not projects or any(not re.fullmatch(r"[A-Za-z0-9._-]+", p) for p in projects):
        raise ValueError("invalid selected projects")
    for line in text.splitlines():
        parts = line.split("\t")
        if len(parts) != 3:
            raise ValueError(f"malformed count line: {line!r}")
        project, file, count = parts
        if (project not in projects or not file or Path(file).is_absolute()
                or ".." in Path(file).parts or any(ord(c) < 32 for c in file)
                or not re.fullmatch(r"[1-9][0-9]*", count)):
            raise ValueError(f"invalid selected file: {line!r}")
        identity = project + "\t" + file
        if identity in seen:
            raise ValueError(f"duplicate selected file: {identity}")
        seen.add(identity)
        name = project + ".isolate." + hashlib.sha256(identity.encode()).hexdigest()[:20]
        rows.append(dict(slice=name, project=project, file=file, count=int(count),
                         list=str((directory / (name + ".list")).resolve())))
    if not rows:
        raise ValueError("empty isolation selection")
    for row in rows:
        Path(row["list"]).write_text(f'[{row["project"]}] › {row["file"]}\n')
    (directory / "manifest.json").write_text(json.dumps(rows, indent=2) + "\n")
    return rows


def specs(suites):
    """Traverse Playwright's nested JSON suites without discarding grouped tests."""
    for suite in suites:
        yield from suite.get("specs", [])
        yield from specs(suite.get("suites", []))


def attempt(root, row):
    """Distinguish a complete test failure from missing or invalid harness evidence."""
    name = row["slice"]
    result = {"exit_code": None, "complete": False, "artifacts": str(root / name)}
    try:
        result["exit_code"] = int((root / "verdicts" / name).read_text())
        count = int((root / "selected" / name).read_text())
        executed = int((root / "executed" / name).read_text())
        report = json.loads((root / "reports" / (name + ".json")).read_text())
        if count != row["count"] or report.get("errors"):
            raise ValueError("selection count differs or Playwright reported an infrastructure error")
        found = []
        for spec in specs(report["suites"]):
            for test in spec["tests"]:
                if spec["file"] != row["file"] or test["projectName"] != row["project"]:
                    raise ValueError("Playwright executed a different project/file")
                if not test["results"] or any(r["status"] not in
                        {"passed", "failed", "timedOut", "skipped"} for r in test["results"]):
                    raise ValueError("missing or interrupted test result")
                found.append(test)
        if len(found) != count:
            raise ValueError("Playwright execution count differs from the plan")
        unexpected = any(t["status"] not in {"expected", "skipped"} for t in found)
        if executed == 0 and unexpected:
            raise ValueError("successful process reported failed tests")
        if executed != 0 and not unexpected:
            raise ValueError("failed process has no failed test evidence")
        # A green Playwright followed by a red daemon/dispatch post-check is infrastructure.
        if result["exit_code"] != 0 and executed == 0:
            raise ValueError("fixture post-check failed after Playwright passed")
        result["complete"] = True
    except (OSError, ValueError, KeyError, TypeError) as error:
        result["error"] = str(error)
    return result


def report(root):
    """Summarize both attempts; a serial success can never erase an initial failure."""
    rows = json.loads((root / "slices/manifest.json").read_text())
    if not rows:
        raise ValueError("empty isolation manifest")
    failures = []
    for row in rows:
        initial = attempt(root, row)
        if initial["complete"] and initial["exit_code"] == 0:
            continue
        rerun = attempt(root / "reruns", row)
        outcome = "rerun infrastructure failure"
        if rerun["complete"]:
            outcome = "failed again" if rerun["exit_code"] else "not reproduced on serial rerun"
        command = "STORYHOOK_E2E_JOBS=1 " + shlex.join([
            "bash", "scripts/run-e2e.sh", "--project=" + row["project"],
            "--test-list=" + row["list"]])
        failures.append({**row, "initial": initial, "rerun": rerun,
                         "outcome": outcome, "rerun_command": command})
    return {"schema": 1, "selected_tests": sum(r["count"] for r in rows),
            "selected_files": len(rows), "failures": failures, "exit_code": int(bool(failures))}


def main():
    """Provide the runner's plan, pending-rerun and final-report entry points."""
    if len(sys.argv) >= 4 and sys.argv[1] == "plan":
        for row in plan(sys.stdin.read(), Path(sys.argv[2]), sys.argv[3:]):
            print("\t".join(str(row[k]) for k in ("slice", "project", "list", "count")))
        return 0
    if len(sys.argv) == 3 and sys.argv[1] in {"failed", "report"}:
        root = Path(sys.argv[2])
        result = report(root)
        if sys.argv[1] == "failed":
            for failure in result["failures"]:
                print(failure["slice"])
            return 0
        (root / "isolation.json").write_text(json.dumps(result, indent=2) + "\n")
        for failure in result["failures"]:
            print(f'isolation: [{failure["project"]}] {failure["file"]}: {failure["outcome"]}')
            print(f'  initial: {failure["initial"]}; rerun: {failure["rerun"]}')
            print(f'  rerun from this checkout: {failure["rerun_command"]}')
        print(f'isolation: {result["selected_tests"]} tests in {result["selected_files"]} files; '
              f'{len(result["failures"])} initial failures; report={root / "isolation.json"}')
        return result["exit_code"]
    raise ValueError("usage: e2e-isolation.py plan LIST_DIR PROJECT... | failed|report RESULTS")


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"e2e-isolation: {error}", file=sys.stderr)
        sys.exit(2)
