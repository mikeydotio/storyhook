"""Resource delivery is replayable and cannot silently discard journal failures."""

import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from host_admission.evidence import Publisher
from host_admission.policy import Refusal

BINDING = dict(attempt_id="attempt", execution_id="gate", generation=7)


class Source:
    """Only retained event data is mocked; delivery uses the production publisher."""

    def __init__(self):
        self.events = [dict(authority="broker", sequence=1, event="pressure", lease=None),
                       dict(authority="broker", sequence=2, event="grant", lease="lease", binding=BINDING),
                       dict(authority="broker", sequence=3, event="grant", lease="foreign",
                            binding=dict(BINDING, generation=8))]

    def call(self, operation, **arguments):
        assert operation == "events"
        return [e for e in self.events if e["sequence"] > arguments["after"]]


class EvidenceTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(dir="/tmp", prefix="ha-evidence-")
        self.addCleanup(self.tmp.cleanup)
        self.journal = Path(self.tmp.name) / "gate.ndjson"
        self.journal.touch(mode=0o600)
        self.source = Source()

    def test_replay_binds_each_record_and_never_includes_foreign_leases(self):
        publisher = Publisher(self.source, BINDING, self.journal)
        publisher.publish(); publisher.publish()
        records = [json.loads(line) for line in self.journal.read_text().splitlines()]
        self.assertEqual([r["observation"]["sequence"] for r in records], [1, 2])
        self.assertTrue(all({k: r[k] for k in BINDING} == BINDING for r in records))
        self.assertEqual(publisher.cursor, 3)

    def test_failed_delivery_keeps_cursor_and_source_for_retry(self):
        publisher = Publisher(self.source, BINDING, self.journal)
        self.journal.unlink()
        with self.assertRaisesRegex(Refusal, "journal"):
            publisher.publish()
        self.assertEqual(publisher.cursor, 0)
        self.journal.touch(mode=0o600)
        publisher.publish()
        self.assertEqual(len(self.journal.read_text().splitlines()), 2)

    def test_symlink_is_not_a_journal_and_partial_binding_is_refused(self):
        self.journal.unlink(); self.journal.symlink_to(Path(self.tmp.name) / "elsewhere")
        with self.assertRaises(Refusal):
            Publisher(self.source, BINDING, self.journal).publish()
        with self.assertRaises(Refusal):
            Publisher(self.source, dict(attempt_id="a"), self.journal)


if __name__ == "__main__":
    unittest.main()
