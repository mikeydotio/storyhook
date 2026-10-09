"""Variable-load admission and unchanged bounded recovery; no native work."""
import copy
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from gate_measurement_exposure import PROTOCOL, annotate_processes, cpu_interval, validate_exposure
from gate_measurement_bounds import Deadline, LIMITS, validate_policy
from gate_measurement_runtime import records
from verifier_state import Refusal
import gate_measurement as scheduling


def exposure():
    return dict(monotonic=10, load=[100, 90, 80], cores=10,
                native_memory_pressure=1, cpu_ticks=[100, 200, 300, 0],
                processes=f'{os.getpid()} 1 0.0 python\n999999 1 1000.0 xcodebuild\n',
                resource_processes=f'{os.getpid()} 1 0.0 10 python\n999999 1 1000.0 10000 xcodebuild\n')


class RepresentativeLoad(unittest.TestCase):
    def test_natural_builds_and_saturated_cpu_are_exposure_not_rejection(self):
        row = exposure()
        self.assertIs(annotate_processes(row, os.getpid()), row)
        self.assertEqual(row['competing_pids'], [999999])
        self.assertEqual(row['owned_resources']['process_count'], 1)
        after = dict(row, monotonic=11, cpu_ticks=[150, 250, 300, 0])
        self.assertEqual(cpu_interval(row, after)['idle_percent'], 0)
        self.assertEqual(after['load'][0], 100)

    def test_admission_retains_load_without_wait_or_fake_gate_progress(self):
        with tempfile.TemporaryDirectory() as root, \
             mock.patch.object(scheduling, 'pressure', return_value=exposure()), \
             mock.patch.object(scheduling, 'today', return_value='2026-10-09'), \
             mock.patch.object(scheduling, 'progress') as progress, \
             mock.patch.object(scheduling.time, 'sleep') as sleep:
            scheduling.admission(Path(root), {'day': '2026-10-09'})
            saved = records(Path(root) / 'pressure.jsonl')
            self.assertEqual(saved[0]['competing_pids'], [999999])
            self.assertEqual(saved[0]['host_load_protocol'], PROTOCOL)
            sleep.assert_not_called(); progress.assert_not_called()

    def test_expired_admission_does_not_read_sensors_or_reset_deadline(self):
        with mock.patch.dict(os.environ, STORYHOOK_MEASUREMENT_END='0'), \
             mock.patch.object(scheduling, 'pressure') as pressure:
            with self.assertRaises(Refusal): scheduling.admission(Path('/unused'), {'day': '2026-10-09'})
            pressure.assert_not_called()
        now = [10]
        deadline = Deadline(5, clock=lambda: now[0])
        for value in [100, 0, 200]:
            validate_exposure(dict(exposure(), load=[value] * 3))
            now[0] += 2
        with self.assertRaises(Refusal): deadline.require('gate')
        self.assertEqual(deadline.end, 15)

    def test_unknown_or_unsafe_sensors_and_unreadable_ownership_still_refuse(self):
        for delta in [dict(load=[float('nan'), 0, 0]), dict(cores=0),
                      dict(native_memory_pressure=2), dict(native_memory_pressure=None),
                      dict(cpu_ticks=None), dict(monotonic=float('inf'))]:
            with self.subTest(delta=delta), self.assertRaises(Refusal):
                validate_exposure(dict(exposure(), **delta))
        with self.assertRaises(Refusal):
            annotate_processes(dict(exposure(), processes='unreadable'), os.getpid())

    def test_old_throughput_reservation_cannot_resume_under_new_policy(self):
        import json
        from gate_measurement_campaign import begin_window, POLICY
        from gate_measurement_storage import directory_identity
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch).resolve()
            approval = root / 'approval.json'
            approval.write_text(json.dumps(dict(version=1, kind='coordinated-measurement-start',
                story='SH-872', revision='baseline', campaign_root=str(root), authority='fixture')))
            old = dict(POLICY, version=1)
            (root / 'campaign.json').write_text(json.dumps(dict(version=1,
                root=directory_identity(root), policy=old, boot='fixture', started=10, end=72010)))
            with self.assertRaises(Refusal):
                begin_window(root, 'baseline', approval, clock=lambda: 11, boot_id='fixture')
            self.assertFalse((root / 'windows.jsonl').exists())

    def test_old_protocol_cannot_silently_reinterpret_saved_cohort(self):
        valid = dict(limits=LIMITS, protocol={'pairs': 10, 'host_load': PROTOCOL})
        validate_policy(valid)
        old = copy.deepcopy(valid)
        old['protocol'] = dict(pairs=10, idle_seconds=60, max_load_per_core=.5)
        with self.assertRaises(Refusal): validate_policy(old)
        altered = copy.deepcopy(valid); altered['limits']['gate_seconds'] += 1
        with self.assertRaises(Refusal): validate_policy(altered)

    def test_no_ticks_is_unknown_not_idle_and_load_is_not_cpu_utilization(self):
        before = exposure(); after = dict(before, monotonic=11)
        self.assertIsNone(cpu_interval(before, after)['idle_percent'])
        after['cpu_ticks'] = [100, 200, 400, 0]
        self.assertEqual(cpu_interval(before, after)['idle_percent'], 100)
        self.assertEqual(after['load'][0], 100)
        with self.assertRaises(Refusal): cpu_interval(before, before)



