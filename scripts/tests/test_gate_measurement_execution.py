"""SH-872 execution bridge, complete coverage, and isolated W-to-R evidence."""
import copy
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from gate_measurement_cohorts import Cohort
from gate_measurement_execution import coverage, run_observation, OwnedGate
from gate_measurement_legs import command_key, current_slot, prepare, finish
from gate_measurement_runtime import records
from verifier_state import Refusal
from gate_measurement_optional import complete
from test_gate_measurement_cohorts import identity


def progress(legs, mode='cold'):
    return [dict(kind='item', path='release gate/' + leg, status=status)
            for leg in legs for status in (['reused'] if mode == 'reuse' else ['running', 'passed'])]


class Execution(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='sh872-execution-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.cohort = Cohort(self.root, 'baseline')
        self.identity = identity()
        self.observe = mock.Mock(side_effect=lambda: copy.deepcopy(self.identity))

    def run_slot(self, launch=None, observe=None):
        return run_observation(self.cohort, observe=observe or self.observe,
                               launch=launch or self.launch, remaining_window=36000,
                               remaining_campaign=72000, clock=mock.Mock(side_effect=[10, 10, 10, 10, 910, 910, 910]))

    def launch(self, slot, directory, deadline):
        self.assertEqual(deadline, 10 + slot['ceiling_seconds'])
        self.assertTrue(directory.is_dir())
        self.assertEqual(self.cohort.history()[1]['slot'], slot['slot'])
        return {'exit_code': 0, 'settled': True, 'telemetry': complete(),
                'progress': progress(self.identity['applicable_legs'], slot['mode'])}

    def test_input_capture_and_terminal_recheck_share_the_original_slot_deadline(self):
        import os
        now = [10]
        ends = []
        def observer():
            ends.append(float(os.environ['STORYHOOK_MEASUREMENT_END']))
            now[0] += 1
            return self.identity
        def launch(slot, directory, end):
            self.assertEqual(end, 4510)
            now[0] = 4509
            return self.launch(slot, directory, end)
        with mock.patch.dict(os.environ):
            os.environ.pop('STORYHOOK_MEASUREMENT_END', None)
            with self.assertRaisesRegex(Refusal, 'original allowance'):
                run_observation(self.cohort, observe=observer, launch=launch,
                    remaining_window=36000, remaining_campaign=72000, clock=lambda:now[0])
        self.assertEqual(ends, [4510, 4510])
        self.assertIsNotNone(self.cohort.history()[1])

    def test_actual_progress_and_settlement_complete_slot_with_target_breach(self):
        result = self.run_slot()
        self.assertTrue(result['accepted'])
        self.assertTrue(result['production_target_breach'])
        self.assertFalse(result['production_certification'])
        self.assertEqual(self.observe.call_count, 2)

    def test_interruption_retains_pending_start_and_prevents_replacement(self):
        launch = mock.Mock(side_effect=InterruptedError('fixture interruption'))
        with self.assertRaises(InterruptedError): self.run_slot(launch)
        self.assertEqual(self.cohort.history()[1]['slot'], 0)
        self.assertEqual(records(self.cohort.root / 'failures.jsonl')[0]['error_type'], 'InterruptedError')
        with self.assertRaises(Refusal): self.run_slot(launch)
        self.assertEqual(launch.call_count, 1)

    def test_source_change_during_execution_never_finishes_successfully(self):
        changed = copy.deepcopy(self.identity); changed['source_commit'] = 'e' * 40
        with self.assertRaises(Refusal):
            self.run_slot(observe=mock.Mock(side_effect=[self.identity, changed]))
        self.assertIsNotNone(self.cohort.history()[1])

    def test_nonzero_or_unsettled_result_is_retained_and_blocks_next_slot(self):
        def launch(*args):
            result = self.launch(*args); result.update(exit_code=9, settled=False)
            return result
        result = self.run_slot(launch)
        self.assertEqual(result['exit_code'], 9)
        self.assertFalse(result['settled'])
        self.assertFalse(result['accepted'])
        with self.assertRaises(Refusal): self.run_slot()

    def test_incomplete_progress_does_not_invent_terminal_exit(self):
        with self.assertRaises(Refusal):
            self.run_slot(lambda *_: {'exit_code': 0, 'settled': True, 'progress': []})
        self.assertIsNotNone(self.cohort.history()[1])

    def test_native_entry_refuses_class_manifest_before_any_process(self):
        health = mock.Mock()
        gate = OwnedGate(str(self.root / 'manifest.json'), ['make', 'test'], health=health)
        with mock.patch('gate_measurement_execution.validate', return_value={'kind': 'gate-class-measurement'}), \
             mock.patch('gate_measurement_execution.subprocess.Popen') as spawn:
            with self.assertRaises(Refusal): gate({}, self.root, 100)
        spawn.assert_not_called(); health.assert_not_called()

    def test_complete_coverage_refuses_skips_failures_duplicates_and_unplanned_legs(self):
        good = progress(['fmt'])
        cases = [[], good + good, good[:-1],
                 [dict(kind='item', path='release gate/fmt', status='passed')],
                 [dict(kind='item', path='release gate/fmt', status='reused')],
                 good + progress(['unknown']),
                 [dict(kind='item', path='release gate/fmt', status='skipped')]]
        for rows in cases:
            with self.subTest(rows=rows), self.assertRaises(Refusal): coverage(rows, ['fmt'], 'cold')
        self.assertEqual(coverage(good, ['fmt'], 'warm'), (['fmt'], []))
        self.assertEqual(coverage(progress(['fmt'], 'reuse'), ['fmt'], 'reuse'), ([], ['fmt']))
        with self.assertRaises(Refusal): coverage(good, ['fmt'], 'reuse')


class Legs(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='sh872-legs-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.cohort = Cohort(self.root, 'baseline')
        self.identity = identity(); self.identity['applicable_legs'] = ['fmt']
        self.env = {'PATH': '/observed/tools', 'CARGO_BUILD_JOBS': '1'}
        self.argv = ['sh', 'scripts/fmt.sh']

    def begin(self):
        slot = self.cohort.begin(self.identity, remaining_window=36000, remaining_campaign=72000)
        directory = self.cohort.root / f"slot-{slot['slot']:02}"; directory.mkdir()
        return slot, directory

    def complete(self, slot):
        return self.cohort.finish(telemetry=complete(), exit_code=0, settled=True, executed=[] if slot['mode']=='reuse' else ['fmt'],
                                  reused=['fmt'] if slot['mode']=='reuse' else [], elapsed=10)

    def warm(self):
        for _ in range(2):
            slot, directory = self.begin()
            self.assertEqual(prepare(self.cohort, slot, directory, 'fmt', self.argv, self.env), 'run')
            finish(slot, directory, 'fmt', self.argv, self.env, 0)
            self.complete(slot)
        return self.begin()

    def test_cold_and_warm_force_execution_then_reuse_earned_command(self):
        ordinary = self.root / 'gate-leg-receipts'; ordinary.mkdir()
        sentinel = ordinary / 'keep'; sentinel.write_text('unrelated receipt')
        slot, directory = self.warm()
        self.assertEqual(prepare(self.cohort, slot, directory, 'fmt', self.argv, self.env), 'reused')
        self.assertEqual(sentinel.read_text(), 'unrelated receipt')
        self.assertEqual(len(list(ordinary.iterdir())), 1)

    def test_changed_command_or_unknown_environment_cannot_reuse(self):
        slot, directory = self.warm()
        for argv, env in [(self.argv + ['--different'], self.env),
                          (self.argv, dict(self.env, UNRECOGNIZED_INPUT='changed'))]:
            with self.subTest(argv=argv, env=env), self.assertRaises(Refusal):
                prepare(self.cohort, slot, directory, 'fmt', argv, env)
        self.assertFalse((directory / 'legs.jsonl').exists())

    def test_only_attempt_telemetry_is_excluded_from_command_identity(self):
        slot, _ = self.begin()
        before = command_key(slot, 'fmt', self.argv, dict(self.env, STORYHOOK_MEASUREMENT_SLOT='/a'))
        after = command_key(slot, 'fmt', self.argv, dict(self.env, STORYHOOK_MEASUREMENT_SLOT='/b'))
        self.assertEqual(before, after)
        self.assertNotEqual(before, command_key(slot, 'fmt', self.argv, dict(self.env, RUSTFLAGS='-C opt-level=1')))

    def test_interrupted_or_failed_leg_cannot_be_retried(self):
        slot, directory = self.begin()
        prepare(self.cohort, slot, directory, 'fmt', self.argv, self.env)
        with self.assertRaises(Refusal): prepare(self.cohort, slot, directory, 'fmt', self.argv, self.env)
        finish(slot, directory, 'fmt', self.argv, self.env, 7)
        with self.assertRaises(Refusal): finish(slot, directory, 'fmt', self.argv, self.env, 0)

    def test_mid_leg_environment_change_retains_key_names_without_values(self):
        slot, directory = self.begin()
        prepare(self.cohort, slot, directory, 'fmt', self.argv, self.env)
        with self.assertRaisesRegex(Refusal, 'UNRECOGNIZED_INPUT'):
            finish(slot, directory, 'fmt', self.argv, dict(self.env, UNRECOGNIZED_INPUT='private value'), 0)
        text = (directory / 'legs.jsonl').read_text()
        self.assertNotIn('private value', text)
        rows = records(directory / 'legs.jsonl')
        self.assertEqual([r['kind'] for r in rows], ['start', 'input-mismatch'])

    def test_warm_terminal_record_is_required_even_when_summary_is_present(self):
        slot, directory = self.warm()
        (self.cohort.root / 'slot-01' / 'legs.jsonl').unlink()
        with self.assertRaises(Refusal): prepare(self.cohort, slot, directory, 'fmt', self.argv, self.env)

    def test_slot_binding_refuses_siblings_wrong_source_and_stale_slot(self):
        slot, directory = self.begin()
        manifest = dict(kind='gate-throughput-measurement', commit=self.identity['source_commit'],
                        tree=self.identity['source_tree'], gate={'argv': self.identity['gate_argv']})
        _, observed = current_slot(manifest, self.root / 'manifest.json', directory)
        self.assertEqual(observed, slot)
        for changed, path in [(dict(manifest, commit='c' * 40), directory),
                              (manifest, self.root / 'slot-00'),
                              (manifest, directory.parent / 'slot-01')]:
            with self.assertRaises(Refusal): current_slot(changed, self.root / 'manifest.json', path)
        self.complete(slot)
        with self.assertRaises(Refusal): current_slot(manifest, self.root / 'manifest.json', directory)

    def test_fresh_build_feedback_path_does_not_invalidate_reuse(self):
        for index in range(2):
            channel = self.root / f'feedback-{index}'; channel.touch()
            self.env['STORYHOOK_GATE_BUILD_OUTCOME'] = str(channel)
            slot, directory = self.begin()
            self.assertEqual(prepare(self.cohort, slot, directory, 'fmt', self.argv, self.env), 'run')
            finish(slot, directory, 'fmt', self.argv, self.env, 0)
            self.complete(slot)
        channel = self.root / 'feedback-reuse'; channel.touch()
        self.env['STORYHOOK_GATE_BUILD_OUTCOME'] = str(channel)
        slot, directory = self.begin()
        self.assertEqual(prepare(self.cohort, slot, directory, 'fmt', self.argv, self.env), 'reused')

    def test_nonempty_or_symlinked_feedback_channel_refuses(self):
        channel = self.root / 'feedback'; channel.write_text('previous build error')
        self.env['STORYHOOK_GATE_BUILD_OUTCOME'] = str(channel)
        slot, directory = self.begin()
        with self.assertRaises(Refusal): prepare(self.cohort, slot, directory, 'fmt', self.argv, self.env)
        channel.unlink(); channel.symlink_to(self.root / 'missing')
        with self.assertRaises(Refusal): prepare(self.cohort, slot, directory, 'fmt', self.argv, self.env)


if __name__ == '__main__': unittest.main()
