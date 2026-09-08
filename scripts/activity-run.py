#!/usr/bin/env python3
"""Observe a verifier command through regular files, preserving its streams.

No pipe is given to the command. Descendants may retain their output files;
the observer snapshots their size when the owned child exits and returns.
The enclosing verifier owns the process-group deadline and cancellation.

Optional test-output mode copies the combined stream to a caller-owned log
and converts only recognized Cargo/libtest milestones into gate progress.
"""

import datetime
import fcntl
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import tempfile
import time

# The observer is run from candidate checkouts; importing its shared parser
# must not create an untracked artifact in the tree under verification.
sys.dont_write_bytecode = True
from test_output import TestOutputParser

CHUNK = 16 * 1024
POLL_SECONDS = 0.05


def clean(text):
    """Redact known token shapes and escape terminal controls in journal text."""
    text = re.sub(r"(?:gh[pousr]_|github_pat_)[A-Za-z0-9_]*", "[REDACTED-GITHUB-TOKEN]", text)
    text = re.sub(r"(?i)(Authorization:|X-Storyhook-Token:).*", r"\1 [REDACTED]", text)
    return "".join(ch if ch.isprintable() else ch.encode("unicode_escape").decode("ascii") for ch in text)


class Journal:
    """Write the same per-record flock and JSON schema as the Rust daemon."""

    def __init__(self, directory, source):
        self.directory = Path(directory)
        self.source = source
        self.failed = False

    def emit(self, stream, message, level=None):
        """Logging failure is visible once and cannot change command status."""
        if self.failed:
            return
        try:
            at = datetime.datetime.now(datetime.timezone.utc)
            self.directory.mkdir(mode=0o700, parents=True, exist_ok=True)
            record = dict(at=at.isoformat(timespec="milliseconds").replace("+00:00", "Z"),
                          level=level or ("WARN" if stream == "stderr" else "INFO"),
                          source=clean(self.source), stream=stream, pid=os.getpid(),
                          context=clean(os.environ.get("STORYHOOK_ACTIVITY_CONTEXT", "")),
                          message=clean(message))
            path = self.directory / (at.strftime("%Y-%m-%d") + ".jsonl")
            fd = os.open(path, os.O_WRONLY | os.O_APPEND | os.O_CREAT | os.O_NOFOLLOW, 0o600)
            with os.fdopen(fd, "ab", buffering=0) as output:
                fcntl.flock(output, fcntl.LOCK_EX)
                data = memoryview((json.dumps(record, ensure_ascii=True) + "\n").encode())
                while data:
                    data = data[output.write(data):]
        except OSError as error:
            self.failed = True
            print(f"warning: activity journal unavailable: {error}", file=sys.stderr, flush=True)


class NullJournal:
    """No-op activity sink for interactive test-output observation."""

    def emit(self, _stream, _message, _level=None):
        """Keep capture and gate progress independent of daemon activity."""


class GateProgress:
    """Append trusted test progress without claiming checklist completion."""

    def __init__(self, journal, path):
        self.journal = journal
        self.path = path
        self.parser = TestOutputParser()

    def consume(self, raw):
        """Append only events recognized by the shared output grammar."""
        line = raw.decode("utf-8", errors="replace")
        for event in self.parser.parse(line):
            if event[0] == "case":
                _, _binary, _name, outcome = event
                record = dict(kind="case", path=self.path, outcome=outcome)
            else:
                _, label = event
                record = dict(kind="activity", path=self.path, label=label, status="running")
                record["at"] = datetime.datetime.now(datetime.timezone.utc).isoformat(
                    timespec="seconds"
                ).replace("+00:00", "Z")
            data = (json.dumps(record, ensure_ascii=True, separators=(",", ":")) + "\n").encode()
            fd = os.open(self.journal, os.O_WRONLY | os.O_APPEND | os.O_CREAT | os.O_NOFOLLOW, 0o600)
            with os.fdopen(fd, "ab", buffering=0) as output:
                fcntl.flock(output, fcntl.LOCK_EX)
                output.write(data)


