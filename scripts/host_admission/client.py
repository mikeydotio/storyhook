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
