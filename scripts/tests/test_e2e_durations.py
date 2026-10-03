"""Scheduling history and selection receipts, exercised through production entry points."""

import importlib.util
import fcntl
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from load_grace import contention, patience

ROOT = Path(__file__).resolve().parents[2]
HELPER = ROOT / "scripts/e2e-durations.py"
REPORTER = ROOT / "e2e/slice-reporter.ts"
PLAYWRIGHT = ROOT / "e2e/node_modules/@playwright/test"


class Fixture(unittest.TestCase):
    """Private files and bounded processes for every scenario."""

    def setUp(self):
        """Keep file-heavy fixtures outside Spotlight's indexed TMPDIR."""
        self.scratch = tempfile.TemporaryDirectory(prefix="sh812-", dir="/tmp")
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.cache = self.root / "durations.tsv"

    def run_command(self, args, **kwargs):
        """Bound a real subprocess with the shared contention policy."""
        return subprocess.run(args, capture_output=True, text=True,
                              timeout=patience(60, contention()), **kwargs)

    def helper(self, *args, **kwargs):
        """Invoke the same command the browser harness uses."""
        return self.run_command([sys.executable, str(HELPER), *map(str, args)], **kwargs)

    def report(self, file="specs/a.spec.ts", observed=100, **changes):
        """A complete successful observation, mutable only for negative cases."""
        value = dict(version=1, status="passed", observed=observed, errors=[],
                     files=[dict(project="chromium", file=file, count=2,
                                 completed=2, passed=2, skipped=0, retries=0, seconds=12.5)])
        value.update(changes)
        path = self.root / (file.replace("/", "-") + ".json")
        path.write_text(json.dumps(value))
        return path


class PlannerTests(Fixture):
    """Weights affect scheduling, never coverage counts."""

    def plan(self, lines, budget=3):
        """Drive the Bash 3.2 production planner directly."""
        lists = self.root / "lists"
        lists.mkdir(exist_ok=True)
        return self.run_command(["/bin/bash", "-c",
            'set -euo pipefail; . "$1"; e2e_pool_plan "$2" "$3" chromium webkit',
            "scenario", str(ROOT / "scripts/e2e-pool.sh"), str(budget), str(lists)], input=lines)

    def test_weights_allocate_pack_and_admit_without_changing_counts(self):
        """The slower engine receives the spare slice despite equal test counts."""
        out = self.plan("chromium\ta.spec.ts\t5\t1\nchromium\tb.spec.ts\t5\t1\n"
                        "webkit\ta.spec.ts\t5\t30\nwebkit\tb.spec.ts\t5\t20\n")
        self.assertEqual(out.returncode, 0, out.stderr)
        rows = [line.split("\t") for line in out.stdout.splitlines()]
        self.assertEqual([r[0] for r in rows], ["webkit.1of2", "webkit.2of2", "chromium"])
        self.assertEqual([int(r[3]) for r in rows], [5, 5, 10])
        self.assertEqual(sum(len(Path(r[2]).read_text().splitlines()) for r in rows), 4)

    def test_invalid_weights_fail_before_writing_a_plan(self):
        """NaN, infinity and zero cannot defeat longest-first packing."""
        for weight in ["0", "-1", "NaN", "inf", "1e999", "", "oops"]:
            with self.subTest(weight=weight):
                out = self.plan(f"chromium\ta.spec.ts\t1\t{weight}\n")
                self.assertEqual(out.returncode, 2, out)
                self.assertEqual(out.stdout, "")

    def test_weighted_partition_is_exact_at_every_budget(self):
        """Extreme imbalances never produce duplicate, missing or empty slices."""
        lines = "chromium\ta.spec.ts\t1\t90\nchromium\tb.spec.ts\t7\t0.1\nchromium\tc.spec.ts\t2\t1\nwebkit\ta.spec.ts\t1\t120\nwebkit\tb.spec.ts\t7\t2\n"
        for budget in range(1, 9):
            out = self.plan(lines, budget)
            self.assertEqual(out.returncode, 0, out.stderr)
            rows = [row.split("\t") for row in out.stdout.splitlines()]
            actual = []
            for row in rows:
                files = Path(row[2]).read_text().splitlines()
                self.assertTrue(files)
                actual.extend(files)
            self.assertEqual(sorted(actual), sorted([f"[{p}] › {f}" for p, f, _, _ in
                                                     (line.split("\t") for line in lines.splitlines())]))
            self.assertEqual(sum(int(row[3]) for row in rows), 18)

    def test_cold_cache_and_equal_weights_have_identical_plans(self):
        """Weights equal to counts preserve old allocation and stable ties."""
        counts = "chromium\ta.spec.ts\t5\nchromium\tb.spec.ts\t5\nwebkit\ta.spec.ts\t5\n"
        old = self.plan(counts)
        weighted = self.plan("".join(f"{line}\t{line.split(chr(9))[-1]}\n" for line in counts.splitlines()))
        self.assertEqual(old.returncode, 0, old.stderr)
        self.assertEqual(weighted.stdout, old.stdout, weighted.stderr)


