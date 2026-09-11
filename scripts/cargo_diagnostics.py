#!/usr/bin/env python3
"""Collect compiler diagnostics at explicit Cargo build-only boundaries.

SH-685: test output is never a diagnostic source. Test preparation is observed
separately, then Cargo executes tests with the original shared stdout/stderr.
The verifier owns the destination; Cargo descendants never inherit it.
"""

import fcntl
import json
import os
import signal
import stat
import subprocess
import sys
import tempfile
import time

ENV_KEY = "STORYHOOK_COMPILER_DIAGNOSTICS"
CHUNK = 16384


def diagnostic(record):
    """Validate a Cargo compiler record before reporting or storing it."""
    message = record.get("message")
    if (record.get("reason") != "compiler-message" or not isinstance(message, dict)
            or not isinstance(message.get("level"), str)
            or not isinstance(message.get("message"), str)
            or not isinstance(message.get("rendered", ""), (str, type(None)))):
        raise ValueError("malformed compiler-message record")
    code = message.get("code")
    if code is not None and (not isinstance(code, dict) or not isinstance(code.get("code"), str)):
        raise ValueError("malformed compiler-message code")
    return message


def open_artifact(path):
    """Open only an existing regular artifact owned by the invocation's caller."""
    fd = os.open(path, os.O_WRONLY | os.O_APPEND | os.O_NOFOLLOW | os.O_NONBLOCK)
    if not stat.S_ISREG(os.fstat(fd).st_mode):
        os.close(fd)
        raise ValueError(f"compiler diagnostics artifact is not a regular file: {path}")
    return os.fdopen(fd, "ab", buffering=0)


def headline(message):
    """Render one diagnostic identity without embedded line or terminal controls."""
    code = message.get("code")
    prefix = message["level"] + (f'[{code["code"]}]' if code else "")
    text = f'{prefix}: {message["message"]}'
    return "".join(ch if ch.isprintable() else " " for ch in text)


class Collector:
    """Incrementally separate Cargo diagnostics from arbitrary output."""

    def __init__(self, artifact, output):
        """Keep per-invocation parsing state and verify the destination up front."""
        with open_artifact(artifact):
            pass
        self.artifact, self.output = artifact, output
        self.pending = bytearray()
        self.finished = False
        self.success = None

    def feed(self, data):
        """Consume arbitrary byte fragments without splitting JSON records."""
        self.pending.extend(data)
        while b"\n" in self.pending:
            line, _, rest = self.pending.partition(b"\n")
            self.pending = bytearray(rest)
            self.line(bytes(line) + b"\n")

    def line(self, raw):
        """Interpret only Cargo stdout before this invocation's build-finished."""
        if self.finished:
            self.output.write(raw)
            return
        try:
            record = json.loads(raw)
        except (ValueError, UnicodeError):
            self.output.write(raw)
            return
        if not isinstance(record, dict):
            self.output.write(raw)
            return
        reason = record.get("reason")
        if reason == "compiler-message":
            message = diagnostic(record)
            with open_artifact(self.artifact) as artifact:
                fcntl.flock(artifact, fcntl.LOCK_EX)
                data = memoryview(wire(record))
                while data:
                    data = data[artifact.write(data):]
            rendered = message.get("rendered") or headline(message) + "\n"
            self.output.write(rendered.encode("utf-8"))
        elif reason == "build-finished":
            if not isinstance(record.get("success"), bool):
                raise ValueError("malformed build-finished record")
            self.finished, self.success = True, record["success"]
        elif reason not in ("compiler-artifact", "build-script-executed"):
            self.output.write(raw)

    def finish(self):
        """Relay the final unterminated fragment without waiting for more output."""
        if self.pending:
            self.line(bytes(self.pending))
            self.pending.clear()
        self.output.flush()


def wire(record):
    """Serialize one appendable Cargo record, including its line delimiter."""
    return (json.dumps(record, ensure_ascii=True) + "\n").encode()


