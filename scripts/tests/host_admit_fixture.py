"""A fixture authority for runner adoption tests (SH-869).

Run as a program, it is `host-admit.py` bound to a fixture root and policy:

    host_admit_fixture.py <root> <policy.json> --entry <id> [--units N] -- <command...>

The production CLI never accepts a root; this file is the injected fixture
API, and it lives only under scripts/tests/.
"""

import json
import multiprocessing
import os
from pathlib import Path
import select
import sys
import tempfile
import time
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from host_admission.broker import Broker
from host_admission.client import Client
from host_admission import native
from host_admission.policy import Policy
from test_host_admission import policy_value
from load_grace import contention, patience

MIB = 1024 * 1024

# A spawned broker, interpreter or supervised child must start or settle within
# this allowance on a loaded gate host. It bounds failure detection only and
# never paces a passing test (SH-698), and grows with contention (SH-767).
PROCESS_ALLOWANCE_S = patience(30, contention())

# The fixture broker's transport deadline, sensor freshness and lease age. A
# loaded host delays its durable transactions, so each is graced the same way.
TRANSPORT_S = patience(1, contention())
LEASE_S = patience(60, contention())

# One measured workload per inventory name: milli-CPUs and MiB. The normal
# envelope is 6000 milli-CPUs and 700 MiB (capacity less headroom and reserve).
WORKLOADS = {
    "rustc": (2000, 100), "rust-test-binary": (1000, 100), "rust-test-thread": (1000, 100),
    "rust-pool": (500, 50), "plugin-script": (1000, 100), "plugin-pool": (500, 50),
    "browser-slice": (2000, 200), "browser-pool": (500, 50), "verifier-python-worker": (1000, 100),
    "verifier-gate": (6000, 600), "release": (4000, 400), "release-observer": (2000, 200),
}


def fixture_policy(**changes):
    """The SH-868 fixture host with a workload for every runner entry."""
    p = policy_value()
    p["host"] = native.host_identity()
    p.update(lease_ms=int(LEASE_S * 1000), stale_ms=int(TRANSPORT_S * 1000), sample_ms=20,
             cleanup_ms=1000)
    for value in (p["capacity"], p["headroom"], p["reserve"]):
        value["cpu"] *= 1000
        value["memory"] *= MIB
    p["workloads"] = {name: dict(cpu=cpu, memory=mib * MIB) for name, (cpu, mib) in WORKLOADS.items()}
    p.update(changes)
    return p


def loader(value):
    """A policy loader that admits this fixture policy and nothing else."""
    return lambda _root, host: Policy(value, host, fixture=True)


def serve(root, value, ready):
    """The real broker over a fixture root, with a constant healthy sensor."""
    broker = Broker(root, Policy(value, value["host"], fixture=True),
                    lambda: dict(at=time.monotonic_ns() // 1_000_000,
                                 available=2000 * MIB, cpu=0, memory=0, runnable=0))
    try:
        ready.send("ready")
        broker.serve()
    finally:
        broker.close()


class AuthorityCase(unittest.TestCase):
    """A real broker per test, a policy file for fixture CLIs, and a client."""

    policy_changes = {}

    def setUp(self):
        # Inherited production grants must not leak into a fixture authority.
        for name in ("STORYHOOK_HOST_GRANT", "STORYHOOK_HOST_REQUEST", "STORYHOOK_HOST_SHARE",
                     "STORYHOOK_HOST_UNITS", "STORYHOOK_HOST_ENTRY", "STORYHOOK_HOST_LEASE_FD"):
            os.environ.pop(name, None)
        self.tmp = tempfile.TemporaryDirectory(dir="/tmp", prefix="ha-")
        self.addCleanup(self.tmp.cleanup)
        self.dir = Path(self.tmp.name)
        self.root = self.dir / "authority"
        self.value = fixture_policy(**self.policy_changes)
        self.policy_file = self.dir / "policy.json"
        self.policy_file.write_text(json.dumps(self.value))
        # A present policy is what makes the authority enabled (decision D1);
        # only the injected fixture loader accepts this fixture calibration.
        self.root.mkdir(mode=0o700)
        fd = os.open(self.root / "policy.json", os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "w") as stream:
            stream.write(json.dumps(self.value))
        ctx = multiprocessing.get_context("spawn")
        receive, send = ctx.Pipe(duplex=False)
        self.broker = ctx.Process(target=serve, args=(self.root, self.value, send))
        self.broker.start()
        send.close()
        self.assertTrue(receive.poll(PROCESS_ALLOWANCE_S), "broker did not report startup")
        self.assertEqual(receive.recv(), "ready")
        receive.close()
        self.addCleanup(self.stop_broker)
        self.client = Client(self.root, timeout_ms=self.value["stale_ms"])

    def stop_broker(self):
        if self.broker.is_alive():
            self.broker.terminate()
        self.broker.join(PROCESS_ALLOWANCE_S)
        if self.broker.is_alive():
            self.broker.kill()
            self.broker.join()

    def cli(self, *arguments):
        """The argv of a fixture-bound host-admit.py."""
        return [sys.executable, "-B", str(Path(__file__).resolve()), str(self.root),
                str(self.policy_file), *arguments]

    def eventually(self, predicate, message="state did not converge"):
        """Await a real change within the fixture's process-test allowance."""
        deadline = time.monotonic() + PROCESS_ALLOWANCE_S
        while time.monotonic() < deadline:
            if predicate():
                return
            select.select([], [], [], self.value["sample_ms"] / 1000)
        self.fail(message)

    def leases(self):
        return self.client.call("status")["leases"]

    def hold(self, identity, cpu, mib, project="blocker"):
        """A directly enqueued root that holds capacity until released."""
        return self.client.call("enqueue", request=dict(
            id=identity, project=project, work="build", resources=dict(cpu=cpu, memory=mib * MIB)))

    def release(self, row):
        self.client.call("cancel", id=row["id"], token=row["token"])
        self.assertTrue(self.client.call("finish", id=row["id"], token=row["token"]))


if __name__ == "__main__":
    from host_admission.adapter import main

    sys.exit(main(sys.argv[3:], root=Path(sys.argv[1]),
                  policy_loader=loader(json.loads(Path(sys.argv[2]).read_text()))))