class ObservationDeadlines(unittest.TestCase):
    def test_nested_capture_receives_remaining_budget_and_restores_parent(self):
        from types import SimpleNamespace
        import gate_measurement_runtime as runtime
        now = [10]
        with mock.patch.dict(os.environ, STORYHOOK_MEASUREMENT_END='100.000', STORYHOOK_MEASUREMENT_OPERATIONS='/unused'), \
             mock.patch.object(runtime.time, 'monotonic', side_effect=lambda: now[0]), \
             mock.patch('gate_measurement_command.bounded', return_value=SimpleNamespace(returncode=0, stdout='ok')) as command:
            with runtime.observation_deadline(20):
                self.assertEqual(os.environ['STORYHOOK_MEASUREMENT_END'], '20')
                now[0] = 19
                self.assertEqual(runtime.capture(['fixture-sensor']), 'ok')
                self.assertEqual(command.call_args.kwargs['seconds'], 1)
                with runtime.observation_deadline(90):
                    self.assertEqual(os.environ['STORYHOOK_MEASUREMENT_END'], '20.0')
            self.assertEqual(os.environ['STORYHOOK_MEASUREMENT_END'], '100.000')

    def test_late_observation_is_preserved_but_refused_and_environment_restored(self):
        import gate_measurement_runtime as runtime
        now = [10]
        with tempfile.TemporaryDirectory() as scratch, \
             mock.patch.dict(os.environ, STORYHOOK_MEASUREMENT_END='100'), \
             mock.patch.object(runtime.time, 'monotonic', side_effect=lambda: now[0]):
            path = Path(scratch) / 'pressure.jsonl'
            with self.assertRaisesRegex(Refusal, 'original allowance'):
                with runtime.observation_deadline(20):
                    runtime.journal(path, dict(exposure(), kind='late-observation'))
                    now[0] = 21
            self.assertEqual(records(path)[0]['kind'], 'late-observation')
            self.assertEqual(os.environ['STORYHOOK_MEASUREMENT_END'], '100')

    def test_sensor_exception_and_missing_parent_restore_environment(self):
        from gate_measurement_runtime import observation_deadline
        with mock.patch.dict(os.environ):
            os.environ.pop('STORYHOOK_MEASUREMENT_END', None)
            with self.assertRaisesRegex(OSError, 'sensor failed'):
                with observation_deadline(20, clock=lambda: 10):
                    raise OSError('sensor failed')
            self.assertNotIn('STORYHOOK_MEASUREMENT_END', os.environ)
        for value in [True, float('nan'), float('inf')]:
            with self.subTest(value=value), self.assertRaises(Refusal):
                with observation_deadline(value, clock=lambda: 10): pass

    def owned_health_expiry(self, *, before_launch, cleanup_timeout=False):
        import signal
        import subprocess
        import gate_measurement_execution as execution
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch).resolve()
            directory = root / 'measurement-results-v1' / 'baseline' / 'slot-00'
            directory.mkdir(parents=True)
            identity = dict(kind='gate-throughput-measurement', gate={'argv': ['make', 'test']},
                            commit='a', tree='b', campaign_root=str(root), common=str(root), worktree=str(root / 'source'))
            slot = {'identity': {'source_commit': 'a', 'source_tree': 'b'}}
            now = [10]; health_calls = []
            def health():
                health_calls.append(float(os.environ['STORYHOOK_MEASUREMENT_END']))
                if before_launch or len(health_calls) == 2: now[0] = 21
            child = mock.Mock()
            child.poll.return_value = None
            if cleanup_timeout: child.wait.side_effect = subprocess.TimeoutExpired('fixture', 35)
            def spawn(*_args, **_kwargs):
                now[0] = max(now[0], 16)
                return child
            gate = execution.OwnedGate(str(root / 'manifest.json'), ['make', 'test'], health=health)
            with mock.patch.object(execution, 'validate', return_value=identity), \
                 mock.patch.object(execution.time, 'monotonic', side_effect=lambda: now[0]), \
                 mock.patch.object(execution.subprocess, 'Popen', side_effect=spawn) as start, \
                 mock.patch.dict(os.environ, STORYHOOK_VERIFIER_OWNER='fixture',
                    STORYHOOK_GATE_PROGRESS=str(root / 'outer.jsonl'), STORYHOOK_MEASUREMENT_END='100'):
                with self.assertRaises(Refusal) as failure:
                    gate(slot, directory, 20)
                if before_launch:
                    start.assert_not_called()
                    self.assertFalse((directory / 'execution.json').exists())
                else:
                    start.assert_called_once()
                    child.send_signal.assert_called_once_with(signal.SIGTERM)
                    child.wait.assert_called_once_with(timeout=35)
                    import json
                    self.assertEqual(json.loads((directory / 'execution.json').read_text())['state'], 'pending')
                self.assertTrue(all(end == 20 for end in health_calls))
                self.assertEqual(os.environ['STORYHOOK_MEASUREMENT_END'], '100')
                self.assertIn('cleanup observation expired' if cleanup_timeout else 'original allowance', str(failure.exception))

    def test_health_expiry_before_launch_never_starts_gate(self):
        self.owned_health_expiry(before_launch=True)

    def test_health_expiry_during_gate_requests_owned_cleanup(self):
        self.owned_health_expiry(before_launch=False)

    def test_health_expiry_with_cleanup_timeout_keeps_pending_evidence(self):
        self.owned_health_expiry(before_launch=False, cleanup_timeout=True)

if __name__ == '__main__': unittest.main()
