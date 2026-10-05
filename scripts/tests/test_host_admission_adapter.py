"""Runner adapter modes, amounts, inheritance, waits and attribution (SH-869)."""

import fcntl
import importlib.util
import json
import os
from pathlib import Path
import shlex
import signal
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
from host_admit_fixture import MIB, PROCESS_ALLOWANCE_S, AuthorityCase, fixture_policy, loader
from host_admission import adapter, namespace
from host_admission.entries import ENTRIES
from host_admission.native import host_identity
from host_admission.policy import Policy, Refusal
from host_admission.reservation import Admission, drain_cause, plan

SCRIPTS = Path(__file__).resolve().parents[1]
REPORT = ("import json,os,sys;json.dump({k:os.environ.get(k) for k in "
          "('STORYHOOK_HOST_UNITS','STORYHOOK_HOST_SHARE','STORYHOOK_HOST_ENTRY','STORYHOOK_HOST_GRANT')}"
          "|{'ppid':os.getppid(),'pid':os.getpid()},open(sys.argv[1],'w'))")


def policy(**changes):
    value = fixture_policy(**changes)
    return Policy(value, value["host"], fixture=True)


def load_entrypoint():
    spec = importlib.util.spec_from_file_location("host_admit_cli", SCRIPTS / "host-admit.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class Arithmetic(unittest.TestCase):
    def test_fast_path_constants_match_the_inventory(self):
        cli = load_entrypoint()
        self.assertEqual(cli.POLICY, str(namespace.ROOT / "policy.json"))
        self.assertEqual(cli.POOLS, {e.id for e in ENTRIES.values() if e.pool})
        self.assertEqual(cli.LEAVES, {e.id for e in ENTRIES.values() if not e.pool})

    def test_root_plan_reserves_overhead_plus_units_and_caps_to_capacity(self):
        p = policy()
        units, reserved, share = plan(ENTRIES["plugin-pool"], 4, p)
        self.assertEqual(units, 4)
        self.assertEqual(reserved, dict(cpu=500 + 4 * 1000, memory=(50 + 4 * 100) * MIB))
        self.assertEqual(share, dict(cpu=1000, memory=100 * MIB), "a pool worker's share is one unit")
        units, reserved, _ = plan(ENTRIES["plugin-pool"], 99, p)
        self.assertEqual(units, 5, "units are capped to what fits the normal envelope")
        self.assertLessEqual(reserved["cpu"], p.normal["cpu"])
        units, reserved, share = plan(ENTRIES["verifier-gate"], 7, p)
        self.assertEqual((units, share), (1, reserved), "a leaf's share is its whole grant")
        with self.assertRaises(Admission) as refused:
            plan(ENTRIES["verifier-gate"], 1, policy(workloads=dict(fixture_policy()["workloads"],
                                                               **{"verifier-gate": dict(cpu=9000, memory=MIB)})))
        self.assertEqual((refused.exception.cause, refused.exception.retryable), ("capacity", False))
        units, _, _ = plan(ENTRIES["verifier-gate"], 1, policy(workloads=dict(
            fixture_policy()["workloads"], **{"verifier-gate": dict(cpu=7000, memory=MIB)})), work="repair")
        self.assertEqual(units, 1, "repair may use the reserve that ordinary work cannot")

    def test_missing_workload_is_a_permanent_refusal(self):
        workloads = dict(fixture_policy()["workloads"])
        del workloads["plugin-script"]
        with self.assertRaises(Admission) as refused:
            plan(ENTRIES["plugin-pool"], 2, policy(workloads=workloads))
        self.assertEqual((refused.exception.cause, refused.exception.retryable), ("workload-missing", False))

    def test_nested_pools_divide_the_inherited_share(self):
        p = policy()
        env = {adapter.SHARE: "cpu=3500,memory=" + str(350 * MIB)}
        units, share = adapter.nested_units(ENTRIES["plugin-pool"], 8, env, p)
        self.assertEqual(units, 3, "(3500 - 500 overhead) / 1000 per unit")
        self.assertEqual(share, dict(cpu=1000, memory=100 * MIB))
        self.assertLessEqual(units * 1000 + 500, 3500, "nesting never exceeds the inherited share")
        units, share = adapter.nested_units(ENTRIES["plugin-pool"], 2, env, p)
        self.assertEqual((units, share["cpu"]), (2, 1500), "fewer requested units each get more")
        units, share = adapter.nested_units(ENTRIES["plugin-pool"], 8, {adapter.SHARE: "cpu=300,memory=1"}, p)
        self.assertEqual(units, 1, "a share below one unit still runs its sole occupant")
        self.assertEqual(adapter.nested_units(ENTRIES["plugin-pool"], 8, {}, p), (1, None),
                         "without a share a nested pool runs serially")
        for bad in ("cpu=1", "cpu=x,memory=1", "cpu=1,memory=2,disk=3", "cpu=-1,memory=2"):
            with self.assertRaises(Refusal):
                adapter.parse_share(bad)

    def test_drain_reasons_map_to_typed_causes(self):
        self.assertEqual(drain_cause("severe pressure"), ("pressure", True))
        self.assertEqual(drain_cause("lease deadline"), ("lease-deadline", False))
        self.assertEqual(drain_cause("resource envelope exceeded"), ("envelope-exceeded", False))
        self.assertEqual(drain_cause("resource observation unavailable: x"), ("sensor", True))


class Modes(unittest.TestCase):
    def setUp(self):
        for name in (adapter.GRANT, adapter.REQUEST, adapter.SHARE, adapter.LEASE_FD):
            os.environ.pop(name, None)
        self.tmp = tempfile.TemporaryDirectory(dir="/tmp", prefix="hm-")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name) / "authority"
        self.root.mkdir(mode=0o700)
        self.calls = []

    def execute(self, program, argv, env):
        self.calls.append((program, argv, env))
        return 0

    def test_disabled_runs_the_command_unchanged_with_its_requested_units(self):
        env = dict(PATH=os.environ["PATH"])
        adapter.admit("plugin-pool", 3, ["bash", "x.sh", "a b"], root=self.root, env=env, execute=self.execute)
        program, argv, child = self.calls[0]
        self.assertEqual((program, argv), ("bash", ["bash", "x.sh", "a b"]))
        self.assertEqual(child[adapter.UNITS], "3")
        self.assertEqual(child[adapter.ENTRY], f"plugin-pool:{os.getpid()}")
        self.assertNotIn(adapter.SHARE, child)
        adapter.admit("rustc", 1, ["rustc", "-vV"], root=self.root, env=env, execute=self.execute)
        self.assertNotIn(adapter.UNITS, self.calls[1][2], "a leaf has no unit count")

    def test_disabled_fast_path_preserves_exit_status_and_signals(self):
        policy_path = Path(self.tmp.name) / "absent" / "policy.json"
        boot = ("import importlib.util,sys;"
                f"s=importlib.util.spec_from_file_location('c',{str(SCRIPTS / 'host-admit.py')!r});"
                "m=importlib.util.module_from_spec(s);s.loader.exec_module(m);"
                f"m.POLICY={str(policy_path)!r};m.fast(sys.argv[1:]);sys.exit(99)")
        for script, status in (("exit 7", 7), ("kill -TERM $$", -signal.SIGTERM)):
            done = subprocess.run([sys.executable, "-c", boot, "--entry", "rustc", "--", "sh", "-c", script],
                                  timeout=PROCESS_ALLOWANCE_S)
            self.assertEqual(done.returncode, status, script)
        done = subprocess.run([sys.executable, "-c", boot, "--entry", "rustc", "--", "/nonexistent/h"],
                              stderr=subprocess.PIPE, timeout=PROCESS_ALLOWANCE_S)
        self.assertEqual(done.returncode, 127, "a missing command answers like a shell")
        done = subprocess.run([sys.executable, "-c", boot, "--entry", "unknown", "--", "true"],
                              timeout=PROCESS_ALLOWANCE_S)
        self.assertEqual(done.returncode, 99, "an unknown entry is left to the validating parser")

    def test_cargo_runner_admits_test_binaries_and_passes_applications_through(self):
        cli = load_entrypoint()
        for path, application in (("target/debug/deps/build_slots-46fa5c3d", False),
                                  ("/tmp/rustdoctestXyz/rust_out", False),
                                  ("target/debug/story", True),
                                  ("target/debug/examples/demo", True)):
            self.assertEqual(cli.application_run("cargo-test-binary", [path]), application, path)
        self.assertFalse(cli.application_run("rustc", ["target/debug/story"]),
                         "only Cargo's runner entry classifies applications")
        app = Path(self.tmp.name) / "target" / "debug" / "story"
        app.parent.mkdir(parents=True)
        app.write_text("#!/bin/sh\necho \"entry=${STORYHOOK_HOST_ENTRY:-none}\"\nexit 5\n")
        app.chmod(0o755)
        (self.root / "policy.json").write_text("{}")
        boot = ("import importlib.util,sys;"
                f"s=importlib.util.spec_from_file_location('c',{str(SCRIPTS / 'host-admit.py')!r});"
                "m=importlib.util.module_from_spec(s);s.loader.exec_module(m);"
                f"m.POLICY={str(self.root / 'policy.json')!r};m.fast(sys.argv[1:]);sys.exit(99)")
        done = subprocess.run([sys.executable, "-c", boot, "--entry", "cargo-test-binary", "--", str(app)],
                              capture_output=True, text=True, timeout=PROCESS_ALLOWANCE_S)
        self.assertEqual((done.returncode, done.stdout.strip()), (5, "entry=none"),
                         "an application runs unchanged even with an enabled authority")
        test_binary = Path(self.tmp.name) / "target" / "debug" / "deps" / "unit-0"
        test_binary.parent.mkdir(parents=True)
        test_binary.write_text("#!/bin/sh\nexit 0\n")
        test_binary.chmod(0o755)
        done = subprocess.run([sys.executable, "-c", boot, "--entry", "cargo-test-binary", "--",
                               str(test_binary)], capture_output=True, timeout=PROCESS_ALLOWANCE_S)
        self.assertEqual(done.returncode, 99, "an enabled test binary is left to the adapter")

    def test_a_present_policy_never_falls_back_to_unbounded(self):
        (self.root / "policy.json").write_text("{}")
        refuse = lambda *_: (_ for _ in ()).throw(Refusal("incomplete or unsupported host policy"))
        status = adapter.main(["--entry", "rustc", "--", "true"], root=self.root, policy_loader=refuse)
        self.assertEqual(status, adapter.ADMISSION_STATUS)
        self.assertEqual(self.calls, [])

    def test_inheritance_requires_a_whole_grant_or_a_held_lease_descriptor(self):
        self.assertTrue(adapter.inherited(self.root, {adapter.GRANT: "t", adapter.REQUEST: "r"}))
        with self.assertRaises(Refusal):
            adapter.inherited(self.root, {adapter.GRANT: "t"})
        self.assertFalse(adapter.inherited(self.root, {}))
        forged = Path(self.tmp.name) / "lease-forged.lock"
        forged.write_text("")
        fd = os.open(forged, os.O_RDWR)
        self.addCleanup(os.close, fd)
        self.assertFalse(adapter.inherited(self.root, {adapter.LEASE_FD: str(fd)}),
                         "a file outside the authority proves nothing")
        guard = self.root / ("lease-" + "a" * 64 + ".lock")
        guard.write_text("")
        inherited = os.open(guard, os.O_RDWR)
        self.addCleanup(os.close, inherited)
        self.assertFalse(adapter.inherited(self.root, {adapter.LEASE_FD: str(inherited)}),
                         "an unheld guard is no live lease")
        self.assertFalse(adapter.inherited(self.root, {adapter.LEASE_FD: "999"}), "a closed descriptor")
        holder = os.open(guard, os.O_RDWR)
        self.addCleanup(os.close, holder)
        fcntl.flock(holder, fcntl.LOCK_EX)
        self.assertTrue(adapter.inherited(self.root, {adapter.LEASE_FD: str(inherited)}))
        self.assertTrue(adapter.inherited(self.root, {}), "found without the variable, by inode")

    def test_inherited_leaves_run_in_place_and_pools_divide_the_share(self):
        (self.root / "policy.json").write_text("{}")
        env = {adapter.GRANT: "t", adapter.REQUEST: "r", adapter.SHARE: "cpu=2500,memory=" + str(250 * MIB)}
        adapter.admit("cargo-test-binary", 1, ["bin"], root=self.root, env=env, execute=self.execute,
                      policy_loader=loader(fixture_policy()))
        self.assertEqual(self.calls[-1][2][adapter.SHARE], env[adapter.SHARE], "a leaf keeps its share")
        adapter.admit("plugin-pool", 4, ["bash", "pool"], root=self.root, env=env, execute=self.execute,
                      policy_loader=loader(fixture_policy()))
        child = self.calls[-1][2]
        self.assertEqual(child[adapter.UNITS], "2")
        self.assertEqual(adapter.parse_share(child[adapter.SHARE]), dict(cpu=1000, memory=100 * MIB))

    def test_drain_seconds_covers_two_cleanup_steps_and_a_sample(self):
        self.assertEqual(adapter.drain_seconds(self.root), 0, "disabled: no allowance needed")
        (self.root / "policy.json").write_text("{}")
        self.assertEqual(adapter.drain_seconds(self.root, loader(fixture_policy(cleanup_ms=1500))), 4)


