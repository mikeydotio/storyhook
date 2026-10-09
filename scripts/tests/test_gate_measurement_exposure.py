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


if __name__ == '__main__': unittest.main()
