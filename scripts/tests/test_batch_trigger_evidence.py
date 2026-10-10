#!/usr/bin/env python3
"""Offline SH-841 regressions; no daemon, Git, provider, or gate subprocess."""
import copy
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "batch_trigger_evidence.py"
spec = importlib.util.spec_from_file_location("batch_trigger_evidence", SCRIPT)
audit = importlib.util.module_from_spec(spec)
spec.loader.exec_module(audit)


def record(number, members=None, verdict="certified", at=0, cap=2):
    head = f"SH-{number}"
    members = members or [(head, f"{number:040x}")]
    stamp = f"2026-10-01T00:{at // 60:02d}:{at % 60:02d}Z"
    return {
        "attempt_id": f"attempt-{number}-{at}", "story_id": head,
        "generation": number, "finished_at": stamp, "verdict": verdict,
        "gate_tree": "a" * 40,
        "preview": {"computed_at": stamp, "head": head, "cap": cap,
                    "outcome": "batch", "head_tree": "a" * 40,
                    "members": [{"story_id": s, "commit": c} for s, c in members]},
    }


def pair(number=1, at=0):
    return [record(number, [(f"SH-{number}", f"{number:040x}"),
                            (f"SH-{number+1}", f"{number+1:040x}")], at=at),
            record(number+1, at=at+1)]


