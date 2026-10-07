"""Real broker and session supervision at the causal resource boundary."""

import json
import shutil
from pathlib import Path
import sys
import time
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
from host_admit_fixture import AuthorityCase, MIB, PROCESS_ALLOWANCE_S, fixture_policy, loader
from host_admission import diagnosis
from host_admission.policy import Refusal
from host_admission.reservation import Admission
from host_admission.policy import Policy
import copy

SCRIPTS = Path(__file__).resolve().parents[1]

# The real child waits for the broker's real native usage observation, so a fast
# exit cannot make this positive case depend on the sampler winning a race.
CHILD = """
import os,sys,time
sys.path.insert(0,sys.argv[1])
from host_admission.client import Client
client=Client(sys.argv[2],timeout_ms=int(sys.argv[3]))
deadline=time.monotonic()+float(sys.argv[4])
while time.monotonic()<deadline:
    row=client.call('inspect',id=os.environ['STORYHOOK_HOST_REQUEST'],token=os.environ['STORYHOOK_HOST_GRANT'])
    if all(v is not None for v in row.get('peaks',{}).values()) and row.get('peaks'):
        break
    time.sleep(0.01)
else:
    raise RuntimeError('native usage was not observed')
sys.exit(int(sys.argv[5]))
"""


class DiagnosticAdmission(AuthorityCase):
    policy_changes = {"workloads": dict(fixture_policy()["workloads"],
                                       **{"causal-rust": dict(cpu=4000, memory=400*MIB)})}

    def invoke(self, code=0, **changes):
        """Run an actual managed child with a unique durable resource binding."""
        journal = self.dir / f"journal-{len(list(self.dir.glob('journal-*')))}"
        journal.touch(mode=0o600)
        arguments = dict(project="causal-fixture", binding=dict(attempt_id="attempt", execution_id=journal.name, generation=7),
                         journal=journal, request_id=journal.name, deadline=diagnosis.monotonic()+PROCESS_ALLOWANCE_S,
                         root=self.root, policy_loader=loader(self.value))
        arguments.update(changes)
        command = [sys.executable, "-B", "-c", CHILD, str(SCRIPTS), str(self.root),
                   str(self.value["stale_ms"]), str(PROCESS_ALLOWANCE_S), str(code)]
        return diagnosis.run(command, **arguments), journal

    def test_actual_success_and_failure_keep_resource_evidence_separate_from_verdict(self):
        for code in (0, 101):
            (result, evidence), journal = self.invoke(code)
            self.assertEqual(result, code)
            self.assertEqual(evidence["version"], 1)
            self.assertEqual(evidence["binding"]["execution_id"], journal.name)
            self.assertEqual(evidence["lease"]["state"], "released")
            self.assertTrue(evidence["supported"])
            self.assertTrue(evidence["cleanup_complete"])
            self.assertNotIn("token", json.dumps(evidence))
            events = [json.loads(line)["observation"] for line in journal.read_text().splitlines()]
            self.assertTrue(any(e["event"] == "release" for e in events))
            self.assertTrue(all(row["state"] == "released" for row in self.leases()))

    def test_absent_policy_missing_workload_and_expired_budget_never_launch(self):
        with self.assertRaises((Refusal, Admission)):
            self.invoke(root=self.dir / "absent", policy_loader=None)
        missing = dict(self.value, workloads={"other": dict(cpu=1000, memory=100*MIB)})
        with self.assertRaises((Refusal, Admission)):
            self.invoke(policy_loader=loader(missing))
        with self.assertRaises((Refusal, Admission)):
            self.invoke(deadline=diagnosis.monotonic()-1)
        self.assertEqual(self.leases(), [])

    def test_pressure_withdrawal_settles_but_cannot_supply_supported_evidence(self):
        # The child changes only its fixture sensor input, not broker behavior.
        marker = self.dir / "pressure"
        command = [sys.executable, "-B", "-c",
                   "import pathlib,sys,time; pathlib.Path(sys.argv[1]).write_text('1000'); time.sleep(float(sys.argv[2]))",
                   str(marker), str(PROCESS_ALLOWANCE_S)]
        journal = self.dir / "pressure-journal"
        journal.touch(mode=0o600)
        result, evidence = diagnosis.run(command, project="pressure-fixture",
            binding=dict(attempt_id="a", execution_id="e", generation=7), journal=journal,
            request_id="pressure", deadline=diagnosis.monotonic()+PROCESS_ALLOWANCE_S,
            root=self.root, policy_loader=loader(self.value))
        self.assertNotEqual(result, 0)
        self.assertFalse(evidence["supported"])
        self.assertTrue(evidence["cleanup_complete"])
        self.assertEqual(self.leases()[0]["state"], "released")

    def test_native_cancellation_drains_the_session_without_supporting_attribution(self):
        command = [sys.executable, "-B", "-c",
                   "import os,signal,time; os.kill(os.getppid(),signal.SIGTERM); time.sleep(float(__import__('sys').argv[1]))",
                   str(PROCESS_ALLOWANCE_S)]
        journal = self.dir / "cancel-journal"
        journal.touch(mode=0o600)
        result, evidence = diagnosis.run(command, project="cancel-fixture",
            binding=dict(attempt_id="a", execution_id="e", generation=7), journal=journal,
            request_id="cancel", deadline=diagnosis.monotonic()+PROCESS_ALLOWANCE_S,
            root=self.root, policy_loader=loader(self.value))
        self.assertNotEqual(result, 0)
        self.assertFalse(evidence["supported"])
        self.assertTrue(evidence["cleanup_complete"])
        self.assertEqual(self.leases()[0]["state"], "released")

    def test_missing_foreign_and_withdrawn_observations_cannot_become_support(self):
        (_, evidence), _ = self.invoke()
        policy = Policy(self.value, self.value["host"], fixture=True)
        status = dict(authority=evidence["authority"], policy=evidence["policy"],
                      boot=evidence["boot"], pressure="ready")
        deadline = diagnosis.monotonic()+PROCESS_ALLOWANCE_S
        for change in ("unknown-usage", "overrun", "foreign-binding", "missing-cleanup", "withdrawn", "policy", "boot", "pressure", "deadline"):
            altered = copy.deepcopy(evidence)
            after = dict(status)
            ended = diagnosis.monotonic()
            if change == "unknown-usage": altered["lease"]["peaks"]["memory"] = None
            if change == "overrun": altered["lease"]["peaks"]["cpu"] = altered["lease"]["resources"]["cpu"]+1
            if change == "foreign-binding": altered["lease"]["binding"]["generation"] += 1
            if change == "missing-cleanup": altered["events"] = [e for e in altered["events"] if e["event"] != "cleanup"]
            if change == "withdrawn": altered["events"][-1]["event"] = "cancel"
            if change in ("policy", "boot", "pressure"): after[change] = "different"
            if change == "deadline": ended = deadline
            self.assertFalse(diagnosis._supported(policy, status, after, altered["lease"],
                             altered["events"], evidence["binding"], ended, deadline), change)

    def test_native_rust_pipeline_builds_lists_and_runs_the_selected_case(self):
        source = self.dir / "source"
        (source / "src").mkdir(parents=True)
        (source / "tests").mkdir()
        (source / "Cargo.toml").write_text("[package]\nname='subject'\nversion='0.1.0'\nedition='2021'\nbuild=false\n")
        (source / "Cargo.lock").write_text("version=4\n[[package]]\nname='subject'\nversion='0.1.0'\n")
        (source / "src/lib.rs").write_text("pub fn answer() -> u32 { 41 }\n")
        (source / "tests/contract.rs").write_text("#[test] fn answer() { assert_eq!(subject::answer(), 42); }\n")
        output = self.dir / "output"
        output.mkdir()
        tools = {name: str(Path(shutil.which(name)).resolve(strict=True)) for name in ("cargo", "rustc")}
        request = dict(version=1, clock="CLOCK_MONOTONIC", source=str(source), output=str(output), package="subject", target="contract", case="answer",
                       tools=tools, wrapper=str(SCRIPTS / "rustc-slot.py"),
                       lock_root=str(Path.home() / ".local/state/storyhook/locks"),
                       deadline=diagnosis.monotonic()+PROCESS_ALLOWANCE_S)
        request_path = self.dir / "request.json"
        request_path.write_text(json.dumps(dict(pipeline=request)))
        journal = self.dir / "pipeline-journal"
        journal.touch(mode=0o600)
        result, evidence = diagnosis.run([sys.executable, "-B", str(SCRIPTS / "attribution-rust.py"), "--worker", str(request_path)],
            project="rust-fixture", binding=dict(attempt_id="a", execution_id="e", generation=7), journal=journal,
            request_id="pipeline", deadline=request["deadline"], root=self.root, policy_loader=loader(self.value))
        self.assertEqual(result, 0, "worker success reports completion, not test success")
        self.assertTrue(evidence["supported"], evidence)
        observed = json.loads((output / "observation.json").read_text())
        self.assertEqual(observed["build"]["exit"], 0)
        self.assertEqual(observed["listing"]["exit"], 0)
        self.assertEqual(observed["run"]["exit"], 101)
        self.assertEqual((output / "listing.stdout").read_text(), "answer: test\n\n1 test, 0 benchmarks\n")
        self.assertIn("left: 41", (output / "run.stdout").read_text())
        self.assertEqual(observed["artifact_before"], observed["artifact_after"])
        self.assertTrue(all(row["state"] == "released" for row in self.leases()))


if __name__ == "__main__":
    unittest.main()
