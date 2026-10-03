"""Producer evidence tests exercise the production output parser and writer."""

import importlib.util
import json
from pathlib import Path
import sys
import os
import subprocess
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
spec = importlib.util.spec_from_file_location("activity_run", SCRIPTS / "activity-run.py")
activity = importlib.util.module_from_spec(spec)
spec.loader.exec_module(activity)
writer_spec = importlib.util.spec_from_file_location("gate_writer", SCRIPTS / "gate-progress-writer.py")
writer = importlib.util.module_from_spec(writer_spec)
writer_spec.loader.exec_module(writer)


class CaseEvidenceTests(unittest.TestCase):
    def test_browser_case_preserves_literal_identity_and_skips_unexecuted_cases(self):
        with tempfile.TemporaryDirectory() as root:
            journal = Path(root) / "browser.ndjson"
            source = (SCRIPTS.parent / "e2e/gate-progress-reporter.ts").as_uri()
            program = f'''import Reporter from {json.dumps(source)};
const reporter = new Reporter();
const test = {{ id: "unique-runner-case", titlePath: () => ["project", "literal \\\"quote\\\"", "line\\nend"],
                location: {{ file: "case.spec.ts" }} }};
reporter.onTestEnd(test, {{ status: "failed" }});
reporter.onTestEnd(test, {{ status: "skipped" }});
'''
            result = subprocess.run(["node", "--experimental-strip-types", "--input-type=module", "-e", program],
                                    env={**os.environ, "STORYHOOK_GATE_PROGRESS": str(journal),
                                         "STORYHOOK_GATE_PROGRESS_PATH": "release gate/e2e"},
                                    capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            rows = [json.loads(line) for line in journal.read_text().splitlines()]
            self.assertEqual(len(rows), 1)
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
