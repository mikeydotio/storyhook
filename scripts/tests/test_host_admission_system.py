"""Exercise real sockets, kernel identities and subprocess ownership."""

import multiprocessing
import json
import os
from pathlib import Path
import sys
import tempfile
import time
import subprocess
import select
import socket
import signal
from concurrent.futures import ThreadPoolExecutor
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from host_admission.broker import Broker
from host_admission.client import Client
from host_admission import native
from host_admission.policy import Policy, Refusal
from host_admission.supervisor import ManagedProcess
from host_admission.evidence import Publisher
from host_admission.usage import counters
from host_admission.command import main
from test_host_admission import policy_value


def fixture_policy():
    """Scale synthetic envelopes to actual CPU units and Python resident memory."""
    p = policy_value(); p["host"] = native.host_identity()
    p.update(lease_ms=60000, stale_ms=1000, sample_ms=20, cleanup_ms=1000)
    for value in [p[k] for k in ("capacity", "headroom", "reserve")] + list(p["workloads"].values()):
        value["cpu"] *= 1000; value["memory"] *= 1024 * 1024
    return p


def serve(root, ready):
    """Fixture host data enters the real broker below the production CLI boundary."""
    p = fixture_policy()
    broker = Broker(root, Policy(p, p["host"], fixture=True),
                    lambda: dict(at=time.monotonic_ns() // 1_000_000,
                                 available=2000 * 1024 * 1024, cpu=0, memory=0, runnable=0))
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
        p = fixture_policy()
        with self.assertRaisesRegex(Refusal, "disappeared"):
            broker = Broker(self.root, Policy(p, p["host"], fixture=True), lambda: None)
            broker.close()

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(dir="/tmp", prefix="ha-")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name) / "authority"
        self.start()
        self.addCleanup(self.stop)
        self.client = Client(self.root, timeout_ms=1000)

    def start(self):
        """Restart the real authority over its retained namespace."""
        ctx = multiprocessing.get_context("spawn")
        receive, send = ctx.Pipe(duplex=False)
        self.process = ctx.Process(target=serve, args=(self.root, send))
        self.process.start(); send.close()
        self.assertTrue(receive.poll(30), "broker did not report startup")
        self.assertEqual(receive.recv(), "ready"); receive.close()

    def eventually(self, predicate):
        """Await a real state change within the fixture's process-test allowance."""
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if predicate():
                return
            select.select([], [], [], fixture_policy()["sample_ms"] / 1000)
        self.fail("broker state did not converge within the fixture allowance")

    def stop(self):
        if self.process.is_alive():
            self.process.terminate()
        self.process.join(30)
        if self.process.is_alive():
            self.process.kill(); self.process.join()

    def enqueue(self, identity, project="a", **extra):
        request = dict(id=identity, project=project, work="build",
                       resources=dict(cpu=6000, memory=600 * 1024 * 1024))
        return self.client.call("enqueue", request=dict(request, **extra))

    def test_two_homes_and_projects_share_one_capacity(self):
        with patch.dict(os.environ, HOME=str(self.root / "home1"), XDG_STATE_HOME="ignored"):
            first = self.enqueue("first")
        with patch.dict(os.environ, HOME=str(self.root / "home2"), STORYHOOK_BUILD_SLOTS="999"):
            second = self.enqueue("second", "b")
        self.assertEqual(first["state"], "reserved")
        self.assertEqual(second["state"], "queued")
        self.assertEqual(self.client.call("status")["allocated"]["cpu"], 6000)
        self.assertEqual(self.enqueue("first")["token"], first["token"])

    def test_foreign_capability_and_policy_changes_are_refused(self):
        row = self.enqueue("first")
        with self.assertRaises(Refusal):
            self.client.call("cancel", id="first", token="wrong")
        with self.assertRaises(Refusal):
            self.enqueue("second", capacity=10000)
        self.assertEqual(self.client.call("status")["allocated"]["cpu"], 6000)
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
                "c=Client(sys.argv[2],timeout_ms=1000);s=c.call('status');"
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

    def test_exec_failure_is_reported_as_launch_failure(self):
        row = self.enqueue("missing")
        with self.assertRaisesRegex(Refusal, "exec"):
            managed = ManagedProcess(self.client, row, ["/nonexistent/sh868-command"])
            self.addCleanup(managed.close)
            managed.wait()

    def test_broker_loss_drains_owned_process_before_reporting_failure(self):
        row = self.enqueue("loss")
        managed = ManagedProcess(self.client, row, [sys.executable, "-c", "import signal;signal.pause()"])
        try:
            self.stop()
            with self.assertRaises((Refusal, OSError)):
                managed.wait()
            self.assertIsNotNone(managed.child.returncode, "broker loss left owned work alive")
        finally:
            if managed.child.returncode is None:
                managed.child.kill(); managed.child.wait(timeout=30)
            if managed.guard is not None:
                os.close(managed.guard); managed.guard = None

    def test_cancel_retains_budget_until_owned_child_exits(self):
        row = self.enqueue("run")
        managed = ManagedProcess(self.client, row, [sys.executable, "-c", "import signal;signal.pause()"])
        self.addCleanup(managed.close)
        self.client.call("cancel", id="run", token=row["token"])
        self.assertEqual(self.client.call("status")["allocated"]["cpu"], 6000)
        self.assertNotEqual(managed.wait(), 0)
        self.assertEqual(self.client.call("status")["allocated"]["cpu"], 0)

    def test_concurrent_clients_and_lost_reply_share_durable_reservations(self):
        request = dict(id="lost", project="a", work="build",
                       resources=dict(cpu=6000, memory=600 * 1024 * 1024))
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as conn:
            conn.connect(str(self.root / "broker.sock"))
            conn.sendall(json.dumps(dict(version=1, operation="enqueue", request=request)).encode() + b"\n")
            self.eventually(lambda: any(r["id"] == "lost" for r in self.client.call("status")["leases"]))
        self.stop(); self.start()
        row = self.client.call("enqueue", request=request)
        self.assertEqual(row["state"], "reserved")
        with ThreadPoolExecutor(max_workers=4) as pool:
            rows = list(pool.map(lambda i: self.enqueue(f"waiting-{i}", str(i)), range(8)))
        self.assertTrue(all(r["state"] == "queued" for r in rows))
        waited = self.client.call("wait", id=rows[0]["id"], token=rows[0]["token"])
        self.assertEqual(waited["state"], "queued", "long polls must have a bounded queued reply")
        grants = [e for e in self.client.call("events") if e["event"] == "grant"]
        self.assertEqual(len(grants), 1)
        self.assertEqual(self.client.call("status")["allocated"]["cpu"], 6000)

    def test_broker_restart_retains_live_execution_until_supervisor_settles(self):
        row = self.enqueue("restart")
        managed = ManagedProcess(self.client, row, [sys.executable, "-c", "import signal;signal.pause()"])
        self.addCleanup(managed.close)
        self.stop(); self.start()
        self.assertEqual(self.client.call("status")["allocated"]["cpu"], 6000)
        self.assertEqual(managed.call("inspect")["state"], "running")
        managed.close()
        self.assertEqual(self.client.call("status")["allocated"]["cpu"], 0)

    def test_orphaned_execution_holds_budget_until_independent_cleanup(self):
        code = ("import sys,json;sys.path.insert(0,sys.argv[1]);"
                "from host_admission.client import Client;from host_admission.supervisor import ManagedProcess;"
                "c=Client(sys.argv[2],timeout_ms=1000);"
                "r=c.call('enqueue',request=dict(id='orphan',project='a',work='build',"
                "resources=dict(cpu=6000,memory=600*1024*1024)));"
                "m=ManagedProcess(c,r,[sys.executable,'-c','import sys;sys.stdin.buffer.read()']);"
                "print(json.dumps(r),flush=True);import signal;signal.pause()")
        worker = subprocess.Popen([sys.executable, "-B", "-c", code,
                                   str(Path(__file__).resolve().parents[1]), str(self.root)],
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE)
        try:
            self.assertTrue(select.select([worker.stdout], [], [], 30)[0])
            row = json.loads(worker.stdout.readline())
            worker.kill(); worker.wait(timeout=30)
            self.eventually(lambda: self.client.call("inspect", id=row["id"], token=row["token"])["state"] == "quarantined")
            self.assertEqual(self.client.call("status")["allocated"]["cpu"], 6000)
            self.stop(); self.start()
            self.assertEqual(self.client.call("status")["allocated"]["cpu"], 6000)
            worker.stdin.close()
            self.eventually(lambda: self.client.call("status")["allocated"]["cpu"] == 0)
        finally:
            worker.stdin.close(); worker.stdout.close()
            if worker.poll() is None:
                worker.kill(); worker.wait(timeout=30)

    def test_evidence_failure_drains_owned_work_and_remains_an_error(self):
        class FailedPublisher:
            def publish(self):
                raise OSError("fixture journal unavailable")
        row = self.enqueue("journal-loss")
        managed = ManagedProcess(self.client, row, [sys.executable, "-c", "import signal;signal.pause()"],
                                 publisher=FailedPublisher())
        self.addCleanup(managed.close)
        with self.assertRaisesRegex(Refusal, "journal unavailable"):
            managed.wait()
        with self.assertRaisesRegex(Refusal, "journal unavailable"):
            managed.wait()
        self.assertIsNotNone(managed.child.returncode)
        self.assertEqual(self.client.call("status")["allocated"]["cpu"], 6000)
        # An evidence failure does not itself release a lease; independent proof can.
        managed.call("settle", execution_id=managed.execution_id)
        self.assertTrue(managed.call("finish"))

    def test_measured_memory_overrun_requests_cleanup_without_early_release(self):
        row = self.enqueue("memory", resources=dict(cpu=6000, memory=1))
        managed = ManagedProcess(self.client, row, [sys.executable, "-c", "import signal;signal.pause()"])
        self.addCleanup(managed.close)
        self.eventually(lambda: managed.call("inspect")["state"] == "draining")
        self.assertGreater(managed.call("inspect")["peaks"]["memory"], 1)
        self.assertEqual(self.client.call("status")["allocated"]["memory"], 1)
        self.assertNotEqual(managed.wait(), 0)
        self.assertEqual(self.client.call("status")["allocated"]["memory"], 0)

    def test_failed_initial_journal_publication_cancels_unlaunched_grant(self):
        p = fixture_policy()
        with patch("host_admission.command.load_policy", return_value=Policy(p, p["host"], fixture=True)):
            result = main(["run", "--request-id", "no-journal", "--project", "a", "--class", "build",
                           "--cpu", "6000", "--memory", "600000000", "--attempt-id", "a",
                           "--execution-id", "e", "--generation", "1", "--journal",
                           str(Path(self.tmp.name) / "missing"), "--", sys.executable, "-c", "pass"], root=self.root)
        self.assertEqual(result, 125)
        self.assertEqual(self.client.call("status")["allocated"]["cpu"], 0)

    def test_launch_does_not_exec_when_attach_acknowledgement_is_lost(self):
        for committed in (False, True):
            with self.subTest(committed=committed):
                row = self.enqueue(f"attach-{committed}")
                marker = Path(self.tmp.name) / f"unexpected-{committed}"
                actual = self.client.call
                def lose_ack(operation, **arguments):
                    if operation == "attach":
                        if committed:
                            actual(operation, **arguments)
                        raise OSError("fixture lost attach reply")
                    return actual(operation, **arguments)
                with patch.object(self.client, "call", side_effect=lose_ack):
                    with self.assertRaisesRegex((OSError, Refusal), "attach reply"):
                        ManagedProcess(self.client, row, [sys.executable, "-c", f"open({str(marker)!r},'w').close()"])
                self.assertFalse(marker.exists())
                self.assertEqual(self.client.call("status")["allocated"]["cpu"], 0)

    def test_nested_execution_partitions_capacity_and_cannot_reenter_as_a_root(self):
        ready = Path(self.tmp.name) / "nested"
        row = self.enqueue("parent")
        code = ("import sys,os,json,signal;sys.path.insert(0,sys.argv[1]);"
                "from host_admission.client import Client;from host_admission.supervisor import ManagedProcess;"
                "from host_admission.policy import Refusal;"
                "c=Client(sys.argv[2],timeout_ms=1000);parent=os.environ.pop('STORYHOOK_HOST_REQUEST');"
                "token=os.environ.pop('STORYHOOK_HOST_GRANT');\n"
                "try: c.call('enqueue',request=dict(id='illegal',project='a',work='build',resources=dict(cpu=1000,memory=100000000)))\n"
                "except Refusal: pass\n"
                "else: raise AssertionError('managed root escaped ownership')\n"
                "r=c.call('subgrant',id=parent,token=token,child='nested',resources=dict(cpu=2000,memory=200000000));"
                "m=ManagedProcess(c,r,[sys.executable,'-c','import signal;signal.pause()']);"
                "open(sys.argv[3],'w').write(json.dumps(r));m.wait()")
        managed = ManagedProcess(self.client, row, [sys.executable, "-c", code,
                                 str(Path(__file__).resolve().parents[1]), str(self.root), str(ready)])
        self.addCleanup(managed.close)
        self.eventually(lambda: ready.exists() and ready.stat().st_size > 0)
        nested = json.loads(ready.read_text())
        def sampled():
            a = managed.call("inspect")
            b = self.client.call("inspect", id="nested", token=nested["token"])
            return a.get("peaks", {}).get("memory", 0) > b.get("peaks", {}).get("memory", 0) > 0
        self.eventually(sampled)
        self.assertEqual(self.client.call("status")["allocated"]["cpu"], 6000)
        managed.call("cancel")
        self.assertNotEqual(managed.wait(), 0)
        self.assertEqual(self.client.call("status")["allocated"]["cpu"], 0)

    def test_detached_lifetime_is_quarantined_until_its_guard_closes(self):
        fifo = Path(self.tmp.name) / "release"
        ready = Path(self.tmp.name) / "detached"
        os.mkfifo(fifo)
        # Retain both ends in the test so fixture cleanup cannot block on open.
        control = os.open(fifo, os.O_RDWR | os.O_NONBLOCK)
        row = self.enqueue("detach")
        code = ("import os,sys;pid=os.fork();\n"
                "if pid: os._exit(0)\n"
                "os.setsid();open(sys.argv[2],'w').close();"
                "fd=os.open(sys.argv[1],os.O_RDONLY);os.read(fd,1);os._exit(0)")
        managed = ManagedProcess(self.client, row, [sys.executable, "-c", code, str(fifo), str(ready)])
        try:
            self.eventually(ready.exists)
            with self.assertRaisesRegex(Refusal, "settlement"):
                managed.wait()
            self.assertEqual(managed.call("inspect")["state"], "quarantined")
            self.assertEqual(self.client.call("status")["allocated"]["cpu"], 6000)
        finally:
            os.write(control, b"X"); os.close(control)
            managed.close()
        self.eventually(lambda: self.client.call("status")["allocated"]["cpu"] == 0)

    def test_sigterm_during_queue_cancels_before_any_launch(self):
        self.enqueue("held")
        code = ("import sys;sys.path[:0]=sys.argv[1:3];"
                "from test_host_admission_system import fixture_policy;"
                "from host_admission import command;from host_admission.policy import Policy;"
                "p=fixture_policy();command.load_policy=lambda *a:Policy(p,p['host'],fixture=True);"
                "sys.exit(command.main(['run','--request-id','interrupt','--project','b','--class','build',"
                "'--cpu','6000','--memory','600000000','--',sys.executable,'-c','raise Exception(1)'],root=sys.argv[3]))")
        worker = subprocess.Popen([sys.executable, "-B", "-c", code,
                                   str(Path(__file__).resolve().parents[1]), str(Path(__file__).resolve().parent),
                                   str(self.root)])
        try:
            self.eventually(lambda: any(r["id"] == "interrupt" for r in self.client.call("status")["leases"]))
            worker.send_signal(signal.SIGTERM)
            self.assertEqual(worker.wait(timeout=30), 143)
            row = next(r for r in self.client.call("status")["leases"] if r["id"] == "interrupt")
            self.assertEqual(row["state"], "cancelled")
        finally:
            if worker.poll() is None:
                worker.kill(); worker.wait(timeout=30)

    # SH-869 decision D4: production runners create process groups inside
    # their session (captured children, `set -m`, fixtures) and inherit
    # descriptors such as Cargo's jobserver. The supervisor must own the whole
    # session and pass the caller's inheritable descriptors through.

    def spawn_survivor(self, record, *, on_term=False):
        """A command whose descendant leaves the leader's group but not the session."""
        grandchild = ("import signal,time;signal.signal(signal.SIGTERM,signal.SIG_IGN)"
                      if on_term else "import time")
        grandchild += ";time.sleep(3600)"
        spawn = ("import os,subprocess,sys;"
                 f"p=subprocess.Popen([sys.executable,'-c',{grandchild!r}],process_group=0);"
                 f"open({str(record)!r},'w').write(str(p.pid))")
        if not on_term:
            return [sys.executable, "-c", spawn]
        # The leader ignores TERM and forks the survivor only when TERM
        # arrives, after the supervisor's first census.
        return [sys.executable, "-c",
                "import signal,time\n"
                f"def term(*_):\n exec({spawn!r})\n"
                "signal.signal(signal.SIGTERM,term)\n"
                f"open({str(record)!r}+'.ready','w').close()\n"
                "while True: time.sleep(1)\n"]

    def reap_survivor(self, record):
        """A failing run must not leak the survivor that holds the runner's pipes."""
        def reap():
            try:
                pid = int(record.read_text())
            except (OSError, ValueError):
                return
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        self.addCleanup(reap)

    def gone(self, pid):
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return True
        return False

    def test_descendant_in_another_group_is_drained_not_refused(self):
        record = Path(self.tmp.name) / "survivor.pid"
        self.reap_survivor(record)
        row = self.enqueue("subgroup")
        managed = ManagedProcess(self.client, row, self.spawn_survivor(record))
        self.addCleanup(managed.close)
        self.assertEqual(managed.wait(), 0, "the leader's own answer is kept")
        survivor = int(record.read_text())
        self.eventually(lambda: self.gone(survivor))
        self.assertEqual(self.client.call("status")["allocated"]["cpu"], 0)

    def test_cancellation_holds_capacity_until_every_group_settles(self):
        record = Path(self.tmp.name) / "late.pid"
        self.reap_survivor(record)
        row = self.enqueue("late")
        managed = ManagedProcess(self.client, row, self.spawn_survivor(record, on_term=True))
        self.addCleanup(managed.close)
        self.eventually(lambda: Path(str(record) + ".ready").exists())
        self.client.call("cancel", id="late", token=row["token"])
        self.assertEqual(self.client.call("status")["allocated"]["cpu"], 6000)
        self.assertNotEqual(managed.wait(), 0)
        survivor = int(record.read_text())
        self.eventually(lambda: self.gone(survivor))
        self.assertEqual(self.client.call("status")["allocated"]["cpu"], 0)

    def test_inherited_descriptor_reaches_the_command(self):
        read, write = os.pipe()
        os.set_inheritable(write, True)
        self.addCleanup(os.close, read)
        try:
            row = self.enqueue("jobserver")
            managed = ManagedProcess(self.client, row,
                                     [sys.executable, "-c", f"import os;os.write({write},b'token')"])
            self.addCleanup(managed.close)
        finally:
            os.close(write)
        self.assertEqual(managed.wait(), 0, "the command could not use its inherited descriptor")
        self.assertEqual(os.read(read, 16), b"token")
        self.assertFalse(os.get_inheritable(read), "only inheritable descriptors pass")


class NativeTests(unittest.TestCase):
    def test_native_resource_counters_are_real_and_nonnegative(self):
        sample = counters(os.getpid())
        self.assertGreater(sample["memory"], 0)
        self.assertGreaterEqual(sample["cpu_ns"], 0)

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
