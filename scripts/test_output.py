#!/usr/bin/env python3
"""Parse Cargo/libtest text once for live progress and result ledgers."""

import re
import sys


RUNNING = re.compile(r"^     Running (.+?)(?: \(|$)")
CASE = re.compile(r"^test (.+) \.\.\. (ok|FAILED)$")
BUILD = re.compile(r"^\s*(Compiling|Checking|Finished) (.+)$")


class TestOutputParser:
    """Recognize conservative Cargo milestones and completed libtest cases."""

    def __init__(self):
        self.current = ""

    def parse(self, line):
        """Return trusted progress events recognized in one output line."""
        events = []
        running = RUNNING.match(line)
        if running:
            source = running.group(1)
            name = source.rsplit("/", 1)[-1]
            if name.endswith(".rs"):
                name = name[:-3]
            self.current = name
            events.append(("stage", f"running {source}"))
            return events

        case = CASE.match(line)
        if case:
            outcome = "pass" if case.group(2) == "ok" else "fail"
            events.append(("case", self.current or "(unknown)", case.group(1), outcome))
            return events

        build = BUILD.match(line)
        if build:
            events.append(("stage", f"{build.group(1).lower()} {build.group(2)}"))
        return events


def parse_stream(source, destination):
    """Render completed cases as binary, name, and uppercase outcome TSV."""
    parser = TestOutputParser()
    for raw in source:
        line = raw.decode("utf-8", errors="replace").rstrip("\r\n")
        for event in parser.parse(line):
            if event[0] == "case":
                _, binary, name, outcome = event
                destination.write(f"{binary}\t{name}\t{outcome.upper()}\n")


if __name__ == "__main__":
    parse_stream(sys.stdin.buffer, sys.stdout)
