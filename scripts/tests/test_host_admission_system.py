"""Exercise real sockets, kernel identities and subprocess ownership."""

import multiprocessing
import json
import os
from pathlib import Path
import sys
import tempfile
import time
import subprocess
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from host_admission.broker import Broker
from host_admission.client import Client
from host_admission import native
from host_admission.policy import Policy, Refusal
from host_admission.supervisor import ManagedProcess
from host_admission.evidence import Publisher
from test_host_admission import policy_value


def serve(root, ready):
    """Fixture host data enters the real broker below the production CLI boundary."""
    p = policy_value(); p["host"] = native.host_identity()
    p.update(lease_ms=60000, stale_ms=1000, sample_ms=20)
    broker = Broker(root, Policy(p, p["host"], fixture=True),
                    lambda: dict(at=time.monotonic_ns() // 1_000_000,
                                 available=2000, cpu=0, memory=0, runnable=0))
    try:
        ready.send("ready")
        broker.serve()
    finally:
        broker.close()


class BrokerTests(unittest.TestCase):
    def test_initialized_database_loss_never_mints_a_new_authority(self):
        self.enqueue("retained")
        self.stop()
        (self.root / "state.db").write_bytes(b"")
        p = policy_value(); p["host"] = native.host_identity()
        p.update(lease_ms=60000, stale_ms=1000, sample_ms=20)
        with self.assertRaisesRegex(Refusal, "disappeared"):
            broker = Broker(self.root, Policy(p, p["host"], fixture=True), lambda: None)
            broker.close()

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(dir="/tmp", prefix="ha-")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name) / "authority"
        ctx = multiprocessing.get_context("spawn")
        receive, send = ctx.Pipe(duplex=False)
        self.process = ctx.Process(target=serve, args=(self.root, send))
        self.process.start(); send.close()
        self.addCleanup(self.stop)
        self.assertTrue(receive.poll(30), "broker did not report startup")
        self.assertEqual(receive.recv(), "ready"); receive.close()
        self.client = Client(self.root)

    def stop(self):
        if self.process.is_alive():
            self.process.terminate()
        self.process.join(30)
        if self.process.is_alive():
            self.process.kill(); self.process.join()

    def enqueue(self, identity, project="a", **extra):
        return self.client.call("enqueue", request=dict(id=identity, project=project,
                                work="build", resources=dict(cpu=6, memory=600), **extra))

    def test_two_homes_and_projects_share_one_capacity(self):
        with patch.dict(os.environ, HOME=str(self.root / "home1"), XDG_STATE_HOME="ignored"):
            first = self.enqueue("first")
        with patch.dict(os.environ, HOME=str(self.root / "home2"), STORYHOOK_BUILD_SLOTS="999"):
            second = self.enqueue("second", "b")
        self.assertEqual(first["state"], "reserved")
        self.assertEqual(second["state"], "queued")
        self.assertEqual(self.client.call("status")["allocated"]["cpu"], 6)
        self.assertEqual(self.enqueue("first")["token"], first["token"])

    def test_foreign_capability_and_policy_changes_are_refused(self):
        row = self.enqueue("first")
        with self.assertRaises(Refusal):
            self.client.call("cancel", id="first", token="wrong")
        with self.assertRaises(Refusal):
            self.enqueue("second", capacity=10000)
        self.assertEqual(self.client.call("status")["allocated"]["cpu"], 6)
        self.client.call("cancel", id="first", token=row["token"])
        self.assertTrue(self.client.call("finish", id="first", token=row["token"]))

    def test_second_broker_cannot_replace_lock_or_socket(self):
        inode = (self.root / "broker.lock").stat().st_ino
        p = policy_value(); p["host"] = native.host_identity()
        with self.assertRaises(Refusal):
            Broker(self.root, Policy(p, p["host"], fixture=True), lambda: None)
        self.assertEqual((self.root / "broker.lock").stat().st_ino, inode)
        self.assertEqual(self.client.call("status")["allocated"]["cpu"], 0)

    def test_command_starts_only_after_durable_attach_and_settles(self):
        row = self.enqueue("run")
        marker = self.root / "started.txt"
        # The child checks the real broker before publishing its own marker.
        code = ("import sys;sys.path.insert(0,sys.argv[1]);"
                "from host_admission.client import Client;"
                "c=Client(sys.argv[2]);s=c.call('status');"
                "assert s['leases'][0]['state']=='running';"
                "open(sys.argv[3],'w').write('started')")
        managed = ManagedProcess(self.client, row,
                                 [sys.executable, "-c", code, str(Path(__file__).resolve().parents[1]),
                                  str(self.root), str(marker)])
        self.addCleanup(managed.close)
        self.assertEqual(managed.wait(), 0)
        self.assertEqual(marker.read_text(), "started")
        self.assertEqual(self.client.call("status")["allocated"]["cpu"], 0)

    def publication(self):
        """Run an actual bound command and return its production NDJSON for Rust import."""
        binding = dict(attempt_id="attempt", execution_id="gate", generation=7)
        journal = Path(self.tmp.name) / "gate.ndjson"
        journal.write_text(json.dumps(dict(kind="run", **binding)) + "\n")
        row = self.enqueue("published", binding=binding)
        publisher = Publisher(self.client, binding, journal)
        publisher.publish()
        managed = ManagedProcess(self.client, row, [sys.executable, "-c", "pass"], publisher=publisher)
        self.addCleanup(managed.close)
        self.assertEqual(managed.wait(), 0)
        return journal.read_text()

    def test_broker_events_reach_bound_journal_after_process_cleanup(self):
        records = [json.loads(line) for line in self.publication().splitlines()]
        kinds = [r["observation"]["event"] for r in records if r["kind"] == "resource"]
        for required in ("request", "grant", "attach", "cleanup", "release"):
            self.assertIn(required, kinds)

    def test_cancel_before_launch_never_executes_command(self):
        row = self.enqueue("run")
        self.client.call("cancel", id="run", token=row["token"])
        marker = self.root / "unexpected.txt"
        with self.assertRaises(Refusal):
            ManagedProcess(self.client, row, [sys.executable, "-c", f"open({str(marker)!r},'w').close()"])
        self.assertFalse(marker.exists())

    def test_cancel_retains_budget_until_owned_child_exits(self):
        row = self.enqueue("run")
        managed = ManagedProcess(self.client, row, [sys.executable, "-c", "import signal;signal.pause()"])
        self.addCleanup(managed.close)
        self.client.call("cancel", id="run", token=row["token"])
        self.assertEqual(self.client.call("status")["allocated"]["cpu"], 6)
        self.assertNotEqual(managed.wait(), 0)
        self.assertEqual(self.client.call("status")["allocated"]["cpu"], 0)


