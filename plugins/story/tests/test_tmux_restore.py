"""Independent snapshot and receipt evidence precedes every restore adoption."""

import copy
import json
from pathlib import Path
import unittest

import test_tmux_target as fixtures
import tmux_target


class RestoreTests(unittest.TestCase):
    """Private files model the provider boundary; no identity mutation is mocked."""

    def setUp(self):
        self.f = fixtures.TargetTests()
        self.f.setUp()
        self.addCleanup(self.f.doCleanups)
        self.old = dict(self.f.record, generation='b' * 32,
                        endpoint=str(self.f.root / ('.rv-' + 'b' * 32) / 's'))
        self.f.record['history'] = [self.old['generation']]
        self.f.response['history'] = [self.old['generation']]
        self.f.publish()
        self.target = self.f.resolve()
        self.base = Path(self.f.record['state_dir'])
        self.snapshot_id = '20261003T010203.123456Z-autosave'
        self.snapshot = dict(schema=1, provenance=dict(socket=self.f.socket, generation=self.old['generation']),
            sessions=[dict(name='session', links=[dict(window_key='window')])],
            windows={'window': dict(key='window', name='SH-1', options={'@storyhook-agent': 'codex'},
                panes=[dict(uuid='unique-pane', cwd=str(self.f.root), options={'@storyhook-identity-v1': '{"old":"identity"}'}, agent=None)])})
        self.receipt = dict(state='done', snapshot_id=self.snapshot_id, pane_map={'unique-pane': '%7'},
                            session_ids={'session': '$2'}, restored=['session'], skipped=[], failed=[])
        self.source = self.base / 'generations' / self.old['generation'] / 'snapshots' / (self.snapshot_id + '.json')
        self.receipt_path = Path(self.target['state_dir']) / 'run/last-restore.json'
        self.write(self.base / 'owners' / (self.old['generation'] + '.json'), self.old)
        self.write(self.source, self.snapshot)
        self.write(self.receipt_path, self.receipt)

    def write(self, path, value):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(value))
        path.chmod(0o600)

    def evidence(self):
        return tmux_target.restore_evidence(self.target, self.f.env)

    def test_unique_uuid_carries_original_metadata_and_new_mapping(self):
        evidence = self.evidence()
        self.assertEqual(evidence['source_generation'], self.old['generation'])
        self.assertEqual(evidence['source_endpoint'], self.old['endpoint'])
        pane = evidence['panes']['unique-pane']
        self.assertEqual(pane['pane_id'], '%7')
        self.assertEqual(pane['pane']['options'], self.snapshot['windows']['window']['panes'][0]['options'])
        self.assertEqual(pane['window']['name'], 'SH-1')

    def test_skipped_without_mapping_grants_no_restoration_evidence(self):
        self.write(self.receipt_path, dict(self.receipt, state='skipped', pane_map={}, snapshot_id=None))
        self.assertIsNone(self.evidence())

    def test_failed_malformed_and_unsafe_receipts_refuse(self):
        for change in [dict(state='failed'), dict(snapshot_id='../escape'), dict(pane_map={'u':'%x'}),
                       dict(pane_map={'one':'%7', 'two':'%7'}), dict(failed=[['session','failed']])]:
            with self.subTest(change=change):
                self.write(self.receipt_path, dict(self.receipt, **change))
                with self.assertRaises(RuntimeError): self.evidence()

    def test_snapshot_requires_matching_provenance_and_complete_history(self):
        for provenance in [None, dict(socket='/foreign',generation=self.old['generation']),
                           dict(socket=self.f.socket,generation='c'*32)]:
            self.write(self.source, dict(self.snapshot, provenance=provenance))
            with self.assertRaises(RuntimeError): self.evidence()
        self.write(self.source, self.snapshot)
        (self.base / 'owners' / (self.old['generation'] + '.json')).unlink()
        with self.assertRaises(RuntimeError): self.evidence()

    def test_missing_snapshot_and_insecure_snapshot_refuse(self):
        self.source.chmod(0o644)
        with self.assertRaises(RuntimeError): self.evidence()
        self.source.unlink()
        with self.assertRaises(RuntimeError): self.evidence()

    def test_duplicate_snapshot_uuid_and_unknown_mapping_refuse(self):
        duplicate = copy.deepcopy(self.snapshot)
        duplicate['windows']['other'] = copy.deepcopy(duplicate['windows']['window'])
        duplicate['windows']['other']['key'] = 'other'
        self.write(self.source, duplicate)
        with self.assertRaises(RuntimeError): self.evidence()
        self.write(self.source, self.snapshot)
        self.write(self.receipt_path, dict(self.receipt, pane_map={'foreign-pane':'%7'}))
        with self.assertRaises(RuntimeError): self.evidence()

    def test_current_activation_cannot_change_behind_captured_target(self):
        self.f.record['identity'] = dict(self.f.record['identity'], start='reused-PID')
        self.f.publish()
        with self.assertRaisesRegex(RuntimeError, 'changed|captured'): self.evidence()

    def test_explicit_legacy_selection_cannot_read_a_different_generation_snapshot(self):
        self.f.record['legacy_snapshot'] = '20261003T010203.123456Z-different'
        self.f.publish()
        with self.assertRaises(RuntimeError): self.evidence()

    def test_ambiguous_or_incomplete_session_maps_refuse(self):
        for change in [dict(session_ids={'session':'$2', 'other':'$2'}),
                       dict(restored=['session', 'session']), dict(restored=[3]),
                       dict(session_ids={'other':'$2'}), dict(restored=['session', 'unknown'])]:
            with self.subTest(change=change):
                self.write(self.receipt_path, dict(self.receipt, **change))
                with self.assertRaises(RuntimeError): self.evidence()
        self.write(self.receipt_path, self.receipt)
        self.write(self.source, dict(self.snapshot, sessions=self.snapshot['sessions'] * 2))
        with self.assertRaises(RuntimeError): self.evidence()

    def test_legacy_snapshot_requires_explicit_current_selection(self):
        self.source.unlink()
        legacy = dict(self.snapshot, provenance=None)
        self.write(self.base / 'snapshots' / (self.snapshot_id + '.json'), legacy)
        with self.assertRaises(RuntimeError): self.evidence()
        self.f.record['legacy_snapshot'] = self.snapshot_id
        self.f.publish()
        evidence = self.evidence()
        self.assertIsNone(evidence['source_generation'])
        self.assertEqual(evidence['snapshot_id'], self.snapshot_id)


if __name__ == '__main__':
    unittest.main()
