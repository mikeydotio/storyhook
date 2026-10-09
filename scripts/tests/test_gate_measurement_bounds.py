"""SH-801 bounded-policy and conservative child-observation regressions."""

import datetime
import errno
import copy
import tempfile
import sys
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest import mock

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from gate_measurement_bounds import Deadline, LIMITS, require_same_day, start_slot, validate_policy
from host_admission.supervisor import ManagedProcess
from verifier_state import Refusal
import gate_measurement_storage as storage


class Bounds(unittest.TestCase):
    def test_monotonic_parent_caps_children_at_exact_boundary(self):
        now = [10]
        parent = Deadline(20, clock=lambda: now[0])
        now[0] = 15
        child = parent.child(100)
        self.assertEqual(child.require('child'), 15)
        now[0] = 30
        with self.assertRaises(Refusal):
            child.require('child')
        now[0] = 9
        with self.assertRaises(Refusal):
            parent.require('parent')

    def test_nonfinite_and_nonpositive_ceilings_refuse(self):
        for value in [True, 0, -1, float('inf'), float('nan')]:
            with self.subTest(value=value), self.assertRaises(Refusal):
                Deadline(value)

    def test_full_allowance_must_fit_original_day(self):
        now = datetime.datetime(2026, 10, 9, 23, 30, tzinfo=datetime.timezone.utc)
        with self.assertRaises(Refusal):
            require_same_day('2026-10-09', 3600, now=now)
        with self.assertRaises(Refusal):
            require_same_day('2026-10-08', 1, now=now)
        require_same_day('2026-10-09', 1799, now=now)

    def test_gate_slots_count_warmups_failures_and_unsettled_attempts(self):
        first = start_slot([], 'warmup', -1)
        self.assertEqual(first['slot'], 0)
        with self.assertRaises(Refusal):
            start_slot([first], 'sample', 0)
        events = [first, {'kind': 'gate-settled', 'slot': 0, 'exit_code': 7}]
        with self.assertRaises(Refusal):
            start_slot(events, 'warmup', -1)
        for index in range(20):
            row = start_slot(events, 'sample', index)
            events.extend([row, {'kind': 'gate-settled', 'slot': row['slot']}])
        with self.assertRaises(Refusal):
            start_slot(events, 'sample', 20)

    def test_missing_or_weakened_policy_refuses(self):
        validate_policy({'limits': LIMITS})
        for identity in [{}, {'limits': dict(LIMITS, gate_seconds=3601)}]:
            with self.assertRaises(Refusal):
                validate_policy(identity)


class Observation(unittest.TestCase):
    def test_unreadable_session_member_is_retained_for_drain(self):
        process = ManagedProcess.__new__(ManagedProcess)
        process.child = SimpleNamespace(pid=100)
        process.boot = 'fixture'
        process.observation_failure = None
        with mock.patch('host_admission.supervisor.native.session_members', return_value=[101, 102]), \
                mock.patch('host_admission.supervisor.native.session_member_is_live',
                           side_effect=[PermissionError(errno.EPERM, 'denied'), True]):
            self.assertEqual(process._members(), [101, 102])
        self.assertIn('101', process.observation_failure)
        with mock.patch('host_admission.supervisor.os.getsid', side_effect=[999, 100]), \
                mock.patch('host_admission.supervisor.os.kill') as kill:
            process._signal([101, 102], 15)
        kill.assert_called_once_with(102, 15)


class Storage(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='sh801-storage-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.description = storage.reserve_description(self.root)

    def check(self, **kwargs):
        return storage.check_storage(self.description,
                                     disk=lambda _: SimpleNamespace(free=storage.INITIAL_FREE),
                                     **kwargs)

    def test_no_existing_or_shared_target_is_adopted(self):
        with self.assertRaises(FileExistsError):
            storage.reserve_description(self.root)
        original = copy.deepcopy(self.description)
        self.description['targets'][0]['path'] = str(self.root.parent)
        with self.assertRaises(Refusal):
            self.check()
        self.description = original
        self.assertFalse(self.check()['disk_reserved'])

    def test_target_inode_and_symlink_substitution_refuse(self):
        target = Path(self.description['targets'][0]['path'])
        retained = target.with_name('retained')
        target.rename(retained)
        target.mkdir()
        with self.assertRaises(Refusal):
            self.check()
        target.rmdir()
        target.symlink_to(retained)
        with self.assertRaises(Refusal):
            self.check()
        self.assertTrue(retained.is_dir())

    def test_low_space_growth_and_unknown_sensors_refuse(self):
        for free in [None, storage.SYSTEM_HEADROOM - 1]:
            with self.subTest(free=free), self.assertRaises(Refusal):
                storage.check_storage(self.description,
                                      disk=lambda _: SimpleNamespace(free=free))
        for cap in ['target', 'evidence']:
            def size(path, **_kwargs):
                if (Path(path) == self.root) == (cap == 'evidence'):
                    return storage.TARGET_CAP + storage.EVIDENCE_CAP
                return 0
            with self.subTest(cap=cap), self.assertRaises(Refusal):
                self.check(size=size)
        with self.assertRaises(OSError):
            storage.check_storage(self.description,
                                  disk=mock.Mock(side_effect=OSError('unavailable')))
        self.assertTrue(Path(self.description['targets'][0]['path']).exists())

    def test_warning_critical_and_unknown_pressure_stop_work(self):
        self.assertEqual(storage.pressure_level(lambda: 1), 1)
        for value in [2, 4, 0, None, True]:
            with self.subTest(value=value), self.assertRaises(Refusal):
                storage.pressure_level(lambda: value)


if __name__ == '__main__':
    unittest.main()
