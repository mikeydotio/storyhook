"""SH-872 measurement-only C/W/R ordering, invalidation and failure controls."""
import copy
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from gate_measurement_cohorts import Cohort, fingerprint
from verifier_state import Refusal
from gate_measurement_optional import complete


def identity():
    return {'source_commit': 'a' * 40, 'source_tree': 'b' * 40,
            'toolchain': {'rustc': 'observed-hash'}, 'environment_digest': 'c' * 64,
            'configuration_digest': 'd' * 64, 'worker_limits': {'cargo': 1},
            'gate_argv': ['make', 'test'], 'target_identity': {'device': 1, 'inode': 2},
            'applicable_legs': ['fmt', 'clippy', 'rust-suite', 'rust-contracts', 'build', 'plugin']}


class Cohorts(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='sh872-cohorts-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.cohort = Cohort(self.root, 'baseline')
        self.identity = identity()

    def start(self, value=None, **kwargs):
        return self.cohort.begin(self.identity if value is None else value,
                                 remaining_window=kwargs.get('window', 36000),
                                 remaining_campaign=kwargs.get('campaign', 72000))

    def finish(self, **kwargs):
        return self.cohort.finish(telemetry=complete(), exit_code=kwargs.get('exit', 0), settled=kwargs.get('settled', True),
                                  executed=kwargs.get('executed', self.identity['applicable_legs']),
                                  reused=kwargs.get('reused', []), elapsed=kwargs.get('elapsed', 60))

    def test_cold_warm_execute_and_reuse_only_earned_warm_results(self):
        for block in range(3):
            self.identity['target_identity']['inode'] = block + 2
            self.assertEqual(self.start()['mode'], 'cold')
            self.assertTrue(self.finish()['accepted'])
            self.assertEqual(self.start()['mode'], 'warm')
            self.assertTrue(self.finish()['accepted'])
            self.assertEqual(self.start()['mode'], 'reuse')
            self.assertTrue(self.cohort.reuse_key(self.identity, 'fmt').endswith(':fmt'))
            result = self.finish(executed=[], reused=self.identity['applicable_legs'])
            self.assertTrue(result['accepted'])
            self.assertFalse(result['production_certification'])
        with self.assertRaises(Refusal): self.start()
        self.assertFalse((self.root / 'gate-leg-receipts').exists())

    def test_failed_warm_cannot_seed_reuse_or_retry(self):
        self.start(); self.finish()
        self.start(); self.finish(exit=7)
        with self.assertRaises(Refusal): self.start()
        with self.assertRaises(Refusal): self.cohort.reuse_key(self.identity, 'fmt')

    def test_unsettled_start_is_retained_and_blocks_new_work(self):
        self.start()
        with self.assertRaises(Refusal): self.start()
        self.assertFalse(self.finish(settled=False)['accepted'])
        with self.assertRaises(Refusal): self.start()

    def test_every_input_component_invalidates_reuse(self):
        self.start(); self.finish(); self.start(); self.finish(); self.start()
        mutations = {'source_commit': 'e' * 40, 'source_tree': 'e' * 40,
                     'toolchain': {'rustc': 'changed'}, 'environment_digest': 'e' * 64,
                     'configuration_digest': 'e' * 64, 'worker_limits': {'cargo': 2},
                     'gate_argv': ['make', 'test-full'], 'target_identity': {'device': 1, 'inode': 3},
                     'applicable_legs': ['fmt']}
        for field, value in mutations.items():
            changed = copy.deepcopy(self.identity); changed[field] = value
            with self.subTest(field=field), self.assertRaises(Refusal):
                self.cohort.reuse_key(changed, 'fmt')

    def test_missing_identity_and_unknown_legs_refuse(self):
        for field in self.identity:
            changed = copy.deepcopy(self.identity); changed.pop(field)
            with self.subTest(field=field), self.assertRaises(Refusal): fingerprint(changed)
        self.start(); self.finish(); self.start(); self.finish(); self.start()
        with self.assertRaises(Refusal): self.cohort.reuse_key(self.identity, 'not-covered')

    def test_insufficient_windows_and_false_execution_refuse(self):
        for value in [0, 4499, 36001]:
            with self.subTest(value=value), self.assertRaises(Refusal): self.start(window=value)
        self.start()
        result = self.finish(executed=[], reused=self.identity['applicable_legs'])
        self.assertFalse(result['accepted'])
        with self.assertRaises(Refusal): self.start()

    def test_production_breach_is_retained_inside_larger_experiment_ceiling(self):
        self.start()
        result = self.finish(elapsed=900)
        self.assertTrue(result['accepted'])
        self.assertTrue(result['production_target_breach'])
        self.assertFalse(result['production_certification'])

    def test_corrupt_journal_and_symlink_namespace_refuse(self):
        self.cohort.path.write_text('{"kind":"start"')
        with self.assertRaises(Refusal): self.start()
        alias = self.root / 'alias'; alias.symlink_to(self.root, target_is_directory=True)
        with self.assertRaises(Refusal): Cohort(alias, 'baseline')
        with self.assertRaises(Refusal): Cohort(self.root, 'second-optimization')

    def test_replayed_start_cannot_expand_ceiling_or_change_slot_shape(self):
        row = self.start()
        for field, value in [('ceiling_seconds', 100000), ('block', 1), ('slot', False),
                             ('identity', None), ('ceiling_seconds', '4500')]:
            changed = copy.deepcopy(row); changed[field] = value
            self.cohort.path.write_text(json.dumps(changed) + '\n')
            with self.subTest(field=field, value=value), self.assertRaises(Refusal):
                self.finish(elapsed=1000)

    def test_replayed_journal_cannot_continue_after_failed_slot(self):
        start = self.start(); self.finish(exit=1)
        next_start = copy.deepcopy(start)
        next_start.update(slot=1, mode='warm')
        with self.cohort.path.open('a') as stream:
            stream.write(json.dumps(next_start) + '\n')
        with self.assertRaises(Refusal): self.cohort.history()

    def test_malformed_inputs_and_nonfinite_windows_refuse(self):
        for value in [None, [], 'identity']:
            with self.subTest(value=value), self.assertRaises(Refusal): fingerprint(value)
        for field, value in [('gate_argv', [1]), ('gate_argv', ['make', '\x00']),
                             ('applicable_legs', [{}]), ('toolchain', {'rustc': float('nan')})]:
            changed = copy.deepcopy(self.identity); changed[field] = value
            with self.subTest(field=field), self.assertRaises(Refusal): fingerprint(changed)
        for value in [float('nan'), float('inf'), True, '4500']:
            with self.subTest(value=value), self.assertRaises(Refusal): self.start(window=value)


if __name__ == '__main__': unittest.main()
