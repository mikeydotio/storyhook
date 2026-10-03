"""Producer evidence tests exercise the production output parser and writer."""

import importlib.util
import json
from pathlib import Path
import sys
import os
import subprocess
import tempfile
import unittest
from unittest import mock

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
spec = importlib.util.spec_from_file_location("activity_run", SCRIPTS / "activity-run.py")
activity = importlib.util.module_from_spec(spec)
spec.loader.exec_module(activity)
writer_spec = importlib.util.spec_from_file_location("gate_writer", SCRIPTS / "gate-progress-writer.py")
writer = importlib.util.module_from_spec(writer_spec)
writer_spec.loader.exec_module(writer)


class CaseEvidenceTests(unittest.TestCase):
    def test_missing_cost_journal_does_not_prevent_owned_workspace_restoration(self):
        from test_verifier_lifecycle import VerifierLifecycle
        fixture = VerifierLifecycle()
        fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        fixture.ensure()
        journal = fixture.root / "cost.ndjson"
        journal.touch()
        fixture.env["STORYHOOK_GATE_PROGRESS"] = str(journal)
        tree = fixture.git("rev-parse", fixture.head + "^{tree}")
        result = fixture.command("bash", str(SCRIPTS / "verify-pr.sh"), "--run-gate", "1",
                                 tree, fixture.base, fixture.head, str(fixture.wt), "--",
                                 "bash", "-c", 'rm "$STORYHOOK_GATE_PROGRESS"; exit 0', check=False)
        self.assertNotEqual(json.loads(result.stdout)["result"], "certified")
        self.assertEqual(fixture.git("-C", str(fixture.wt), "rev-parse", "--absolute-git-dir"),
                         str(fixture.admin), result.stderr)

    def test_production_supervisor_retains_pinned_context_and_real_lifecycle_phases(self):
        from test_verifier_lifecycle import VerifierLifecycle
        fixture = VerifierLifecycle()
        fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        fixture.ensure()
        journal = fixture.root / "cost.ndjson"
        marker = dict(kind="run", attempt_id="owned", execution_id="physical", generation=7)
        journal.write_text(json.dumps(marker) + "\n")
        fixture.env["STORYHOOK_GATE_PROGRESS"] = str(journal)
        fixture.env["STORYHOOK_VERIFICATION_ATTEMPT"] = "owned"
        tree = fixture.git("rev-parse", fixture.head + "^{tree}")
        command = ["bash", "-c", '"$STORYHOOK_GATE_PROGRESS_WRITER" case fixture fail "literal failure"; exit 3']
        result = fixture.command("bash", str(SCRIPTS / "verify-pr.sh"), "--run-gate", "1",
                                 tree, fixture.base, fixture.head, str(fixture.wt), "--", *command)
        verdict = json.loads(result.stdout)
        self.assertEqual(verdict["result"], "tests-failed", verdict)
        rows = [json.loads(line) for line in journal.read_text().splitlines()]
        self.assertEqual(rows[0], marker)
        context = next(row["inputs"] for row in rows if row["kind"] == "context")
        self.assertEqual((context["base"], context["head"], context["tree"]),
                         (fixture.base, fixture.head, tree))
        self.assertEqual(context["contract"]["argv"], command)
        self.assertGreater(context["resources"]["nofile"][0], 0)
        self.assertEqual(context["cache"]["build_artifacts"], "unmeasured")
        self.assertIn("python", context["toolchain"])
        spans = [row for row in rows if row["kind"] == "cost"]
        for phase in ("workspace", "resource-wait", "cleanup", "verdict"):
            starts = [row for row in spans if row["phase"] == phase and row["event"] == "start"]
            self.assertTrue(starts, phase)
            for start in starts:
                end = next(row for row in spans if row["id"] == start["id"] and row["event"] == "end")
                self.assertGreaterEqual(end["monotonic_ns"], start["monotonic_ns"])
        self.assertTrue(any(row.get("name") == "literal failure" for row in rows))
        self.assertEqual(fixture.git("-C", str(fixture.wt), "status", "--porcelain"), "")

    def test_build_and_discovery_emit_real_intervals(self):
        from cargo_diagnostics import run_build
        from test_discovery import Discovery
        with tempfile.TemporaryDirectory() as root:
            journal = Path(root) / "cost.ndjson"
            journal.touch()
            with mock.patch.dict(os.environ, {"STORYHOOK_GATE_PROGRESS": str(journal)}):
                result = run_build([sys.executable, "-c", 'print(\'{"reason":"build-finished","success":false}\'); exit(3)'], None)
                self.assertEqual(result, 3)
                script = Path(root) / "list-tests"
                script.write_text('#!/bin/sh\ncase "$*" in *--ignored*) ;; *) echo "named: test";; esac\n')
                script.chmod(0o700)
                self.assertEqual(Discovery(dict(os.environ), lambda: False).count_tests(str(script), []), 1)
            rows = [json.loads(line) for line in journal.read_text().splitlines()]
            self.assertEqual([(r["phase"], r["event"]) for r in rows],
                             [("compile-link", "start"), ("compile-link", "end"),
                              ("discovery", "start"), ("discovery", "end")])

    def test_browser_case_preserves_literal_identity_and_skips_unexecuted_cases(self):
        with tempfile.TemporaryDirectory() as root:
            journal = Path(root) / "browser.ndjson"
            module = Path(root) / "reporter.mts"
            module.write_bytes((SCRIPTS.parent / "e2e/gate-progress-reporter.ts").read_bytes())
            source = module.as_uri()
            program = f'''import Reporter from {json.dumps(source)};
const reporter = new Reporter();
reporter.onBegin();
const test = {{ id: "unique-runner-case", titlePath: () => ["project", "literal \\\"quote\\\"", "line\\nend"],
                location: {{ file: "case.spec.ts" }} }};
reporter.onTestEnd(test, {{ status: "failed" }});
reporter.onTestEnd(test, {{ status: "skipped" }});
reporter.onEnd();
'''
            result = subprocess.run(["node", "--experimental-strip-types", "--input-type=module", "-e", program],
                                    env={**os.environ, "STORYHOOK_GATE_PROGRESS": str(journal),
                                         "STORYHOOK_GATE_PROGRESS_PATH": "release gate/e2e"},
                                    capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            rows = [json.loads(line) for line in journal.read_text().splitlines()]
            self.assertEqual([r["kind"] for r in rows], ["cost", "case", "cost"])
            self.assertEqual((rows[0]["phase"], rows[0]["event"], rows[2]["event"]),
                             ("execution", "start", "end"))
            self.assertEqual(rows[0]["id"], rows[2]["id"])
            self.assertGreaterEqual(rows[2]["monotonic_ns"], rows[0]["monotonic_ns"])
            rows = [rows[1]]
            self.assertEqual(rows[0]["name"], 'project > literal "quote" > line\nend')
            self.assertEqual(rows[0]["target"], "case.spec.ts")
            self.assertEqual(rows[0]["identity"], "unique-runner-case")
            self.assertEqual(rows[0]["title_path"], ["project", 'literal "quote"', "line\nend"])

    def test_shell_named_case_uses_valid_json_for_literal_names(self):
        with tempfile.TemporaryDirectory() as root:
            journal = Path(root) / "progress.ndjson"
            name = 'literal "quote" \\ slash\nnext'
            result = subprocess.run(["bash", str(SCRIPTS / "gate-progress.sh"), "case",
                                     "release gate/plugin", "fail", name],
                                    env={**os.environ, "STORYHOOK_GATE_PROGRESS": str(journal)},
                                    capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(journal.read_text())["name"], name)

    def test_phase_events_are_typed_and_include_monotonic_samples(self):
        value = writer.record(["cost", "start", "discovery", "id-1", "unit"])
        self.assertEqual(value["kind"], "cost")
        self.assertEqual(value["phase"], "discovery")
        self.assertEqual(value["path"], "release gate/unit")
        self.assertIsInstance(value["monotonic_ns"], int)
        with self.assertRaises(writer.Refused):
            writer.record(["cost", "start", "invented-phase", "id-1", "unit"])

    def test_portable_named_case_preserves_legacy_shape(self):
        self.assertNotIn("name", writer.record(["case", "unit", "pass"]))
        self.assertEqual(writer.record(["case", "unit", "fail", "parse::bad"])["name"], "parse::bad")
        name = 'literal "quote" \\ slash\nnext'
        encoded = json.dumps(writer.record(["case", "unit", "fail", name]))
        self.assertEqual(json.loads(encoded)["name"], name)

    def test_failed_case_retains_exact_identity(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "progress.ndjson"
            observer = activity.GateProgress(str(path), "release gate/rust-suite")
            observer.consume(b"test parser::escaped_names ... FAILED\n")
            record = json.loads(path.read_text())
            self.assertEqual(record["name"], "parser::escaped_names")
            self.assertEqual(record["outcome"], "fail")


if __name__ == "__main__":
    unittest.main()