class Root(AuthorityCase):
    def run_root(self, entry, units, journal=None, extra=()):
        record = self.dir / f"{entry}.json"
        env = dict(os.environ)
        if journal is not None:
            env["STORYHOOK_GATE_PROGRESS"] = str(journal)
        process = subprocess.Popen(self.cli("--entry", entry, "--units", str(units), "--",
                                            sys.executable, "-c", REPORT, str(record), *extra),
                                   env=env, stderr=subprocess.PIPE, text=True)
        return process, record

    def test_a_root_waits_visibly_then_runs_inside_its_grant(self):
        blocker = self.hold("blocker", 6000, 700)
        journal = self.dir / "gate.ndjson"
        journal.write_text("")
        process, record = self.run_root("plugin-pool", 4, journal)
        self.eventually(lambda: any(l["state"] == "queued" for l in self.leases()), "the root never queued")
        self.eventually(lambda: "waiting for host admission (plugin-pool)" in journal.read_text())
        self.release(blocker)
        _, stderr = process.communicate(timeout=PROCESS_ALLOWANCE_S)
        self.assertEqual(process.returncode, 0, stderr)
        seen = json.loads(record.read_text())
        self.assertEqual(seen["STORYHOOK_HOST_UNITS"], "4")
        self.assertEqual(adapter.parse_share(seen["STORYHOOK_HOST_SHARE"]), dict(cpu=1000, memory=100 * MIB))
        self.assertEqual(seen["STORYHOOK_HOST_ENTRY"], f"plugin-pool:{seen['ppid']}",
                         "the marker names the supervising adapter")
        records = [json.loads(line) for line in journal.read_text().splitlines()]
        labels = [(r.get("label"), r.get("status")) for r in records if r["kind"] == "activity"]
        self.assertIn(("waiting for host admission (plugin-pool)", "running"), labels)
        self.assertIn(("host admission granted (plugin-pool)", "passed"), labels)
        costs = [r["event"] for r in records if r["kind"] == "cost" and r["phase"] == "resource-wait"]
        self.assertEqual(costs, ["start", "end"])
        self.assertIn("waiting for host admission", stderr)
        self.assertEqual(self.client.call("status")["allocated"]["cpu"], 0)

    def test_nested_work_takes_no_new_lease(self):
        record = self.dir / "nested.json"
        nested = shlex.join(self.cli("--entry", "plugin-pool", "--units", "8", "--",
                                     sys.executable, "-c", REPORT, str(record)))
        done = subprocess.run(self.cli("--entry", "rust-pool", "--units", "3", "--", "sh", "-c",
                                       f"{nested}; exit $?"), timeout=PROCESS_ALLOWANCE_S)
        self.assertEqual(done.returncode, 0)
        seen = json.loads(record.read_text())
        self.assertEqual(seen["STORYHOOK_HOST_UNITS"], "1", "a one-unit worker share runs a nested pool serially")
        roots = [l for l in self.leases() if l["parent"] is None]
        self.assertEqual(len(roots), 1, f"nesting enqueued another root: {roots}")

    def test_a_signal_while_queued_cancels_before_launch(self):
        blocker = self.hold("blocker", 6000, 700)
        self.addCleanup(self.release, blocker)
        process, record = self.run_root("rustc", 1)
        self.eventually(lambda: any(l["state"] == "queued" for l in self.leases()))
        process.send_signal(signal.SIGTERM)
        process.communicate(timeout=PROCESS_ALLOWANCE_S)
        self.assertEqual(process.returncode, 128 + signal.SIGTERM)
        self.assertFalse(record.exists(), "the command ran after cancellation")
        self.assertTrue(all(l["state"] in ("cancelled", "released", "reserved", "running")
                            for l in self.leases()))

    def test_a_withdrawn_grant_is_an_admission_cause_not_a_test_failure(self):
        journal = self.dir / "gate.ndjson"
        journal.write_text("")
        env = dict(os.environ, STORYHOOK_GATE_PROGRESS=str(journal))
        hog = "b=b'x'*(400*1024*1024);import time;time.sleep(60)"
        done = subprocess.run(self.cli("--entry", "rustc", "--", sys.executable, "-c", hog),
                              env=env, stderr=subprocess.PIPE, text=True, timeout=PROCESS_ALLOWANCE_S * 2)
        self.assertEqual(done.returncode, adapter.ADMISSION_STATUS, done.stderr)
        records = [json.loads(line) for line in journal.read_text().splitlines()]
        causes = [(r["cause"], r["retryable"]) for r in records if r["kind"] == "admission"]
        self.assertEqual(causes, [("envelope-exceeded", False)])
        self.assertIn("envelope-exceeded", done.stderr)
        self.eventually(lambda: self.client.call("status")["allocated"]["cpu"] == 0)

    def test_an_absent_broker_is_a_retryable_refusal(self):
        self.stop_broker()
        journal = self.dir / "gate.ndjson"
        journal.write_text("")
        env = dict(os.environ, STORYHOOK_GATE_PROGRESS=str(journal))
        done = subprocess.run(self.cli("--entry", "rustc", "--", "true"), env=env,
                              stderr=subprocess.PIPE, text=True, timeout=PROCESS_ALLOWANCE_S)
        self.assertEqual(done.returncode, adapter.ADMISSION_STATUS, done.stderr)
        admission = [json.loads(l) for l in journal.read_text().splitlines()]
        self.assertEqual([(r["cause"], r["retryable"]) for r in admission], [("broker-unavailable", True)])


if __name__ == "__main__":
    unittest.main()