class CacheTests(Fixture):
    """Optional cache state must never change the selected tests or verdict."""

    def test_round_trip_and_changed_count_fallback(self):
        """Only the same project/file/count reuses measured seconds."""
        report = self.report()
        out = self.helper("merge", self.cache, report)
        self.assertEqual(out.returncode, 0, out.stderr)
        out = self.helper("weights", self.cache, input="chromium\tspecs/a.spec.ts\t2\nwebkit\tspecs/a.spec.ts\t2\nchromium\tspecs/a.spec.ts\t3\n")
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(out.stdout.splitlines(), ["chromium\tspecs/a.spec.ts\t2\t12.5", "webkit\tspecs/a.spec.ts\t2\t2", "chromium\tspecs/a.spec.ts\t3\t3"])

    def test_bad_history_is_diagnosed_and_falls_back(self):
        """History is advisory; invalid observations never enter the planner."""
        self.cache.write_text("1\tchromium\tspecs/a.spec.ts\t2\tNaN\t100\n")
        out = self.helper("weights", self.cache, input="chromium\tspecs/a.spec.ts\t2\n")
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(out.stdout, "chromium\tspecs/a.spec.ts\t2\t2\n")
        self.assertIn("invalid", out.stderr)

    def test_valid_weight_extremes_remain_positive_plain_decimals(self):
        """Serialization cannot turn advisory positive history into a bad plan."""
        for seconds in [1e-12, 0.0000000001, 1e12]:
            self.cache.write_text(f"1\tchromium\ta.spec.ts\t2\t{seconds}\t100\n")
            out = self.helper("weights", self.cache, input="chromium\ta.spec.ts\t2\n")
            self.assertEqual(out.returncode, 0, out.stderr)
            weight = out.stdout.strip().split("\t")[-1]
            self.assertRegex(weight, r"^[0-9]+([.][0-9]+)?$")
            self.assertEqual(float(weight), seconds)

    def test_failed_interrupted_skipped_and_retried_files_do_not_replace_history(self):
        """A shortened or repeated execution is not a whole-file duration."""
        out = self.helper("merge", self.cache, self.report())
        self.assertEqual(out.returncode, 0, out.stderr)
        before = self.cache.read_text()
        for field, value in [("status", "failed"), ("status", "interrupted"), ("skipped", 1), ("retries", 1), ("completed", 1)]:
            path = self.report(observed=200)
            report = json.loads(path.read_text())
            if field == "status":
                report[field] = value
            else:
                report["files"][0][field] = value
            path.write_text(json.dumps(report))
            out = self.helper("merge", self.cache, path)
            self.assertEqual(out.returncode, 0, out.stderr)
            self.assertEqual(self.cache.read_text(), before)

    def test_concurrent_writers_keep_both_keys_and_newest_timestamp(self):
        """Merges reload under the lock; old runs cannot overwrite newer evidence."""
        a, b = self.report(), self.report("specs/b.spec.ts")
        procs = [subprocess.Popen([sys.executable, str(HELPER), "merge", str(self.cache), str(p)],
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True) for p in [a, b]]
        for proc in procs:
            _, err = proc.communicate(timeout=patience(60, contention()))
            self.assertEqual(proc.returncode, 0, err)
        # A busy cache is explicitly advisory: replay after contention to prove merging.
        for p in [a, b]:
            self.assertEqual(self.helper("merge", self.cache, p).returncode, 0)
        before = self.cache.read_text()
        self.helper("merge", self.cache, self.report(observed=50))
        self.assertEqual(self.cache.read_text(), before)
        self.assertEqual(len(before.splitlines()), 2)

    def test_missing_and_invalid_receipts_fail_loudly(self):
        """Shell validation closes Playwright's swallowed-reporter-error path."""
        expected = self.root / "expected.tsv"
        expected.write_text("chromium\tspecs/a.spec.ts\t2\n")
        for path in [self.root / "absent", self.report(status="passed", files=[])]:
            out = self.helper("validate", path, expected)
            self.assertNotEqual(out.returncode, 0)
            self.assertIn("e2e-durations:", out.stderr)
        self.assertEqual(self.helper("validate", self.report(), expected).returncode, 0)
        expected.write_text("webkit\tspecs/a.spec.ts\t2\n")
        self.assertNotEqual(self.helper("validate", self.report(), expected).returncode, 0)

    def test_invalid_duration_never_passes_receipt_validation(self):
        """Malformed JSON values cannot exploit arithmetic coercion."""
        expected = self.root / "expected.tsv"
        expected.write_text("chromium\tspecs/a.spec.ts\t2\n")
        for seconds in [-0.5, True, None, "12", float("nan"), float("inf")]:
            report = self.report()
            value = json.loads(report.read_text())
            value["files"][0]["seconds"] = seconds
            report.write_text(json.dumps(value))
            self.assertNotEqual(self.helper("validate", report, expected).returncode, 0)

    def test_busy_cache_warns_without_overwriting_or_waiting(self):
        """An unrelated run holding the cache lock cannot delay test completion."""
        self.helper("merge", self.cache, self.report())
        before = self.cache.read_text()
        with open(str(self.cache) + ".lock", "a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            out = self.helper("merge", self.cache, self.report("specs/b.spec.ts"))
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertIn("cannot update history", out.stderr)
        self.assertEqual(self.cache.read_text(), before)

    def test_failed_atomic_replace_preserves_history_and_removes_temporary(self):
        """A failed publish cannot truncate the last usable history."""
        from unittest.mock import patch
        spec = importlib.util.spec_from_file_location("e2e_durations", HELPER)
        helper = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(helper)
        helper.merge(self.cache, [self.report()])
        before = self.cache.read_text()
        with patch.object(helper.os, "replace", side_effect=OSError("injected publication failure")):
            helper.merge(self.cache, [self.report("specs/b.spec.ts")])
        self.assertEqual(self.cache.read_text(), before)
        self.assertEqual(list(self.root.glob("e2e-durations-*")), [])


class ReporterTests(Fixture):
    """The actual Playwright process must produce authoritative slice receipts."""

    def run_suite(self, body, expected_count=1, reporter_path=None):
        """Load the production reporter with fixture tests and no browser/daemon."""
        config = self.root / "playwright.config.cjs"
        config.write_text("module.exports = " + json.dumps(dict(testDir=str(self.root), workers=1,
            retries=0, reporter=[[str(reporter_path or REPORTER)]])))
        (self.root / "a.spec.cjs").write_text("const {test, expect} = require(" + json.dumps(str(PLAYWRIGHT)) + ");\n" + body)
        report = self.root / "report.json"
        expected = self.root / "expected.tsv"
        expected.write_text(f"fixture\ta.spec.cjs\t{expected_count}\n")
        # Project name and root are explicit so paths match the outer list's namespace.
        config.write_text(config.read_text()[:-1] + ',"projects":[{"name":"fixture"}]}')
        env = dict(os.environ, E2E_SLICE_REPORT=str(report),
                   E2E_SLICE_EXPECTED=str(expected), CI="1")
        for key in ["NO_COLOR", "STORYHOOK_GATE_PROGRESS", "STORYHOOK_GATE_PROGRESS_PATH"]:
            env.pop(key, None)
        report.unlink(missing_ok=True)
        out = self.run_command(["node", str(PLAYWRIGHT / "cli.js"), "test", "--config", str(config)], env=env, cwd=self.root)
        return out, report, expected

    def test_success_is_recorded_and_validated(self):
        """A complete run records the selected file and positive duration."""
        out, report, expected = self.run_suite("test('pass', async () => {expect(1).toBe(1)});")
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
        self.assertEqual(self.helper("validate", report, expected).returncode, 0)
        value = json.loads(report.read_text())
        self.assertEqual(value["files"][0]["passed"], 1)
        self.assertGreater(value["files"][0]["seconds"], 0)

    def test_discovery_mismatch_cannot_pass(self):
        """Changing a plan count cannot silently report partial coverage."""
        out, report, expected = self.run_suite("test('pass', async () => {});", expected_count=2)
        self.assertNotEqual(out.returncode, 0, out.stdout + out.stderr)
        self.assertNotEqual(self.helper("validate", report, expected).returncode, 0)

    def test_expected_failure_is_successful_and_cacheable(self):
        """A declared failure that occurs is a successful Playwright proof."""
        out, report, expected = self.run_suite(
            "test('expected failure', async () => {test.fail(); expect(1).toBe(2)});")
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
        self.assertEqual(self.helper("validate", report, expected).returncode, 0)
        self.assertEqual(json.loads(report.read_text())["files"][0]["passed"], 1)
        self.helper("merge", self.cache, report)
        self.assertTrue(self.cache.exists())

    def test_unexpected_pass_is_not_successful_or_cacheable(self):
        """Passing a declared failure cannot count as an accepted observation."""
        out, report, expected = self.run_suite(
            "test('unexpected pass', async () => {test.fail(); expect(1).toBe(1)});")
        self.assertNotEqual(out.returncode, 0, out.stdout + out.stderr)
        self.assertNotEqual(self.helper("validate", report, expected).returncode, 0)
        self.assertEqual(json.loads(report.read_text())["files"][0]["passed"], 0)
        self.helper("merge", self.cache, report)
        self.assertFalse(self.cache.exists())

    def test_load_error_and_test_failure_remain_failures(self):
        """No empty/failed discovery may become a successful receipt."""
        for body in ["throw new Error('fixture load failure')", "test('fail', async () => {expect(1).toBe(2)});"]:
            out, report, expected = self.run_suite(body)
            self.assertNotEqual(out.returncode, 0, out.stdout + out.stderr)
            self.assertNotEqual(self.helper("validate", report, expected).returncode, 0)

    def test_skip_remains_visible_and_is_not_cacheable(self):
        """A deliberate skip preserves coverage accounting without fake timing."""
        out, report, expected = self.run_suite("test.skip('skip', async () => {});")
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
        self.assertEqual(self.helper("validate", report, expected).returncode, 0)
        self.helper("merge", self.cache, report)
        self.assertFalse(self.cache.exists())

    def test_interruption_is_recorded_without_a_successful_receipt(self):
        """Interrupt the actual runner from its own worker, not a reporter double."""
        out, report, expected = self.run_suite(
            "test('interrupt', async () => {process.kill(process.ppid, 'SIGINT'); await new Promise(() => {});});")
        self.assertNotEqual(out.returncode, 0, out.stdout + out.stderr)
        self.assertEqual(json.loads(report.read_text())["status"], "interrupted")
        self.assertNotEqual(self.helper("validate", report, expected).returncode, 0)
        self.helper("merge", self.cache, report)
        self.assertFalse(self.cache.exists())

    def test_reporter_override_is_detected_by_the_shell_validator(self):
        """Playwright's success alone cannot replace missing execution evidence."""
        out, report, expected = self.run_suite("test('pass', async () => {});", reporter_path="list")
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
        self.assertFalse(report.exists())
        self.assertNotEqual(self.helper("validate", report, expected).returncode, 0)

    def test_retry_cannot_train_history_or_launder_the_slice(self):
        """Even a caller-enabled retry retains its failed first attempt."""
        out, report, expected = self.run_suite(
            "test.describe.configure({retries: 1}); test('retry', async ({}, info) => {expect(info.retry).toBe(1)});")
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
        self.assertEqual(json.loads(report.read_text())["files"][0]["retries"], 1)
        self.assertNotEqual(self.helper("validate", report, expected).returncode, 0)
        self.helper("merge", self.cache, report)
        self.assertFalse(self.cache.exists())


if __name__ == "__main__":
    unittest.main()
