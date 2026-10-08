"""The single local admission service; test fixtures inject roots explicitly."""

import fcntl
import json
import os
import selectors
import signal
import socket
import sqlite3
import time

from .authority import Authority
from .client import MAX_MESSAGE
from . import native
from .namespace import check_file, directory, exclusive, open_private
from .policy import Refusal
from .ledger import HELD
from .usage import Monitor


class Broker:
    """Own one host namespace and process a versioned admission protocol."""

    def __init__(self, root, policy, sensor, *, monitor=None):
        self.root = directory(root, create=True)
        self.lock = exclusive(self.root / "broker.lock")
        self.authority = self.socket = self.selector = None
        self.stopped = False
        try:
            self.policy, self.sensor = policy, sensor
            self.boot = native.boot_identity()
            self.identity = native.identity(os.getpid(), self.boot)
            self.monitor = monitor or Monitor(self.boot)
            self.clock = lambda: time.monotonic_ns() // 1_000_000
            database = self.root / "state.db"
            marker = self.root / "initialized"
            if marker.exists() and not database.exists():
                raise Refusal("authority database disappeared; retained grants cannot be reconstructed")
            for path in self.root.iterdir():
                if path.name != "broker.sock":
                    check_file(path)
            fd = open_private(database, create=True); os.close(fd)
            self.authority = Authority(database, policy, self.boot, self.clock,
                                       lambda owner: native.observe(owner, self.boot), initialize=not marker.exists())
            fd = open_private(marker, create=True)
            os.fsync(fd); os.close(fd)
            fd = os.open(self.root, os.O_RDONLY); os.fsync(fd); os.close(fd)
            endpoint = self.root / "broker.sock"
            if endpoint.exists():
                check_file(endpoint, socket=True)
                endpoint.unlink()  # Exclusive permanent lock proves no broker is using it.
            self.socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            self.socket.bind(str(endpoint)); endpoint.chmod(0o600)
            self.socket.listen(); self.socket.setblocking(False)
            self.selector = selectors.DefaultSelector()
            self.selector.register(self.socket, selectors.EVENT_READ, None)
            self._sample()
        except BaseException:
            self.close()
            raise

    def _sample(self):
        self._usage()
        try:
            sample = self.sensor()
        except (OSError, ValueError, Refusal) as error:
            with self.authority.transaction() as state:
                self.authority.event(state, "sensor-error", reason=str(error))
            sample = None
        self.authority.sample(sample)

    def _usage(self):
        a = self.authority
        with a.transaction() as state:
            rows = list(state["leases"].values())
        for row in rows:
            if row["state"] not in HELD:
                continue
            descendants = {row["id"]}
            for _ in rows:
                descendants.update(r["id"] for r in rows if r["parent"] in descendants)
            sessions = {e["session"] for r in rows if r["id"] in descendants
                        for e in r["executions"] if not e["settled"]}
            if not sessions:
                continue
            try:
                sample = self.monitor.sample(row["id"], sessions)
                a.usage(row["id"], row["token"], sample)
            except (OSError, Refusal, ValueError) as error:
                a.quarantine(row["id"], f"resource observation unavailable: {error}")
        # Only independent settlement can recover a lost supervisor. Work that
        # never attached has no lifetime proof and remains quarantined.
        for row in reversed(rows):
            if row["state"] == "quarantined" and row["executions"]:
                for execution in row["executions"]:
                    if not execution["settled"] and self._proof(execution) is True:
                        a.settle(row["id"], row["token"], execution["id"], self._proof)
                a.finish(row["id"], row["token"])

    def _proof(self, execution):
        try:
            if native.session_members(execution["session"]):
                return False
            if native.observe(execution["leader"], self.boot) is not False:
                return None
            fd = open_private(self.root / execution["guard"])
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                return False
            finally:
                os.close(fd)
            return True
        except (OSError, Refusal):
            return None

    def dispatch(self, message, owner):
        """Validate transport identity before giving requests to the durable authority."""
        if not isinstance(message, dict) or message.get("version") != 1:
            raise Refusal("unsupported host admission protocol")
        op = message.get("operation")
        a = self.authority
        if op == "restoration-proof":
            from .restoration import proof
            if (type(message["version"]) is not int
                    or set(message) != {"version", "operation", "nonce", "fault", "window", "affected"}):
                raise Refusal("invalid restoration-proof fields")
            if native.observe(self.identity, self.boot) is not True:
                raise Refusal("broker incarnation is unavailable")
            return proof(a, {key: message[key] for key in ("nonce", "fault", "window", "affected")}, self.identity)
        if op == "status":
            return a.status()
        if op == "events":
            after = message.get("after", 0)
            if type(after) is not int or after < 0:
                raise Refusal("invalid evidence cursor")
            return a.events(after)[:100]
        if op == "enqueue":
            session = native.process(owner["pid"], self.boot)["session"]
            with a.transaction() as state:
                if any(e["session"] == session for r in state["leases"].values()
                       for e in r["executions"] if not e["settled"]):
                    raise Refusal("managed descendants must use a subgrant, not another root")
            return a.enqueue(message["request"], owner)
        row = a.inspect(message["id"])
        token = message.get("token")
        # Read operations must not disclose another caller's bearer capability.
        if row["owner"] != owner and token != row["token"]:
            raise Refusal("request belongs to another supervisor")
        if op in ("inspect", "wait"):
            return row
        if op == "subgrant":
            return a.subgrant(row["id"], token, message["child"], message["resources"], owner)
        if op == "cancel":
            return a.cancel(row["id"], token)
        if op == "finish":
            return a.finish(row["id"], token)
        if op == "attach":
            execution = message["execution"]
            facts = native.process(execution["leader"]["pid"], self.boot)
            if facts["parent"] != owner["pid"] or facts["session"] != execution["session"]:
                raise Refusal("launch is not an owned child session")
            if execution["guard"] != f"lease-{row['token']}.lock":
                raise Refusal("foreign execution lifetime guard")
            check_file(self.root / execution["guard"])
            return a.attach(row["id"], token, execution)
        if op == "settle":
            return a.settle(row["id"], token, message["execution_id"], self._proof)
        raise Refusal(f"unknown admission operation: {op}")

    def serve(self):
        """Process bounded socket frames; wait on events and the policy sample deadline."""
        def stop(_signal, _frame):
            self.stopped = True
        signal.signal(signal.SIGTERM, stop)
        signal.signal(signal.SIGINT, stop)
        next_sample = self.clock() + self.policy.value["sample_ms"]
        while not self.stopped:
            timeout = max(0, next_sample - self.clock()) / 1000
            for key, mask in self.selector.select(timeout):
                if key.fileobj is self.socket:
                    conn, _ = self.socket.accept()
                    try:
                        owner = native.peer_identity(conn, self.boot)
                        conn.setblocking(False)
                        self.selector.register(conn, selectors.EVENT_READ,
                                               dict(owner=owner, input=bytearray(), output=None))
                    except (OSError, Refusal):
                        conn.close()
                    continue
                conn, state = key.fileobj, key.data
                try:
                    if mask & selectors.EVENT_WRITE:
                        count = conn.send(state["output"])
                        state["output"] = state["output"][count:]
                        if not state["output"]:
                            self._disconnect(conn)
                        continue
                    chunk = conn.recv(65536)
                    if not chunk:
                        self._disconnect(conn); continue
                    state["input"].extend(chunk)
                    if len(state["input"]) > MAX_MESSAGE:
                        self._disconnect(conn); continue
                    if b"\n" in state["input"]:
                        try:
                            message = json.loads(state["input"])
                            value = self.dispatch(message, state["owner"])
                            if message["operation"] == "wait" and value["state"] == "queued":
                                state["waiting"] = message
                                state["wait_until"] = self.clock() + self.policy.value["sample_ms"]
                                state["input"].clear()
                                continue
                            response = dict(version=1, value=value)
                        except (Refusal, KeyError, TypeError, ValueError, OSError, sqlite3.Error) as error:
                            response = dict(version=1, error=f"admission refused: {error}")
                        state["output"] = json.dumps(response).encode() + b"\n"
                        self.selector.modify(conn, selectors.EVENT_WRITE, state)
                except (BrokenPipeError, ConnectionResetError):
                    self._disconnect(conn)
            if self.clock() >= next_sample:
                self._sample()
                next_sample = self.clock() + self.policy.value["sample_ms"]
            for key in list(self.selector.get_map().values()):
                state = key.data
                if state and state.get("waiting"):
                    try:
                        row = self.dispatch(state["waiting"], state["owner"])
                        if row["state"] == "queued" and self.clock() < state["wait_until"]:
                            continue
                        response = dict(version=1, value=row)
                    except (Refusal, OSError) as error:
                        response = dict(version=1, error=str(error))
                    del state["waiting"]
                    state["output"] = json.dumps(response).encode() + b"\n"
                    self.selector.modify(key.fileobj, selectors.EVENT_WRITE, state)

    def _disconnect(self, conn):
        self.selector.unregister(conn)
        conn.close()

    def close(self):
        """Drop broker ownership without releasing reservations or deleting permanent state."""
        if self.selector:
            for key in list(self.selector.get_map().values()):
                key.fileobj.close()
            self.selector.close(); self.selector = None
        if self.socket:
            self.socket.close(); self.socket = None
        if self.authority:
            self.authority.close(); self.authority = None
        if self.lock is not None:
            os.close(self.lock); self.lock = None