class Stream:
    """Read with pread so observation never moves the child's write cursor."""

    def __init__(self, file, target, name, journal, capture=None, progress=None):
        self.file, self.target, self.name, self.journal = file, target, name, journal
        self.capture, self.progress = capture, progress
        self.offset = 0
        self.pending = bytearray()

    def drain(self, final=False):
        """Relay a finite snapshot and journal complete lines or bounded chunks."""
        end = os.fstat(self.file.fileno()).st_size
        while self.offset < end:
            data = os.pread(self.file.fileno(), min(CHUNK, end - self.offset), self.offset)
            if not data:
                break
            self.offset += len(data)
            self.target.write(data)
            self.target.flush()
            if self.capture:
                self.capture.write(data)
                self.capture.flush()
            for byte in data:
                if byte in (10, 13):
                    self.flush()
                else:
                    self.pending.append(byte)
                    if len(self.pending) >= CHUNK:
                        self.flush()
        if final:
            self.flush()

    def flush(self):
        """Flush final fragments as well as ordinary newline-delimited output."""
        if self.pending:
            if self.progress:
                self.progress.consume(bytes(self.pending))
            self.journal.emit(self.name, self.pending.decode("utf-8", errors="replace"))
            self.pending.clear()


def run(source, command, capture_path=None, progress_path=None):
    """Preserve stdin, cwd, environment, process group and the child's status."""
    destination = os.environ.get("STORYHOOK_ACTIVITY_LOG_DIR")
    if not destination and not capture_path and not progress_path:
        os.execvp(command[0], command)
    journal = Journal(destination, source) if destination else NullJournal()
    try:
        out = tempfile.TemporaryFile(dir="/tmp")
        err = tempfile.TemporaryFile(dir="/tmp")
    except OSError as error:
        if capture_path or progress_path:
            print(f"activity-run: required capture unavailable: {error}", file=sys.stderr)
            return 125
        print(f"warning: activity capture unavailable: {error}", file=sys.stderr, flush=True)
        os.execvp(command[0], command)
    with out, err:
        try:
            capture = open(capture_path, "ab", buffering=0) if capture_path else None
        except OSError as error:
            print(f"activity-run: required capture unavailable: {error}", file=sys.stderr)
            return 125
        progress = None
        progress_journal = os.environ.get("STORYHOOK_GATE_PROGRESS")
        if progress_path and progress_journal:
            progress = GateProgress(progress_journal, progress_path)
        try:
            child_env = None
            if progress:
                child_env = os.environ.copy()
                child_env.pop("STORYHOOK_GATE_PROGRESS", None)
                child_env.pop("STORYHOOK_GATE_PROGRESS_PATH", None)
            if capture:
                child = subprocess.Popen(command, stdout=out, stderr=subprocess.STDOUT, env=child_env)
            else:
                child = subprocess.Popen(command, stdout=out, stderr=err, env=child_env)
        except OSError as error:
            if capture:
                capture.close()
            journal.emit("event", f"could not start: {error}", "ERROR")
            print(f"activity-run: could not start {source}: {error}", file=sys.stderr)
            return 127
        # The owner cancels the entire inherited group. Stay alive to drain
        # cleanup output; forwarding would deliver a second signal while the
        # child is cleaning up. Install after spawn so the child keeps its own
        # signal dispositions (a Python handler, unlike SIG_IGN, is not inherited).
        for signum in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
            signal.signal(signum, lambda _signum, _frame: None)
        if capture:
            streams = [Stream(out, sys.stdout.buffer, "stdout", journal, capture, progress)]
        else:
            streams = [Stream(out, sys.stdout.buffer, "stdout", journal),
                       Stream(err, sys.stderr.buffer, "stderr", journal)]
        journal.emit("event", f"process started child={child.pid}")
        while True:
            status = child.poll()
            for stream in streams:
                stream.drain(final=status is not None)
            if status is not None:
                if capture:
                    capture.close()
                journal.emit("event", f"process finished status={status}", "INFO" if status == 0 else "ERROR")
                return status if status >= 0 else 128 - status
            time.sleep(POLL_SECONDS)


def arguments(argv):
    """Parse compatible observer arguments without consuming command flags."""
    capture = None
    progress = None
    index = 0
    while index < len(argv) and argv[index].startswith("--"):
        option = argv[index]
        if option not in ("--capture", "--test-progress") or index + 1 >= len(argv):
            raise ValueError
        if option == "--capture":
            capture = argv[index + 1]
        else:
            progress = argv[index + 1]
        index += 2
    if index + 2 >= len(argv) or argv[index + 1] != "--":
        raise ValueError
    return capture, progress, argv[index], argv[index + 2:]


if __name__ == "__main__":
    try:
        capture_path, progress_path, source, command = arguments(sys.argv[1:])
    except ValueError:
        sys.exit("usage: activity-run.py [--capture <path>] [--test-progress <path>] <source> -- <command> [args...]")
    sys.exit(run(source, command, capture_path, progress_path))