class NativeTests(unittest.TestCase):
    def test_client_refuses_a_symlink_namespace_before_resolving_it(self):
        with tempfile.TemporaryDirectory(dir="/tmp", prefix="ha-alias-") as root:
            target = Path(root) / "authority"; target.mkdir(mode=0o700)
            alias = Path(root) / "alias"; alias.symlink_to(target)
            with self.assertRaisesRegex(Refusal, "symlink"):
                Client(alias)

    def test_native_incarnation_and_socket_identity_are_stable(self):
        boot = native.boot_identity()
        owner = native.identity(os.getpid(), boot)
        self.assertTrue(native.observe(owner, boot))
        self.assertFalse(native.observe(dict(owner, start="wrong"), boot))
        self.assertIn(os.getpid(), native.session_members(os.getsid(0)))
        self.assertEqual(native.host_identity(), native.host_identity())

    def test_default_client_ignores_environment_namespace_overrides(self):
        with patch.dict(os.environ, HOME="/other", XDG_STATE_HOME="/other",
                        STORYHOOK_LOCK_DIR="/other", STORYHOOK_HOST_ROOT="/other"):
            self.assertEqual(Client().root, Path("/var/tmp/storyhook-host-admission-v1").resolve())


if __name__ == "__main__":
    unittest.main()
