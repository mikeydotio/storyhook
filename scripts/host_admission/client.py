"""Versioned clients use one canonical host endpoint, independent of test homes."""

import json
import os
from pathlib import Path
import socket
import time

from .namespace import ROOT, check_file, directory
from .policy import Refusal, integer

MAX_MESSAGE = 1_048_576

class Client:
    """An admission client with an explicitly injected fixture endpoint when testing."""

    def __init__(self, root=None, *, timeout_ms=None):
        path = Path(root if root is not None else ROOT)
        if path.is_symlink():
            raise Refusal(f"symlink authority root: {path}")
        self.root = path.resolve()
        self.timeout_ms = timeout_ms

    def call(self, operation, **arguments):
        """Send one request; a lost reply must be retried with its original request ID."""
        if operation == "enqueue" and os.environ.get("STORYHOOK_HOST_GRANT"):
            raise Refusal("nested work must use a subgrant, never a new root")
        if self.timeout_ms is None:
            from .activation import load_policy
            from .native import host_identity
            self.timeout_ms = load_policy(self.root, host_identity()).value["stale_ms"]
        integer(self.timeout_ms, "transport deadline")
        directory(self.root)
        endpoint = self.root / "broker.sock"
        check_file(endpoint, socket=True)
        payload = json.dumps(dict(version=1, operation=operation, **arguments)).encode() + b"\n"
        if len(payload) > MAX_MESSAGE:
            raise Refusal("host request exceeds protocol size")
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as conn:
            deadline = time.monotonic() + self.timeout_ms / 1000
            conn.settimeout(self.timeout_ms / 1000)
            conn.connect(str(endpoint))
            conn.sendall(payload)
            answer = bytearray()
            while b"\n" not in answer:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise Refusal("host broker deadline exceeded; retain request identity and lease")
                conn.settimeout(remaining)
                data = conn.recv(65536)
                if not data:
                    raise Refusal("host broker disconnected; retain request identity and lease")
                answer.extend(data)
                if len(answer) > MAX_MESSAGE:
                    raise Refusal("host response exceeds protocol size")
        response = json.loads(answer)
        if response.get("version") != 1:
            raise Refusal("unsupported host broker protocol")
        if response.get("error"):
            raise Refusal(response["error"])
        return response["value"]

    def restoration_proof(self, *, fault, window, affected, nonce):
        """Fetch live native restoration evidence; this JSON is not a capability.

        Unlike ordinary observation calls, this path has no production endpoint
        override and pins the current measured policy and kernel socket peer.
        A Rust factory must validate/consume this live result, not deserialize a
        saved report as authority. Same-account namespace custody is the trust
        boundary, not cryptographic attestation of a Python program.
        """
        return self._pressure_proof("restoration-proof", "host-pressure-restoration",
                                    fault=fault, window=window, affected=affected, nonce=nonce)

    def fault_proof(self, *, fault, window, affected, nonce):
        """Fetch live causal enrollment evidence, never a restoration capability."""
        return self._pressure_proof("fault-proof", "host-pressure-fault",
                                    fault=fault, window=window, affected=affected, nonce=nonce)

    def _pressure_proof(self, operation, kind, *, fault, window, affected, nonce):
        """Both proof kinds use the same canonical native authenticated boundary."""
        from .activation import load_policy
        from . import native
        from .policy import label
        label(nonce, "pressure proof nonce")
        if self.root != Path(ROOT).resolve():
            raise Refusal("pressure proof requires the canonical host endpoint")
        host, boot = native.host_identity(), native.boot_identity()
        policy = load_policy(self.root, host)
        if not isinstance(fault, dict) or fault.get("host") != host or fault.get("boot") != boot or fault.get("policy") != policy.digest:
            raise Refusal("pressure fault differs from native host, boot or measured policy")
        directory(self.root)
        endpoint = self.root / "broker.sock"
        pinned = check_file(endpoint, socket=True)
        payload = json.dumps(dict(version=1, operation=operation, nonce=nonce,
                                  fault=fault, window=window, affected=affected)).encode() + b"\n"
        if len(payload) > MAX_MESSAGE:
            raise Refusal("pressure proof request exceeds protocol size")
        timeout = policy.value["stale_ms"] / 1000
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as conn:
            deadline = time.monotonic() + timeout
            conn.settimeout(timeout)
            conn.connect(str(endpoint))
            peer = native.peer_identity(conn, boot)
            conn.sendall(payload)
            answer = bytearray()
            while b"\n" not in answer:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise Refusal("pressure proof broker deadline exceeded")
                conn.settimeout(remaining)
                data = conn.recv(65536)
                if not data:
                    raise Refusal("pressure proof broker disconnected")
                answer.extend(data)
                if len(answer) > MAX_MESSAGE:
                    raise Refusal("pressure proof response exceeds protocol size")
            if native.peer_identity(conn, boot) != peer:
                raise Refusal("pressure proof broker incarnation changed")
            current = check_file(endpoint, socket=True)
            if (current.st_dev, current.st_ino) != (pinned.st_dev, pinned.st_ino):
                raise Refusal("pressure proof endpoint changed")
        response = json.loads(answer)
        if (not isinstance(response, dict) or type(response.get("version")) is not int
                or response.get("version") != 1 or response.get("error")):
            raise Refusal("native pressure proof was refused")
        result = response.get("value")
        if (not isinstance(result, dict) or type(result.get("version")) is not int or result.get("version") != 1
                or result.get("kind") != kind
                or result.get("nonce") != nonce or result.get("broker") != peer
                or result.get("fault") != fault or result.get("window") != window
                or result.get("affected") != affected):
            raise Refusal("pressure proof reply does not match the live native request")
        if native.boot_identity() != boot or load_policy(self.root, host).digest != policy.digest:
            raise Refusal("pressure proof boot or measured policy changed during observation")
        if operation == "fault-proof":
            checked = result.get("checked_at")
            now = time.monotonic_ns() // 1_000_000
            if type(checked) is not int or not 0 <= checked <= now or now - checked > policy.value["stale_ms"]:
                raise Refusal("native fault proof clock is stale or invalid at receipt")
            timing = result.get("timing")
            if (set(result) != {"version", "kind", "nonce", "broker", "fault", "window", "affected",
                               "settled", "checked_at", "ledger_sequence", "timing"}
                    or not isinstance(timing, dict) or set(timing) != {"stale_ms"}
                    or type(timing["stale_ms"]) is not int or timing["stale_ms"] != policy.value["stale_ms"]):
                raise Refusal("fault enrollment cannot include restoration evidence")
            return result
        sample, checked = result.get("sample"), result.get("checked_at")
        if (not isinstance(sample, dict) or set(sample) != {"at", "available", "cpu", "memory", "runnable"}
                or any(type(v) is not int or not 0 <= v <= 2**63 - 1 for v in sample.values())
                or type(checked) is not int or checked < sample["at"]):
            raise Refusal("restoration reply lacks a valid native sample clock")
        now = time.monotonic_ns() // 1_000_000
        levels = policy.value["thresholds"]
        if (not sample["at"] <= checked <= now or now - sample["at"] > policy.value["stale_ms"]
                or sample["cpu"] > 1000 or sample["memory"] > 1000
                or any(sample[k] >= levels[k][1] for k in levels)
                or sample["available"] <= policy.value["headroom"]["memory"]):
            raise Refusal("restoration sample is stale or unhealthy at native receipt")
        return result
