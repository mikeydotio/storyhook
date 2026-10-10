"""External transport prerequisite selection and archive identity regressions."""
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock
sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from gate_measurement_campaign import campaign_environment
from gate_measurement_fixture import revivify_fixture
from verifier_state import Refusal


class FixtureInputs(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='measurement-fixture-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.source = self.root / 'plugins/story/tests/test_revivify_transport.py'
        self.source.parent.mkdir(parents=True)
        self.source.write_text('REVISION = ' + repr('a' * 40) + '\n')
        self.env = {'STORY_TEST_REVIVIFY_REPO': str(self.root)}
    def test_campaign_preserves_explicit_test_prerequisite(self):
        self.assertEqual(campaign_environment(self.env)['STORY_TEST_REVIVIFY_REPO'], str(self.root))
    def test_archive_content_and_revision_are_pinned(self):
        query = mock.Mock(side_effect=['b' * 40, 'c' * 64])
        result = revivify_fixture(self.root, self.env, query)
        self.assertEqual(result, {'repository': str(self.root), 'revision': 'a' * 40,
                                 'tree': 'b' * 40, 'archive_sha256': 'c' * 64})
        command = query.call_args.args[0]
        self.assertIn('pipefail', command[2])
        self.assertEqual(command[-3:-1], [str(self.root), 'a' * 40])
        changed = revivify_fixture(self.root, self.env, mock.Mock(side_effect=['b' * 40, 'd' * 64]))
        self.assertNotEqual(result, changed)
    def test_missing_selector_or_invalid_source_refuses_before_query(self):
        query = mock.Mock()
        with self.assertRaises(Refusal): revivify_fixture(self.root, {}, query)
        self.source.write_text('REVISION = "HEAD"\n')
        with self.assertRaises(Refusal): revivify_fixture(self.root, self.env, query)
        query.assert_not_called()
    def test_missing_object_and_invalid_digest_refuse(self):
        with self.assertRaises(Refusal):
            revivify_fixture(self.root, self.env, mock.Mock(side_effect=Refusal('missing object')))
        with self.assertRaises(Refusal):
            revivify_fixture(self.root, self.env, mock.Mock(side_effect=['b' * 40, 'not a digest']))

if __name__ == '__main__': unittest.main()
