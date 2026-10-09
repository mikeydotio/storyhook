"""SH-801 bounded-policy and conservative child-observation regressions."""

import datetime
import errno
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
                mock.patch('host_admission.supervisor.native.process',
                           side_effect=[PermissionError(errno.EPERM, 'denied'), {'live': True}]):
            self.assertEqual(process._members(), [101, 102])
        self.assertIn('101', process.observation_failure)
        with mock.patch('host_admission.supervisor.os.getsid', side_effect=[999, 100]), \
                mock.patch('host_admission.supervisor.os.kill') as kill:
            process._signal([101, 102], 15)
        kill.assert_called_once_with(102, 15)


if __name__ == '__main__':
    unittest.main()
