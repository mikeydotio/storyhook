#!/usr/bin/env python3
"""The portable progress writer a project gate calls (SH-777).

The verifier supplies this executable's absolute path to every project gate
as STORYHOOK_GATE_PROGRESS_WRITER, beside the SH-665 receipt writer
(`scripts/merge-watch.sh`). A gate reports its own legs and test cases
through it; it never writes the SH-524 journal format by hand:

    "$STORYHOOK_GATE_PROGRESS_WRITER" leg start unit
    "$STORYHOOK_GATE_PROGRESS_WRITER" case unit pass
    "$STORYHOOK_GATE_PROGRESS_WRITER" leg pass unit

`case <leg> pass|fail [name]` can retain the literal case identity. Names are
UTF-8 data and JSON-escaped; the stricter checklist path rules do not apply.
`cost start|end <phase> <id> <leg>` records a monotonic phase boundary. Pair
the same ID at both boundaries, and use a new ID for each interval. Missing
ends remain unknown. These are real work boundaries, never timer heartbeats.

`leg start|pass|fail|skip <leg>` sets the checklist row "release gate/<leg>"
to running, passed, failed or skipped. `case <leg> pass|fail` counts one
finished test under that row. A leg may nest ("unit/Parser"). Every row is
below "release gate/", so a gate can never address "merge preflight" or
"release gate" itself -- the verifier's own recovery evidence
(`GateProgress::reached_verification_gate`).

Each line is a journal append, and journal growth is what renews the
verifier's silence watchdog (`scripts/machine-lock.sh`, SH-536). Raw
stdout/stderr never renews it (SH-713), so a gate that reports nothing must
finish within the silence ceiling; `story help project-settings` states it.

EXIT STATUS
  0  the line was appended, or STORYHOOK_GATE_PROGRESS is unset (a local run
     outside verification: the same no-op contract as gate-progress.sh)
  1  the journal is missing or cannot be written; nothing was appended
  2  the call was refused before anything was written: unknown verb, wrong
     arity, or a leg that is not safe to record

A LEG IS REFUSED, NOT REPAIRED, when it is not strict UTF-8, holds a control
character, has an empty segment (a leading, trailing or doubled "/"), or is
longer than LEG_LIMIT bytes. One invalid byte would make the daemon's
read_to_string fail for the whole journal, and a complete malformed line
invalidates the attempt's recovery evidence (gate_progress::fold), so the
only safe answer to a bad leg is to write nothing. Arguments are checked
before the environment, so a wrong call fails the same way in a local run.

Each line is one os.write() to a local regular file opened O_APPEND. The
journal is never created here: the verifier prepares it, and a missing one
means this run is not the verifier's.
"""

import datetime
import json
import os
import sys
import time

# Upper bound on a checklist path, independent of literal case-name length.
LEG_LIMIT = 256

ITEM_STATUS = {"start": "running", "pass": "passed", "fail": "failed", "skip": "skipped"}
CASE_OUTCOMES = ("pass", "fail")
ROOT = "release gate"
USAGE = (
    "usage: gate-progress-writer.py leg start|pass|fail|skip <leg> | "
    "gate-progress-writer.py case <leg> pass|fail [name] | "
    "gate-progress-writer.py cost start|end <phase> <id> <leg>"
)


class Refused(Exception):
    """A call that must not write anything."""


def leg_path(raw):
    """The journal path for one leg argument, or Refused with the reason."""
    try:
        leg = os.fsencode(raw).decode("utf-8")
    except UnicodeDecodeError as error:
        raise Refused(f"leg is not valid UTF-8: {error}") from None
    if not leg:
        raise Refused("leg is empty")
    if len(leg.encode("utf-8")) > LEG_LIMIT:
        raise Refused(f"leg is longer than {LEG_LIMIT} bytes")
    if any(ord(char) < 0x20 or ord(char) == 0x7F for char in leg):
        raise Refused(f"leg contains a control character: {leg!r}")
    if any(segment == "" for segment in leg.split("/")):
        raise Refused(f"leg has an empty path segment: {leg!r}")
    return f"{ROOT}/{leg}"


def record(argv):
    """The one journal record `argv` asks for, or Refused."""
    if len(argv) == 5 and argv[0] == "cost":
        _, event, phase, identity, path = argv
        if event not in ("start", "end") or phase not in (
                "workspace", "resource-wait", "discovery", "compile-link",
                "execution", "cleanup", "verdict"):
            raise Refused("invalid cost event or phase")
        leg_path(identity)
        return dict(kind="cost", event=event, phase=phase, id=identity,
                    path=leg_path(path), monotonic_ns=time.monotonic_ns(),
                    at=datetime.datetime.now(datetime.timezone.utc).isoformat())
    if len(argv) not in (3, 4) or (len(argv) == 4 and argv[0] != "case"):
        raise Refused(USAGE)
    verb, first, second = argv[:3]
    if verb == "leg":
        status = ITEM_STATUS.get(first)
        if status is None:
            raise Refused(f"unknown leg status {first!r}; {USAGE}")
        now = datetime.datetime.now(datetime.timezone.utc)
        return {
            "kind": "item",
            "path": leg_path(second),
            "status": status,
            "at": now.strftime("%Y-%m-%dT%H:%M:%SZ"),
        }
    if verb == "case":
        if second not in CASE_OUTCOMES:
            raise Refused(f"unknown case outcome {second!r}; {USAGE}")
        result = {"kind": "case", "path": leg_path(first), "outcome": second}
        if len(argv) == 4:
            # Case identities are data, not checklist paths. JSON escapes controls.
            try:
                result["name"] = os.fsencode(argv[3]).decode("utf-8")
            except UnicodeDecodeError as error:
                raise Refused(f"case name is not valid UTF-8: {error}") from None
        return result
    raise Refused(f"unknown verb {verb!r}; {USAGE}")


def append(journal, line):
    """Appends `line` to an existing journal with one write, or raises OSError."""
    data = line.encode("ascii")
    descriptor = os.open(journal, os.O_WRONLY | os.O_APPEND)
    try:
        written = os.write(descriptor, data)
    finally:
        os.close(descriptor)
    if written != len(data):
        raise OSError(f"short write: {written} of {len(data)} bytes")


def main(argv):
    try:
        line = json.dumps(record(argv), ensure_ascii=True, separators=(",", ":")) + "\n"
    except Refused as refusal:
        print(f"gate-progress-writer: {refusal}", file=sys.stderr)
        return 2
    journal = os.environ.get("STORYHOOK_GATE_PROGRESS", "")
    if not journal:
        return 0
    try:
        append(journal, line)
    except OSError as error:
        print(
            f"gate-progress-writer: could not append to the progress journal {journal}: {error}",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
