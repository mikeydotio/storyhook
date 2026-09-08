#!/usr/bin/env python3
"""Observe a verifier command through regular files, preserving its streams.

No pipe is given to the command. Descendants may retain their output files;
the observer snapshots their size when the owned child exits and returns.
The enclosing verifier owns the process-group deadline and cancellation.
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


class Stream:
    """Read with pread so observation never moves the child's write cursor."""

    def __init__(self, file, target, name, journal):
        self.file, self.target, self.name, self.journal = file, target, name, journal
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
            self.journal.emit(self.name, self.pending.decode("utf-8", errors="replace"))
            self.pending.clear()


def run(source, command):
    """Preserve stdin, cwd, environment, process group and the child's status."""
    destination = os.environ.get("STORYHOOK_ACTIVITY_LOG_DIR")
    if not destination:
        os.execvp(command[0], command)
    journal = Journal(destination, source)
    try:
        out = tempfile.TemporaryFile(dir="/tmp")
        err = tempfile.TemporaryFile(dir="/tmp")
    except OSError as error:
        print(f"warning: activity capture unavailable: {error}", file=sys.stderr, flush=True)
        os.execvp(command[0], command)
    with out, err:
        try:
            child = subprocess.Popen(command, stdout=out, stderr=err)
        except OSError as error:
            journal.emit("event", f"could not start: {error}", "ERROR")
            print(f"activity-run: could not start {source}: {error}", file=sys.stderr)
            return 127
        # The owner cancels the entire inherited group. Stay alive to drain
        # cleanup output; forwarding would deliver a second signal while the
        # child is cleaning up. Install after spawn so the child keeps its own
        # signal dispositions (a Python handler, unlike SIG_IGN, is not inherited).
        for signum in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
            signal.signal(signum, lambda _signum, _frame: None)
        streams = [Stream(out, sys.stdout.buffer, "stdout", journal),
                   Stream(err, sys.stderr.buffer, "stderr", journal)]
        journal.emit("event", f"process started child={child.pid}")
        while True:
            status = child.poll()
            for stream in streams:
                stream.drain(final=status is not None)
            if status is not None:
                journal.emit("event", f"process finished status={status}", "INFO" if status == 0 else "ERROR")
                return status if status >= 0 else 128 - status
            time.sleep(POLL_SECONDS)


if __name__ == "__main__":
    if len(sys.argv) < 4 or sys.argv[2] != "--":
        sys.exit("usage: activity-run.py <source> -- <command> [args...]")
    sys.exit(run(sys.argv[1], sys.argv[3:]))
