"""The verifier gate runs inside a host admission root its owner holds (SH-869, D3/D8)."""

import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import unittest
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parent))
from host_admit_fixture import MIB, AuthorityCase, loader
from host_admission.reservation import Reservation
from verifier_state import read, save
import verifier_result

SCRIPTS = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("verifier_owner", SCRIPTS / "verifier-owner.py")
owner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(owner)

# Reports the gate's own view: its grant variables and its lease's broker state.
GATE = ("import json,os,sys;sys.path.insert(0,sys.argv[1]);"
        "from host_admission.client import Client;"
        "rows=Client(sys.argv[2],timeout_ms=int(sys.argv[3])).call('status')['leases'];"
        "json.dump(dict(grant=bool(os.environ.get('STORYHOOK_HOST_GRANT')),"
        "fd=os.environ.get('STORYHOOK_HOST_LEASE_FD'),entry=os.environ.get('STORYHOOK_HOST_ENTRY'),"
        "states=[r['state'] for r in rows]),open(sys.argv[4],'w'))")


class NoCancellation:
    signum = None


class GateRoot(AuthorityCase):
    def setUp(self):
        super().setUp()
        self.nonce = uuid.uuid4().hex
        self.execution = self.dir / "attempt-1"
        os.environ["STORYHOOK_VERIFIER_OWNER"] = self.nonce
        self.addCleanup(os.environ.pop, "STORYHOOK_VERIFIER_OWNER", None)
        save(str(self.execution), dict(version=1, attempt=self.execution.name, owner=self.nonce,
                                       state="pending", tree="t", base="b", head="h"))
        os.environ[verifier_result.EXECUTION_FILE] = str(self.execution)
        self.addCleanup(os.environ.pop, verifier_result.EXECUTION_FILE, None)
        self.journal = self.dir / "gate.ndjson"
        self.journal.write_text(json.dumps(dict(kind="run", attempt_id="a1", execution_id="e1", generation=7)) + "\n")
        os.environ["STORYHOOK_GATE_PROGRESS"] = str(self.journal)
        self.addCleanup(os.environ.pop, "STORYHOOK_GATE_PROGRESS", None)
        self.record = self.dir / "owner.json"

    def reservation(self, value=None):
        return lambda: Reservation("verifier-gate", project="fixture-project", root=self.root,
                                   policy_loader=loader(value or self.value), binding=owner.gate_binding())

    def gate(self, command, factory):
        record = dict(version=1)
        save(str(self.record), record)
        return owner.execute(command, str(self.record), record, "gate_session", NoCancellation(), 8.0,
                             reservation_factory=factory)

    def records(self, kind):
        return [r for r in map(json.loads, self.journal.read_text().splitlines()) if r["kind"] == kind]

    def test_the_gate_runs_attached_inside_its_root_and_releases_after_its_census(self):
        seen = self.dir / "seen.json"
        status = self.gate([sys.executable, "-c", GATE, str(SCRIPTS), str(self.root),
                            str(self.value["stale_ms"]), str(seen)], self.reservation())
        self.assertEqual(status, 0)
        view = json.loads(seen.read_text())
        self.assertTrue(view["grant"], "the gate inherits its grant")
        self.assertTrue(view["fd"], "the gate inherits the lease lifetime guard")
        self.assertEqual(view["entry"], f"verifier-gate:{os.getpid()}")
        self.assertEqual(view["states"], ["running"], "the session was attached before the command ran")
        self.assertEqual(self.client.call("status")["allocated"]["cpu"], 0, "released after settlement")
        self.assertEqual(read(str(self.execution))["state"], "completed")
        self.assertEqual(self.records("admission"), [])
        lease = [l for l in self.leases()][0]
        self.assertEqual(lease["state"], "released")

    def test_pressure_withdrawal_is_a_retryable_process_hold(self):
        marker = self.dir / "running"
        sleeper = ("import pathlib,sys,time;pathlib.Path(sys.argv[1]).touch();"
                   "[time.sleep(0.05) for _ in iter(int, 1)]")
        import threading

        def pressure_once_running():
            self.eventually(marker.exists, "the gate never started")
            (self.dir / "pressure").write_text("990")

        thread = threading.Thread(target=pressure_once_running)
        thread.start()
        status = self.gate([sys.executable, "-c", sleeper, str(marker)], self.reservation())
        thread.join()
        self.assertNotEqual(status, 0)
        value = read(str(self.execution))
        self.assertEqual(value["state"], "admission")
        self.assertEqual(value["admission"]["cause"], "pressure")
        disposition, detail = verifier_result.admission_verdict(value)
        self.assertEqual(disposition, "retryable")
        self.assertIn("did not judge the change", detail)
        self.assertEqual([(r["cause"], r["retryable"]) for r in self.records("admission")], [("pressure", True)])
        self.assertEqual(self.client.call("status")["allocated"]["cpu"], 0,
                         "capacity returns only after the drained session settled")

    def test_an_overrun_is_a_permanent_process_fault_not_a_test_failure(self):
        hog = "b=b'x'*(800*1024*1024);import time;time.sleep(120)"
        status = self.gate([sys.executable, "-c", hog], self.reservation())
        self.assertNotEqual(status, 0)
        value = read(str(self.execution))
        self.assertEqual((value["state"], value["admission"]["cause"]), ("admission", "envelope-exceeded"))
        self.assertEqual(verifier_result.admission_verdict(value)[0], "permanent")
        self.assertEqual(self.client.call("status")["allocated"]["cpu"], 0)

    def test_a_refused_gate_never_runs(self):
        marker = self.dir / "ran"
        workloads = dict(self.value["workloads"])
        del workloads["verifier-gate"]
        status = self.gate([sys.executable, "-c", f"open({str(marker)!r},'w')"],
                           self.reservation(dict(self.value, workloads=workloads)))
        self.assertEqual(status, 125)
        self.assertFalse(marker.exists(), "a refused gate must not execute")
        value = read(str(self.execution))
        self.assertEqual((value["state"], value["admission"]["cause"], value["admission"]["retryable"]),
                         ("admission", "workload-missing", False))
        self.assertEqual(self.leases(), [], "a refusal holds nothing")

    def test_a_disabled_authority_leaves_the_gate_unchanged(self):
        status = self.gate([sys.executable, "-c", "import os,sys;sys.exit(3 if 'STORYHOOK_HOST_GRANT' in os.environ else 7)"],
                           lambda: None)
        self.assertEqual(status, 7)
        self.assertEqual(read(str(self.execution))["state"], "completed")
        self.assertEqual(self.leases(), [])


