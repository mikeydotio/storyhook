"""Validate slice receipts and maintain disposable, worktree-shared timing history."""

from decimal import Decimal
import fcntl
import json
import math
import os
from pathlib import Path
import sys
import tempfile


def warn(message):
    """Name the scheduling cache separately from a test failure."""
    print(f"e2e-durations: {message}", file=sys.stderr)


def positive(value):
    """Reject booleans, nonfinite values and impossible scheduling magnitudes."""
    return type(value) in (int, float) and math.isfinite(value) and 0 < value <= 1e12


def valid_key(project, file):
    """Keys must fit one TSV row and use a checkout-relative POSIX path."""
    return (isinstance(project, str) and isinstance(file, str) and bool(project) and bool(file)
            and not any(c in project + file for c in "\t\r\n")
            and not Path(file).is_absolute() and ".." not in Path(file).parts)


def read_history(path):
    """Read valid version-one rows; malformed optional history is diagnosed."""
    result = {}
    try:
        lines = Path(path).read_text().splitlines()
    except FileNotFoundError:
        return result
    except (OSError, UnicodeError) as error:
        warn(f"cannot read history {path}: {error}; using test counts")
        return result
    for number, line in enumerate(lines, 1):
        try:
            version, project, file, count, seconds, observed = line.split("\t")
            count, seconds, observed = int(count), float(seconds), float(observed)
            if (version != "1" or not valid_key(project, file) or count <= 0
                    or not positive(seconds) or not positive(observed)):
                raise ValueError("invalid fields")
            key = (project, file)
            if key not in result or observed > result[key][2]:
                result[key] = (count, seconds, observed)
        except ValueError as error:
            warn(f"invalid history {path}:{number}: {error}; ignoring row")
    return result


def weights(path, stream):
    """Enrich counts without changing the selection, using exact count matches."""
    history = read_history(path)
    for line in stream:
        project, file, count_text = line.rstrip("\n").split("\t")
        count = int(count_text)
        record = history.get((project, file))
        seconds = record[1] if record and record[0] == count else count
        text = format(Decimal(str(seconds)), "f")
        if "." in text:
            text = text.rstrip("0").rstrip(".")
        print(f"{project}\t{file}\t{count}\t{text}")


def manifest(path):
    """Read exact project/file counts and refuse duplicate or empty selections."""
    result = {}
    for line in Path(path).read_text().splitlines():
        project, file, count_text = line.split("\t")
        count = int(count_text)
        key = (project, file)
        if not valid_key(project, file) or count <= 0 or key in result:
            raise ValueError(f"invalid or duplicate selection in {path}: {line!r}")
        result[key] = count
    if not result:
        raise ValueError(f"empty selection: {path}")
    return result


def read_report(path):
    """Validate receipt structure independently of the reporter's exit status."""
    report = json.loads(Path(path).read_text())
    if (not isinstance(report, dict) or report.get("version") != 1
            or report.get("status") not in ("passed", "failed", "timedout", "interrupted")
            or not positive(report.get("observed")) or not isinstance(report.get("errors"), list)
            or not isinstance(report.get("files"), list)):
        raise ValueError(f"invalid completed report: {path}")
    seen = set()
    for row in report["files"]:
        if not isinstance(row, dict) or not valid_key(row.get("project"), row.get("file")):
            raise ValueError(f"invalid file record: {path}")
        key = (row["project"], row["file"])
        if key in seen:
            raise ValueError(f"duplicate file record: {path}: {key}")
        seen.add(key)
        for field in ("count", "completed", "passed", "skipped", "retries"):
            if type(row.get(field)) is not int or row[field] < 0:
                raise ValueError(f"invalid {field}: {path}: {key}")
        seconds = row.get("seconds")
        if (row["count"] == 0 or type(seconds) not in (int, float)
                or not math.isfinite(seconds) or not 0 <= seconds <= 1e12):
            raise ValueError(f"invalid count/duration: {path}: {key}")
    return report


def validate(path, expected):
    """Fail closed on missing evidence, changed discovery or incomplete success."""
    report = read_report(path)
    rows = report["files"]
    actual = {(row["project"], row["file"]): row["count"] for row in rows}
    if actual != manifest(expected):
        raise ValueError(f"selection mismatch: {path} versus {expected}")
    if report["status"] != "passed" or report["errors"]:
        raise ValueError(f"unsuccessful slice report {path}: {report['status']}: {report['errors']}")
    if any(row["completed"] != row["count"] or row["passed"] + row["skipped"] != row["count"]
           or row["retries"] for row in rows):
        raise ValueError(f"incomplete or retried slice: {path}")


def merge(path, reports):
    """Merge completed successful observations under a nonblocking process lock."""
    updates = {}
    for source in reports:
        try:
            report = read_report(source)
        except (OSError, ValueError, TypeError) as error:
            warn(f"cannot learn from {source}: {error}")
            continue
        if report["status"] != "passed" or report["errors"]:
            continue
        for row in report["files"]:
            if (row["completed"] == row["count"] == row["passed"] and not row["skipped"]
                    and not row["retries"] and positive(row["seconds"])):
                key = (row["project"], row["file"])
                value = (row["count"], row["seconds"], report["observed"])
                if key not in updates or value[2] > updates[key][2]:
                    updates[key] = value
    if not updates:
        return
    temporary = None
    try:
        Path(path).parent.mkdir(parents=True, exist_ok=True)
        with open(f"{path}.lock", "a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            current = read_history(path)
            for key, value in updates.items():
                if key not in current or value[2] > current[key][2]:
                    current[key] = value
            with tempfile.NamedTemporaryFile(mode="w", dir=Path(path).parent,
                                             prefix="e2e-durations-", delete=False) as output:
                temporary = output.name
                for (project, file), (count, seconds, observed) in sorted(current.items()):
                    output.write(f"1\t{project}\t{file}\t{count}\t{seconds}\t{observed}\n")
            os.replace(temporary, path)
            temporary = None
    except OSError as error:
        warn(f"cannot update history {path}: {error}; test verdict is unchanged")
    finally:
        if temporary is not None:
            try:
                os.unlink(temporary)
            except OSError as error:
                warn(f"cannot remove temporary history {temporary}: {error}")


def main(args):
    """Serve weights, validation and merge requests from the isolated runner."""
    try:
        command, path, *rest = args
        if command == "weights" and not rest:
            weights(path, sys.stdin)
        elif command == "validate" and len(rest) == 1:
            validate(path, rest[0])
        elif command == "merge":
            merge(path, rest)
        else:
            raise ValueError("usage: e2e-durations.py weights CACHE | validate REPORT EXPECTED | merge CACHE REPORT...")
    except (OSError, ValueError, TypeError) as error:
        warn(str(error))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
