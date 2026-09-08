"""Synthetic event histories pin the audit's evidence and arithmetic rules."""

import importlib.util
import json
from pathlib import Path
import sqlite3
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("audit", ROOT / "scripts/lifecycle_audit.py")
AUDIT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(AUDIT)
sys.path.insert(0, str(ROOT / "scripts"))
CLI_SPEC = importlib.util.spec_from_file_location("audit_cli", ROOT / "scripts/lifecycle-audit.py")
CLI = importlib.util.module_from_spec(CLI_SPEC)
CLI_SPEC.loader.exec_module(CLI)
START = "2026-09-06T00:00:00Z"
END = "2026-09-08T00:00:00Z"


def event(seq, kind, at, story=1, **payload):
    """Build a store-shaped event, retaining its independent coordinates."""
    return dict(story_no=story, seq=seq, global_seq=story * 100 + seq,
                at=at, kind=kind, payload=dict(kind=kind, at=at, **payload))


class LifecycleTests(unittest.TestCase):
    """Test observations the report must never conflate."""

    def history(self):
        """Return a four-hour task with one verification hour."""
        return [event(1, "StoryCreated", START, state="todo", title="Example"),
                event(2, "StoryStateChanged", "2026-09-06T02:00:00Z", state="in-progress"),
                event(3, "StoryStateChanged", "2026-09-06T03:00:00Z", state="verifying"),
                event(4, "StoryStateChanged", "2026-09-06T04:00:00Z", state="done")]

    def test_state_residence_is_not_gate_runtime(self):
        story = AUDIT.analyze(self.history(), START, END)[0]
        self.assertEqual(story["state_seconds"], {"todo": 7200, "in-progress": 3600, "verifying": 3600})
        self.assertEqual(story["cycle_seconds"], 14400)
        self.assertIsNone(story["gate_runtime_seconds"])

    def test_archive_is_not_a_second_done_entry(self):
        events = self.history() + [event(5, "StoryClosedAndArchived", "2026-09-06T04:01:00Z", state="done")]
        self.assertEqual(AUDIT.analyze(events, START, END)[0]["done_entries"], 1)

    def test_reopen_and_same_state_verifying_are_distinct(self):
        events = self.history() + [
            event(5, "StoryStateChanged", "2026-09-06T05:00:00Z", state="todo"),
            event(6, "StoryStateChanged", "2026-09-06T06:00:00Z", state="verifying"),
            event(7, "StoryStateChanged", "2026-09-06T07:00:00Z", state="verifying"),
            event(8, "StoryStateChanged", "2026-09-06T08:00:00Z", state="done")]
        story = AUDIT.analyze(events, START, END)[0]
        self.assertEqual(story["done_entries"], 2)
        self.assertEqual(story["reopenings"], 1)
        self.assertEqual([g["generation"] for g in story["generations"]], [103, 106, 107])
        self.assertEqual(story["state_seconds"]["verifying"], 10800)

    def test_cutoff_is_inclusive_and_initial_done_counts(self):
        events = [event(1, "StoryCreated", at, story=n, title=str(n), state="done")
                  for n, at in enumerate(["2026-09-05T23:59:59Z", START, END,
                                          "2026-09-08T00:00:01Z"], 1)]
        self.assertEqual([s["id"] for s in AUDIT.analyze(events, START, END)], ["SH-2", "SH-3"])

    def test_late_changes_do_not_change_historical_report(self):
        events = self.history() + [event(5, "StoryTitleSet", "2026-09-08T00:00:01Z", title="Later")]
        self.assertEqual(AUDIT.analyze(events, START, END)[0]["title"], "Example")

    def test_missing_and_backwards_time_stay_unknown(self):
        for at in [None, "not a timestamp", "2026-09-05T23:00:00Z"]:
            with self.subTest(at=at):
                events = self.history()
                events[2]["at"] = at
                events[2]["payload"]["at"] = at
                story = AUDIT.analyze(events, START, END)[0]
                self.assertIsNone(story["state_seconds"]["in-progress"])
                self.assertTrue(story["warnings"])

    def test_retractions_match_text_and_time_and_preserve_evidence(self):
        events = self.history() + [event(5, "StoryCommentAdded", END, text="A"),
                                  event(6, "StoryCommentAdded", END, text="B"),
                                  event(7, "StoryCommentRetracted", END, comment_at=END, text="A")]
        story = AUDIT.analyze(events, START, END)[0]
        self.assertEqual([(c["text"], c["retracted"]) for c in story["comments"]], [("A", True), ("B", False)])
        self.assertEqual(len(story["events"]), 7)

    def test_input_order_does_not_change_output(self):
        self.assertEqual(AUDIT.analyze(self.history(), START, END),
                         AUDIT.analyze(list(reversed(self.history())), START, END))

    def test_duplicate_sequence_fails_loud(self):
        with self.assertRaisesRegex(ValueError, "duplicate"):
            AUDIT.analyze(self.history() + [self.history()[0]], START, END)

    def test_embedded_source_cannot_close_script_element(self):
        value = {"text": '</script><img src=x onerror="alert(1)">&\u2028'}
        encoded = AUDIT.embedded_json(value)
        self.assertNotIn("<", encoded)
        self.assertNotIn("&", encoded)
        self.assertEqual(json.loads(encoded), value)

    def test_archival_serializers_preserve_quoted_paths_without_literal_citations(self):
        marker = ".council/"
        value = {"text": marker + 'old-verdict/ "quote" \\ / </script> & café',
                 "url": "https://example.com/source", "escaped": r"\/\u002f"}
        for serialize in [AUDIT.archived_json, AUDIT.embedded_json]:
            with self.subTest(serializer=serialize.__name__):
                encoded = serialize(value)
                self.assertNotIn(marker, encoded)
                self.assertEqual(json.loads(encoded), value)

    def test_collection_preserves_provenance_and_excludes_other_projects_and_future(self):
        with tempfile.TemporaryDirectory(prefix="SH-560-", dir="/tmp") as directory:
            root = Path(directory)
            pointer = root / "project.toml"
            pointer.write_text('uuid = "chosen"\n')
            db = root / "source.db"
            conn = sqlite3.connect(db)
            conn.executescript("CREATE TABLE projects(id INTEGER, uuid TEXT);"
                               "INSERT INTO projects VALUES(9,'chosen'),(10,'unrelated');"
                               "CREATE TABLE events(project_id,story_no,seq,global_seq,at,kind,payload,command,actor);")
            for project, number, seq, at in [(9, 1, 1, CLI.START), (10, 2, 2, CLI.START),
                                             (9, 3, CLI.WATERMARK + 1, CLI.START),
                                             (9, 4, 4, "2026-09-09T00:00:00Z")]:
                payload = dict(kind="StoryCreated", at=at, title="Example", state="done")
                conn.execute("INSERT INTO events VALUES(?,?,?,?,?,?,?,?,?)",
                             (project,number,1,seq,at,"StoryCreated",json.dumps(payload),"new","declared"))
            conn.commit()
            conn.close()
            before = db.read_bytes()
            result = CLI.collect(db, pointer)
            self.assertEqual(result["cohort_ids"], ["SH-1"])
            self.assertEqual(len(result["events"]), 1)
            self.assertEqual(result["events"][0]["command"], "new")
            self.assertEqual(result["events"][0]["actor"], "declared")
            self.assertEqual(before, db.read_bytes())

    def test_missing_store_is_not_created(self):
        with tempfile.TemporaryDirectory(prefix="SH-560-", dir="/tmp") as directory:
            root = Path(directory)
            pointer = root / "project.toml"
            pointer.write_text('uuid = "missing"\n')
            with self.assertRaises(sqlite3.OperationalError):
                CLI.collect(root / "absent.db", pointer)
            self.assertFalse((root / "absent.db").exists())

    def test_render_rejects_broken_evidence_links(self):
        evidence = dict(events=self.history(), window_start=START, window_end=END, cohort_ids=["SH-1"])
        with self.assertRaisesRegex(ValueError, "missing evidence"):
            CLI.render(evidence, {"findings": [dict(id="F1", evidence=[999])]})

    def test_render_is_deterministic_and_preserves_all_source_text(self):
        evidence = dict(events=self.history(), window_start=START, window_end=END, cohort_ids=["SH-1"])
        result = CLI.render(evidence, {"findings": []})
        self.assertEqual(result, CLI.render(evidence, {"findings": []}))
        self.assertNotIn("__AUDIT_DATA__", result)
        self.assertIn('"global_seq":103', result)

    def test_committed_report_matches_frozen_evidence_and_assessment(self):
        reports = ROOT / "docs/reports"
        evidence = json.loads((reports / "SH-560-evidence.json").read_text())
        self.assertEqual((reports / "SH-560-evidence.json").read_text(),
                         AUDIT.archived_json(evidence), "regenerate the archival representation")
        findings = json.loads((reports / "SH-560-findings.json").read_text())
        actual = (reports / "SH-560-lifecycle-audit.html").read_text()
        self.assertEqual(actual, CLI.render(evidence, findings), "regenerate the frozen report")
        stories = AUDIT.analyze(evidence["events"], evidence["window_start"], evidence["window_end"])
        # These independent SQL-confirmed totals are the approved historical
        # cohort, not predictions made from the generator's own output.
        self.assertEqual(len(stories), 36)
        self.assertEqual(sum(s["done_entries"] for s in stories), 40)
        self.assertEqual(sum(len(s["generations"]) for s in stories), 75)


if __name__ == "__main__":
    unittest.main()