class Verdict(unittest.TestCase):
    def test_the_verdict_command_names_the_disposition_and_refuses_ordinary_evidence(self):
        import tempfile
        with tempfile.TemporaryDirectory(dir="/tmp", prefix="va-") as tmp:
            path = Path(tmp) / "attempt-9"
            nonce = uuid.uuid4().hex
            env = dict(os.environ, STORYHOOK_VERIFIER_OWNER=nonce)
            base = dict(version=1, attempt=path.name, owner=nonce, tree="t", base="b", head="h")
            command = [sys.executable, "-B", str(SCRIPTS / "verifier_result.py")]
            save(str(path), dict(base, state="admission", exit_status=143,
                                 admission=dict(cause="pressure", retryable=True, reason="severe pressure")))
            done = subprocess.run(command + ["admission", str(path), "t", "b", "h"], env=env,
                                  capture_output=True, text=True, check=True)
            self.assertEqual(done.stdout.splitlines()[0], "retryable")
            self.assertIn("host admission pressure: severe pressure", done.stdout)
            refused = subprocess.run(command + ["read", str(path), "t", "b", "h"], env=env, capture_output=True)
            self.assertEqual(refused.returncode, 1, "an admission state is never a completed gate answer")
            save(str(path), dict(base, state="completed", exit_status=1))
            ordinary = subprocess.run(command + ["admission", str(path), "t", "b", "h"], env=env,
                                      capture_output=True)
            self.assertEqual(ordinary.returncode, 1, "an ordinary failure carries no admission cause")


if __name__ == "__main__":
    unittest.main()
