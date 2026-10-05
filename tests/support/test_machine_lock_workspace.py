"""Exercise observer scheduling without sleeping or signalling real processes."""

import signal
import subprocess
import unittest
from unittest.mock import Mock, patch

import machine_lock_workspace as fixture


class TimerObservation(unittest.TestCase):
    """The caller's patience bounds readiness, including each process census."""

    def test_late_timer_uses_the_callers_remaining_patience(self):
        wrapper = Mock(pid=100)
        wrapper.poll.return_value = None
        running = {200: (100, 'S', 'bash'), 300: (200, 'S', '/bin/sleep')}
        stopped = {300: (200, 'T', '/bin/sleep')}
        with patch.object(fixture.time, 'monotonic', side_effect=[0, 11, 12]), \
                patch.object(fixture, 'processes', side_effect=[running, stopped]) as census, \
                patch.object(fixture.os, 'kill') as kill:
            self.assertEqual(fixture.stop_owned_timer(wrapper, 30), 300)
        self.assertEqual(census.call_args_list, [unittest.mock.call(19), unittest.mock.call(18)])
        kill.assert_called_once_with(300, signal.SIGSTOP)

    def test_deadline_exhaustion_names_the_wrapper(self):
        wrapper = Mock(pid=100)
        with patch.object(fixture.time, 'monotonic', side_effect=[0, 30]), \
                patch.object(fixture, 'processes') as census:
            with self.assertRaisesRegex(AssertionError, 'wrapper 100.*30'):
                fixture.stop_owned_timer(wrapper, 30)
        census.assert_not_called()

    def test_census_uses_the_supplied_bound(self):
        with patch.object(fixture.subprocess, 'check_output', return_value='') as run:
            self.assertEqual(fixture.processes(17), {})
        self.assertEqual(run.call_args.kwargs['timeout'], 17)

    def test_failed_stop_observation_resumes_the_unreturned_timer(self):
        wrapper = Mock(pid=100)
        running = {200: (100, 'S', 'bash'), 300: (200, 'S', '/bin/sleep')}
        failure = subprocess.TimeoutExpired('ps', 18)
        with patch.object(fixture.time, 'monotonic', side_effect=[0, 11, 12]), \
                patch.object(fixture, 'processes', side_effect=[running, failure]), \
                patch.object(fixture.os, 'kill') as kill:
            with self.assertRaises(subprocess.TimeoutExpired):
                fixture.stop_owned_timer(wrapper, 30)
        self.assertEqual(kill.call_args_list, [
            unittest.mock.call(300, signal.SIGSTOP),
            unittest.mock.call(300, signal.SIGCONT),
        ])


if __name__ == '__main__':
    unittest.main()
