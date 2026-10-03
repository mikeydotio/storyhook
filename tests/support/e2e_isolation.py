"""File-isolation contracts; no browser or real user store is needed here."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("isolation", ROOT / "scripts/e2e-isolation.py")
isolation = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(isolation)


class IsolationTests(unittest.TestCase):
    """Each scenario owns the exact lists, verdicts and reports it inspects."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="e2e-isolation-", dir="/tmp")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.lists = self.root / "slices"
        self.lists.mkdir()

    def plan(self, text="chromium\ta [x].spec.ts\t3\nwebkit\ta [x].spec.ts\t3\nchromium\tb.spec.ts\t1\n"):
        return isolation.plan(text, self.lists, ["chromium", "webkit"])

    def evidence(self, row, code=0, *, rerun=False, executed=True, count=None):
        root = self.root / "reruns" if rerun else self.root
        for name in ("verdicts", "executed", "selected", "logs", "reports"):
            (root / name).mkdir(parents=True, exist_ok=True)
        (root / "verdicts" / row["slice"]).write_text(str(code))
        (root / "selected" / row["slice"]).write_text(str(row["count"] if count is None else count))
        (root / "logs" / (row["slice"] + ".log")).write_text("attempt\n")
        if executed:
            (root / "executed" / row["slice"]).write_text(str(code))
            tests = [{"projectName": row["project"], "status": "unexpected" if code else "expected",
                      "results": [{"status": "failed" if code else "passed"}]}] * row["count"]
            (root / "reports" / (row["slice"] + ".json")).write_text(json.dumps({
                "errors": [], "suites": [{"specs": [{"file": row["file"], "tests": tests}]}]}))

    def test_exact_partition_and_stable_identity_independent_of_counts(self):
        rows = self.plan()
        self.assertEqual(len(rows), 3)
        self.assertEqual(sum(r["count"] for r in rows), 7)
        self.assertEqual(len({r["slice"] for r in rows}), 3)
        for row in rows:
            self.assertEqual(Path(row["list"]).read_text(), f'[{row["project"]}] › {row["file"]}\n')
        changed = self.plan("webkit\ta [x].spec.ts\t90\nchromium\tb.spec.ts\t2\nchromium\ta [x].spec.ts\t1\n")
        self.assertEqual({(r["project"], r["file"]): r["slice"] for r in rows},
                         {(r["project"], r["file"]): r["slice"] for r in changed})

    def test_malformed_unknown_duplicate_and_empty_selections_fail_before_writes(self):
        for text in ("", "chromium\ta.ts\t0\n", "unknown\ta.ts\t1\n", "chromium\ta.ts\tNaN\n",
                     "chromium\ta.ts\t1\nchromium\ta.ts\t1\n", "chromium\t../a.ts\t1\n",
                     "chromium\t\t1\n", "chromium\ta.ts\t1\textra\n"):
            with self.subTest(text=text), self.assertRaises(ValueError):
                self.plan(text)
            self.assertEqual(list(self.lists.iterdir()), [])

    def test_all_success_requires_complete_counts_and_execution_evidence(self):
        rows = self.plan()
        for row in rows:
            self.evidence(row)
        result = isolation.report(self.root)
        self.assertEqual(result["exit_code"], 0)
        self.assertEqual(result["selected_tests"], 7)
        self.assertEqual(result["failures"], [])
        (self.root / "executed" / rows[0]["slice"]).unlink()
        self.assertNotEqual(isolation.report(self.root)["exit_code"], 0)
        self.evidence(rows[0], count=99)
        self.assertNotEqual(isolation.report(self.root)["exit_code"], 0)

    def test_serial_rerun_never_erases_original_failure(self):
        for rerun_code, executed, outcome in ((0, True, "not reproduced on serial rerun"),
                                             (1, True, "failed again"),
                                             (1, False, "rerun infrastructure failure")):
            with self.subTest(outcome=outcome):
                rows = self.plan("chromium\ta.spec.ts\t1\n")
                row = rows[0]
                self.evidence(row, 1)
                self.evidence(row, rerun_code, rerun=True, executed=executed)
                if not executed:
                    (self.root / "reruns/executed" / row["slice"]).unlink(missing_ok=True)
                report = isolation.report(self.root)
                self.assertEqual(report["exit_code"], 1)
                self.assertEqual(report["failures"][0]["outcome"], outcome)
                self.assertEqual(report["failures"][0]["file"], "a.spec.ts")
                self.assertIn("--test-list=", report["failures"][0]["rerun_command"])
                self.assertEqual((self.root / "verdicts" / row["slice"]).read_text(), "1")

    def test_dead_slice_and_missing_rerun_are_failures(self):
        self.plan("chromium\ta.spec.ts\t1\n")
        report = isolation.report(self.root)
        self.assertEqual(report["exit_code"], 1)
        self.assertEqual(report["failures"][0]["outcome"], "rerun infrastructure failure")

    def test_red_process_without_failed_tests_is_infrastructure(self):
        row = self.plan("chromium\ta.spec.ts\t1\n")[0]
        self.evidence(row)
        (self.root / "verdicts" / row["slice"]).write_text("137")
        (self.root / "executed" / row["slice"]).write_text("137")
        self.assertFalse(isolation.attempt(self.root, row)["complete"])

    def test_incompatible_options_fail_before_artifact_removal_or_build(self):
        for option in ("--shard=1/2", "--test-list=/missing", "--shard", "--test-list",
                       "--workers=2", "--retries=1", "--config=other", "--reporter=json", "--list",
                       "--output", "--output=/tmp/another-run"):
            result = subprocess.run(["/bin/bash", str(ROOT / "scripts/run-e2e.sh"),
                                     "--isolate-files", option], capture_output=True, text=True)
            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertIn("cannot combine", result.stderr)
            self.assertNotIn("building", result.stderr)

    def test_wrong_project_file_interrupted_and_invalid_reports_fail(self):
        row = self.plan("chromium\ta.spec.ts\t1\n")[0]
        for mutation in ("project", "file", "interrupted", "empty", "errors", "invalid"):
            self.evidence(row)
            path = self.root / "reports" / (row["slice"] + ".json")
            report = json.loads(path.read_text())
            spec = report["suites"][0]["specs"][0]
            if mutation == "project":
                spec["tests"][0]["projectName"] = "webkit"
            elif mutation == "file":
                spec["file"] = "other.spec.ts"
            elif mutation == "interrupted":
                spec["tests"][0]["results"][0]["status"] = "interrupted"
            elif mutation == "empty":
                spec["tests"] = []
            elif mutation == "errors":
                report["errors"] = [{"message": "global setup failed"}]
            path.write_text("not json" if mutation == "invalid" else json.dumps(report))
            with self.subTest(mutation=mutation):
                self.assertFalse(isolation.attempt(self.root, row)["complete"])

    def test_explicit_results_root_never_overwrites_existing_files(self):
        marker = self.root / "retain.txt"
        marker.write_text("keep this evidence")
        alias = self.root / "alias"
        alias.symlink_to(self.lists, target_is_directory=True)
        for value in ("", "relative", str(self.root), str(alias)):
            environment = dict(os.environ, STORYHOOK_E2E_RESULTS_DIR=value)
            result = subprocess.run(["/bin/bash", str(ROOT / "scripts/run-e2e.sh"), "--isolate-files"],
                                    env=environment, capture_output=True, text=True)
            with self.subTest(value=value):
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertNotIn("building", result.stderr)
                self.assertEqual(marker.read_text(), "keep this evidence")


if __name__ == "__main__":
    unittest.main()
