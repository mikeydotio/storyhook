#!/usr/bin/env python3
"""Live native pressure proof bridge. No endpoint or policy override is accepted."""
import json
import sys

sys.dont_write_bytecode = True
from host_admission.client import Client
from host_admission.policy import Refusal


def main():
    raw = sys.stdin.buffer.read(1_048_577)
    if len(raw) > 1_048_576:
        raise Refusal("native host proof request exceeds limit")
    request = json.loads(raw)
    if not isinstance(request, dict) or set(request) != {"operation", "nonce", "fault", "window", "affected"}:
        raise Refusal("unsupported native host proof request")
    operation = request.pop("operation")
    client = Client()
    if operation == "fault-proof":
        result = client.fault_proof(**request)
    elif operation == "restoration-proof":
        result = client.restoration_proof(**request)
    else:
        raise Refusal("native host proof operation is read-only and closed")
    sys.stdout.write(json.dumps(result, separators=(",", ":")) + "\n")


if __name__ == "__main__":
    try:
        main()
    except (Refusal, OSError, ValueError, TypeError, KeyError) as error:
        print(f"native host proof refused: {error}", file=sys.stderr)
        sys.exit(1)
