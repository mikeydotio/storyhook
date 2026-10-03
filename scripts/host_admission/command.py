"""Explicit host service and supervised commands; no implicit unbounded fallback."""

import argparse
import json
import os
from pathlib import Path
import sys

from .activation import load_policy
from .broker import Broker
from .client import Client
from .evidence import Publisher
from .native import host_identity
from .namespace import ROOT
from .policy import CLASSES, Refusal
from .sensors import Sensor
from .supervisor import ManagedProcess


def main(arguments=None, *, root=ROOT):
    """Run the production CLI; root injection is available only to in-process fixtures."""
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="operation", required=True)
    sub.add_parser("serve")
    sub.add_parser("status")
    events = sub.add_parser("events")
    events.add_argument("--after", type=int, default=0)
    run = sub.add_parser("run")
    run.add_argument("--request-id", required=True)
    run.add_argument("--project", required=True)
    run.add_argument("--class", dest="work", choices=CLASSES, required=True)
    run.add_argument("--cpu", type=int, required=True, help="CPU capacity in milli-CPUs")
    run.add_argument("--memory", type=int, required=True, help="memory capacity in bytes")
    run.add_argument("--attempt-id")
    run.add_argument("--execution-id")
    run.add_argument("--generation", type=int)
    run.add_argument("--journal")
    run.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args(arguments)
    root = Path(root)
    try:
        if args.operation == "status" and not (root / "policy.json").exists():
            print(json.dumps(dict(version=1, enabled=False, reason="measured host calibration is absent")))
            return 0
        policy = load_policy(root, host_identity())
        if args.operation == "serve":
            broker = Broker(root, policy, Sensor(freshness_ms=policy.value["stale_ms"]))
            try:
                broker.serve()
            finally:
                broker.close()
            return 0
        client = Client(root)
        if args.operation in {"status", "events"}:
            result = client.call(args.operation, **({"after": args.after} if args.operation == "events" else {}))
            print(json.dumps(dict(version=1, enabled=True, value=result)))
            return 0
        command = args.command[1:] if args.command[:1] == ["--"] else args.command
        if not command:
            raise Refusal("run requires a command after --")
        amount = dict(cpu=args.cpu, memory=args.memory)
        binding = None
        publisher = None
        if any(value is not None for value in (args.attempt_id, args.execution_id, args.generation, args.journal)):
            if not all(value is not None for value in (args.attempt_id, args.execution_id, args.generation, args.journal)):
                raise Refusal("gate evidence requires attempt, execution, generation and existing journal")
            binding = dict(attempt_id=args.attempt_id, execution_id=args.execution_id, generation=args.generation)
            publisher = Publisher(client, binding, args.journal)
        parent = os.environ.get("STORYHOOK_HOST_REQUEST")
        capability = os.environ.get("STORYHOOK_HOST_GRANT")
        if parent or capability:
            if not parent or not capability:
                raise Refusal("incomplete inherited grant")
            lease = client.call("subgrant", id=parent, token=capability,
                                child=args.request_id, resources=amount)
            if binding is not None and lease["binding"] != binding:
                client.call("cancel", id=lease["id"], token=lease["token"])
                client.call("finish", id=lease["id"], token=lease["token"])
                raise Refusal("nested work must preserve its parent evidence binding")
        else:
            lease = client.call("enqueue", request=dict(id=args.request_id, project=args.project,
                                work=args.work, resources=amount, **({"binding": binding} if binding else {})))
        try:
            if publisher:
                publisher.publish()
            if lease["state"] == "queued":
                lease = client.call("wait", id=lease["id"], token=lease["token"])
            managed = ManagedProcess(client, lease, command, publisher=publisher)
            try:
                result = managed.wait()
                return result if result >= 0 else 128 - result
            finally:
                managed.close()
        except KeyboardInterrupt:
            client.call("cancel", id=lease["id"], token=lease["token"])
            client.call("finish", id=lease["id"], token=lease["token"])
            return 130
    except (Refusal, OSError, ValueError) as error:
        print(f"host-admission: {error}", file=sys.stderr)
        return 125
