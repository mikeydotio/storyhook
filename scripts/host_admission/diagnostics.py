"""Best-effort boundary telemetry, never custody or verdict evidence.

No polling-path files, fsyncs, threads, subprocesses or native censuses. Child
messages are fixed-schema datagrams; a full/broken channel drops, never waits.
The first 32 and last 256 events plus each boundary's latest state are retained.
Missing events always mean unknown, including loss on supervisor death.
"""

from collections import deque
import json
import os
import socket
import time

PHASES = frozenset("""
request descriptor_capture command_spawn command_wait result_flush result_fsync
custody_fsync custody_directory_fsync admission supervisor_descriptor_capture
supervisor_spawn readiness_handshake exec_handshake waitid census liveness
publisher deadline cancellation signal_term signal_kill reap settle finish
command_cancel command_reap
supervision close
""".split())
EDGES = frozenset(("begin", "end", "error"))


def event(phase, edge, value=None):
    if phase not in PHASES or edge not in EDGES or (value is not None and type(value) is not int):
        return None
    return [phase, edge, time.monotonic_ns(), value]


def boundary(trace, phase, function, /, *args, **kwargs):
    if trace is not None:
        trace.emit(phase, "begin")
    try:
        value = function(*args, **kwargs)
    except BaseException:
        if trace is not None:
            trace.emit(phase, "error")
        raise
    if trace is not None:
        trace.emit(phase, "end")
    return value


class ChildTrace:
    def __init__(self, fd):
        # Do not leak the diagnostic descriptor into the observed command.
        self.socket = None
        try:
            self.socket = socket.socket(fileno=fd)
            os.set_inheritable(fd, False)
            self.socket.setblocking(False)
        except OSError:
            try:
                if self.socket is None:
                    os.close(fd)
                else:
                    self.socket.close()
            except OSError:
                pass
            raise
        self.sequence = 0

    def emit(self, phase, edge, value=None):
        row = event(phase, edge, value)
        if row is None:
            return
        self.sequence += 1
        try:
            self.socket.send(json.dumps([self.sequence, *row]).encode("ascii"))
        except OSError:
            pass  # Sequence gaps diagnose loss if a later packet arrives.

    def close(self):
        try:
            self.socket.close()
        except OSError:
            pass


class Trace:
    def __init__(self):
        self.reader, self.writer = socket.socketpair(socket.AF_UNIX, socket.SOCK_DGRAM)
        self.reader.setblocking(False)
        self.writer.setblocking(False)
        self.first = []
        self.tail = deque(maxlen=256)
        self.latest = {}
        self.count = 0
        self.child_sequence = 0
        self.child_gaps = 0
        self.invalid_packets = 0

    def _retain(self, source, row):
        self.count += 1
        record = [source, *row]
        if len(self.first) < 32:
            self.first.append(record)
        else:
            self.tail.append(record)
        self.latest[source + ":" + row[0]] = record

    def emit(self, phase, edge, value=None):
        row = event(phase, edge, value)
        if row is not None:
            self._retain("supervisor", row)

    def poll(self):
        # A hostile or broken child cannot turn this into an unbounded drain.
        for _ in range(32):
            try:
                packet = self.reader.recv(256)
            except OSError:
                return
            try:
                seq, phase, edge, timestamp, value = json.loads(packet)
                if (type(seq) is not int or seq <= self.child_sequence
                        or phase not in PHASES or edge not in EDGES
                        or type(timestamp) is not int or timestamp < 0
                        or (value is not None and type(value) is not int)):
                    raise ValueError("invalid boundary event")
            except (ValueError, TypeError):
                self.invalid_packets += 1
                continue
            self.child_gaps += seq - self.child_sequence - 1
            self.child_sequence = seq
            self._retain("child", [phase, edge, timestamp, value])

    def snapshot(self):
        self.poll()
        return dict(version=1, evidence="diagnostic-only; missing boundaries are unknown",
                    durability="best effort; no fsync; lost on supervisor death",
                    events=self.first + list(self.tail), latest=self.latest,
                    events_seen=self.count, events_evicted=max(0, self.count - 288),
                    child_sequence=self.child_sequence, child_gaps=self.child_gaps,
                    invalid_packets=self.invalid_packets,
                    unobserved_tail_possible=True)

    def save(self, path):
        # Exactly one best-effort write after supervision/cleanup; never fsync.
        # Failure cannot erase or replace a workload/custody failure.
        try:
            with open(path, "x", encoding="utf-8") as stream:
                json.dump(self.snapshot(), stream, sort_keys=True)
                stream.write("\n")
        except OSError:
            pass

    def close(self):
        for channel in (self.reader, self.writer):
            try:
                channel.close()
            except OSError:
                pass