def run_build(command, artifact):
    """Run a build-only command with stdout diagnostic collection."""
    try:
        collector = Collector(artifact, sys.stdout.buffer)
        out = tempfile.TemporaryFile(dir="/tmp")
    except (OSError, ValueError) as error:
        print(f"compiler diagnostics: cannot prepare collection: {error}", file=sys.stderr)
        return 125
    child_env = os.environ.copy()
    child_env.pop(ENV_KEY, None)
    with out:
        try:
            child = subprocess.Popen(command, stdout=out, env=child_env)
        except OSError as error:
            print(f"compiler diagnostics: cannot start {command[0]}: {error}", file=sys.stderr)
            return 127
        # The enclosing supervisor cancels the process group. Stay to drain
        # the child's final output, without forwarding a duplicate signal.
        handlers = {}
        for signum in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
            handlers[signum] = signal.signal(signum, lambda _signum, _frame: None)
        offset, failure = 0, None
        try:
            while True:
                status = child.poll()
                end = os.fstat(out.fileno()).st_size
                while offset < end:
                    data = os.pread(out.fileno(), min(CHUNK, end - offset), offset)
                    if not data:
                        break
                    offset += len(data)
                    if failure:
                        sys.stdout.buffer.write(data)
                    else:
                        try:
                            collector.feed(data)
                        except (OSError, ValueError) as error:
                            failure = str(error)
                            sys.stdout.buffer.write(data)
                    sys.stdout.buffer.flush()
                if status is not None:
                    break
                time.sleep(0.05)
            if not failure:
                try:
                    collector.finish()
                except (OSError, ValueError) as error:
                    failure = str(error)
        finally:
            for signum, handler in handlers.items():
                signal.signal(signum, handler)
        if failure:
            print(f"compiler diagnostics: collection failed: {failure}", file=sys.stderr)
            return 125
        if not collector.finished:
            print("compiler diagnostics: no build-finished record; inspect Cargo output", file=sys.stderr)
            if status == 0:
                return 125
        elif status == 0 and not collector.success:
            print("compiler diagnostics: build-finished contradicts successful exit", file=sys.stderr)
            return 125
        return status if status >= 0 else 128 - status


def summarize(path):
    """Read only collected error records; malformed evidence is never empty success."""
    lines = []
    with open(path, "rb") as source:
        for raw in source:
            record = json.loads(raw)
            if not isinstance(record, dict):
                raise ValueError("non-object compiler diagnostics record")
            message = diagnostic(record)
            if message["level"] in ("error", "error: internal compiler error"):
                text = headline(message)
                if text not in lines:
                    lines.append(text)
    for line in lines:
        print(line)


def main(args):
    """Adapt explicit Cargo commands; preserve the original execution argv."""
    if len(args) == 2 and args[0] == "--summarize":
        try:
            summarize(args[1])
            return 0
        except (OSError, ValueError) as error:
            print(f"compiler diagnostics: cannot read {args[1]}: {error}", file=sys.stderr)
            return 125
    if len(args) < 3 or args[0] != "--":
        print("usage: cargo_diagnostics.py -- <cargo> <command> [args...] | --summarize <artifact>", file=sys.stderr)
        return 2
    command = args[1:]
    artifact = os.environ.pop(ENV_KEY, None)
    if not artifact:
        os.execvp(command[0], command)
    options = command[:command.index("--")] if "--" in command else command[:]
    operation = options[1]
    if operation not in ("test", "check", "build", "clippy"):
        print(f"compiler diagnostics: unsupported Cargo command {operation}", file=sys.stderr)
        return 2
    # Rustdoc runs compilations and tests together. Its errors remain raw log
    # context; --no-run cannot prepare doctests on stable Cargo.
    if operation == "test" and "--doc" in options:
        os.execvp(command[0], command)
    # Only the preparation command changes format. Preserve all original
    # arguments, including user format choices, for actual test execution.
    build = []
    skip = False
    for arg in options:
        if skip:
            skip = False
            continue
        if arg == "--message-format":
            skip = True
        elif not arg.startswith("--message-format="):
            build.append(arg)
    build.append("--message-format=json")
    if operation != "test" and "--" in command:
        build.extend(command[command.index("--"):])
    if operation == "test" and "--no-run" not in build:
        build.append("--no-run")
    status = run_build(build, artifact)
    if status or operation != "test" or "--no-run" in options:
        return status
    os.execvp(command[0], command)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
