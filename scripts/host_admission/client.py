"""Versioned clients use one canonical host endpoint, independent of test homes."""

import json
import os
from pathlib import Path
import socket

from .namespace import ROOT, check_file, directory
from .policy import Refusal

MAX_MESSAGE = 1_048_576

class Client:
    """An admission client with an explicitly injected fixture endpoint when testing."""

    def __init__(self, root=None):
        path = Path(root if root is not None else ROOT)
        if path.is_symlink():
            raise Refusal(f"symlink authority root: {path}")
        self.root = path.resolve()

    def call(self, operation, **arguments):
        """Send one request; a lost reply must be retried with its original request ID."""
        if operation == "enqueue" and os.environ.get("STORYHOOK_HOST_GRANT"):
            raise Refusal("nested work must use a subgrant, never a new root")
        directory(self.root)
        endpoint = self.root / "broker.sock"
        check_file(endpoint, socket=True)
        payload = json.dumps(dict(version=1, operation=operation, **arguments)).encode() + b"\n"
        if len(payload) > MAX_MESSAGE:
            raise Refusal("host request exceeds protocol size")
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as conn:
            conn.connect(str(endpoint))
            conn.sendall(payload)
            answer = bytearray()
            while b"\n" not in answer:
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