class BatchTriggerEvidenceTests(unittest.TestCase):
    def test_exact_commit_pair_is_green(self):
        result = audit.analyze(pair())
        self.assertEqual(result["pairs"]["green"], 1)
        self.assertFalse(result["pairs"]["observed_threshold_met"])
        self.assertFalse(result["activation_authorized"])

    def test_thirty_pairs_and_half_green_is_the_boundary(self):
        rows = []
        for i in range(30):
            records = pair(i * 2 + 1, i * 3)
            if i >= 15:
                records[1]["verdict"] = "tests-failed"
            rows.extend(records)
        result = audit.analyze(rows)
        self.assertEqual(result["pairs"]["green"], 15)
        self.assertTrue(result["pairs"]["observed_threshold_met"])
        self.assertFalse(result["activation_authorized"])

    def test_changed_commit_does_not_supply_a_verdict(self):
        rows = pair()
        rows[1]["preview"]["members"][0]["commit"] = "b" * 40
        self.assertEqual(audit.analyze(rows)["pairs"]["unknown"], 1)

    def test_wrong_gate_tree_is_unknown(self):
        rows = pair()
        rows[1]["gate_tree"] = "b" * 40
        self.assertEqual(audit.analyze(rows)["pairs"]["unknown"], 1)

    def test_red_is_not_replaced_by_a_later_green_retry(self):
        rows = pair()
        rows[1]["verdict"] = "tests-failed"
        rows.append(record(2, at=2))
        self.assertEqual(audit.analyze(rows)["pairs"]["red"], 1)

    def test_infrastructure_failure_is_not_replaced_by_green(self):
        rows = pair()
        rows[1]["verdict"] = "infrastructure-failure"
        rows.append(record(2, at=2))
        self.assertEqual(audit.analyze(rows)["pairs"]["unknown"], 1)

    def test_earlier_commit_verdict_is_not_reused(self):
        rows = [record(2, at=0), pair(at=1)[0]]
        self.assertEqual(audit.analyze(rows)["pairs"]["unknown"], 1)

    def test_cap_one_and_single_members_are_not_pairs(self):
        rows = pair()
        rows[0]["preview"]["cap"] = 1
        self.assertEqual(audit.analyze(rows)["pairs"]["eligible_dequeues"], 0)

    def test_only_first_two_members_are_the_cap_two_pair(self):
        rows = pair()
        rows[0]["preview"]["cap"] = 4
        rows[0]["preview"]["members"].append({"story_id": "SH-3", "commit": "c"*40})
        self.assertEqual(audit.analyze(rows)["pairs"]["green"], 1)

    def test_smoothed_members_cannot_justify_unsmoothed_activation(self):
        rows = pair()
        rows[0]["preview"]["members"][1]["smoothed"] = ["docs/spec/a.md"]
        self.assertEqual(audit.analyze(rows)["pairs"]["eligible_dequeues"], 0)

    def test_duplicate_attempts_do_not_inflate_sample(self):
        rows = pair()
        self.assertEqual(audit.analyze(rows + copy.deepcopy(rows))["records"], 2)

    def test_conflicting_duplicate_attempt_fails_closed(self):
        rows = pair()
        conflict = copy.deepcopy(rows[0])
        conflict["verdict"] = "tests-failed"
        with self.assertRaises(ValueError):
            audit.analyze(rows + [conflict])

    def test_same_time_distinct_attempts_are_ambiguous(self):
        rows = pair()
        other = copy.deepcopy(rows[1])
        other["attempt_id"] = "another"
        rows.append(other)
        self.assertEqual(audit.analyze(rows)["pairs"]["unknown"], 1)

    def test_missing_identity_and_bad_timestamps_fail_closed(self):
        for field in ("attempt_id", "finished_at"):
            rows = pair()
            del rows[0][field]
            with self.assertRaises(ValueError):
                audit.analyze(rows)
        rows = pair()
        rows[0]["preview"]["computed_at"] = "yesterday"
        with self.assertRaises(ValueError):
            audit.analyze(rows)

    def test_unknowns_are_not_dropped_from_denominator(self):
        rows = []
        for i in range(30):
            records = pair(i * 2 + 1, i * 3)
            if i == 29:
                records.pop()
            rows.extend(records)
        result = audit.analyze(rows)["pairs"]
        self.assertEqual(result["eligible_dequeues"], 30)
        self.assertEqual(result["unknown"], 1)
        self.assertFalse(result["observed_threshold_met"])

    def test_submission_rate_is_not_invented_from_first_retained_record(self):
        result = audit.analyze(pair())["submissions"]
        self.assertFalse(result["observed_threshold_met"])
        self.assertEqual(result["status"], "submission-provenance-unavailable")

    def test_identity_less_attempt_cannot_be_skipped(self):
        rows = pair()
        rows[1]["preview"]["members"] = []
        rows[1]["preview"]["outcome"] = "unavailable"
        rows.append(record(2, at=2))
        self.assertEqual(audit.analyze(rows)["pairs"]["unknown"], 1)

    def test_no_records_cannot_qualify(self):
        result = audit.analyze([])
        self.assertIsNone(result["pairs"]["green_fraction"])
        self.assertFalse(result["pairs"]["observed_threshold_met"])

    def test_red_pair_with_unknown_partner_keeps_incomplete_evidence(self):
        rows = pair()[:1]
        rows[0]["verdict"] = "tests-failed"
        result = audit.analyze(rows)["pairs"]
        self.assertEqual(result["red"], 1)
        self.assertEqual(result["pairs_with_unresolved_members"], 1)

    def test_order_does_not_change_result(self):
        rows = pair()
        self.assertEqual(audit.analyze(rows), audit.analyze(list(reversed(rows))))

    def test_cli_reads_only_explicit_input_and_reports_hash(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "input.ndjson"
            content = "\n".join(json.dumps(r) for r in pair()) + "\n"
            path.write_text(content)
            result = subprocess.run([sys.executable, "-B", str(SCRIPT), str(path)],
                                    capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            payload = json.loads(result.stdout)
            self.assertEqual(payload["sources"][0]["bytes"], len(content.encode()))
            self.assertEqual(len(payload["sources"][0]["sha256"]), 64)
            self.assertEqual(path.read_text(), content)

    def test_cli_refuses_malformed_ndjson_without_partial_report(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "input.ndjson"
            path.write_text(json.dumps(pair()[0]) + "\n{bad\n")
            result = subprocess.run([sys.executable, "-B", str(SCRIPT), str(path)],
                                    capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 2)
            self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
