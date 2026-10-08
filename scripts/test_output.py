#!/usr/bin/env python3
"""Parse Cargo/libtest text once for live progress and result ledgers."""

import re
import shlex
import sys


RUNNING = re.compile(r"^     Running (.+?) \(.+\)$")
CASE = re.compile(r"^test (.+) \.\.\. (ok|FAILED)$")
BUILD = re.compile(r"^\s*(Compiling|Checking|Finished) (.+)$")
ARTIFACT = re.compile(r"^(.+)-[0-9a-f]{16}(?:\.exe)?$")
ENV_ASSIGNMENT = re.compile(r"^[A-Za-z_][A-Za-z_0-9]*=")


def running_target(line):
    """Return source/target for normal headers or a verbose Cargo test command.

    Verbose commands identify the executable, not the source file. Strip only
    Cargo's artifact suffix, never arguments or an arbitrary hyphenated name.
    Library source aliases such as lib.rs cannot be recovered from this format.
    """
    prefix = "     Running "
    if not line.startswith(prefix):
        return None
    command = line[len(prefix):]
    if command.startswith("`"):
        if not command.endswith("`"):
            return None
        try:
            args = shlex.split(command[1:-1])
        except ValueError:
            return None
        while args and ENV_ASSIGNMENT.match(args[0]):
            args.pop(0)
        # The repository's Cargo runner execs exactly the command after --.
        if args and args[0].rsplit("/", 1)[-1] == "host-admit.py":
            if args[1:4] != ["--entry", "cargo-test-binary", "--"]:
                return None
            args = args[4:]
        if not args:
            return None
        executable = args[0]
        parts = executable.replace("\\", "/").rsplit("/", 2)
        if len(parts) < 2 or parts[-2] != "deps":
            return None
        artifact = ARTIFACT.fullmatch(parts[-1])
        return (executable, artifact.group(1)) if artifact else None
    running = RUNNING.match(line)
    if not running:
        return None
    source = running.group(1)
    name = source.rsplit("/", 1)[-1]
    return source, name[:-3] if name.endswith(".rs") else name


class TestOutputParser:
    """Recognize conservative Cargo milestones and completed libtest cases."""

    def __init__(self):
        self.current = ""

    def parse(self, line):
        """Return trusted progress events recognized in one output line."""
        events = []
        running = running_target(line)
        if running is not None:
            source, name = running
            self.current = name
            events.append(("stage", f"running {source}"))
            return events
        if line.startswith("     Running "):
            # A compiler/build-script/unknown runner is not a test target, and
            # must not lend the preceding binary's identity to later chatter.
            self.current = ""
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
