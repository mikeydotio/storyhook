#!/usr/bin/env python3
"""Pure retained-child regressions. No subprocess, kernel census or signal runs."""
import errno
import sys
sys.dont_write_bytecode = True
import importlib.util
from pathlib import Path
import types
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
spec = importlib.util.spec_from_file_location('sh797_shim_fixture', Path(__file__).resolve().parents[1] / 'git_shim_measurement.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class Wait:
    P_PID, WEXITED, WNOWAIT, WNOHANG = 1, 2, 4, 8

    def __init__(self, results):
        self.results, self.calls = list(results), []

    def waitid(self, kind, pid, flags):
        self.calls.append((kind, pid, flags))
        value = self.results.pop(0)
        if isinstance(value, BaseException):
            raise value
        return value


def exited(pid=80):
    return types.SimpleNamespace(si_pid=pid)


def row(pid):
    return dict(pid=pid, session=80, start='fixture:' + str(pid), boot='fixture', live=True)


class Native:
    def __init__(self, pids=(80,), process=None, session=None):
        self.ids = pids
        self.reads = []
        self.process_fn = process or row
        self.session_fn = session or (lambda _pid: 80)

    def pids(self):
        return self.ids

    def process(self, pid, _boot):
        self.reads.append(('process', pid))
        return self.process_fn(pid)

    def getsid(self, pid):
        self.reads.append(('session', pid))
        return self.session_fn(pid)


def denied(_pid):
    raise PermissionError(errno.EPERM, 'controlled Darwin denial')


class RetainedChildTests(unittest.TestCase):
    def probe(self, wait):
        return lambda: module.exact_child_exit(80, nonblocking=True, wait_api=wait) is not None

    def members(self, native, wait):
        return module.retained_session_members(native, 'fixture', 80, self.probe(wait), native.getsid)

    def test_wait_identity_and_nonreaping_flags_are_required(self):
        wait = Wait([exited()])
        self.assertEqual(module.exact_child_exit(80, nonblocking=True, wait_api=wait).si_pid, 80)
        self.assertEqual(wait.calls, [(Wait.P_PID, 80, Wait.WEXITED | Wait.WNOWAIT | Wait.WNOHANG)])

    def test_wrong_child_or_blocking_absence_never_proves_exit(self):
        for answer in (exited(81), None):
            with self.subTest(answer=answer):
                with self.assertRaises(RuntimeError):
                    module.exact_child_exit(80, nonblocking=False, wait_api=Wait([answer]))
        with self.assertRaises(ChildProcessError):
            module.exact_child_exit(80, nonblocking=True, wait_api=Wait([ChildProcessError('not retained')]))

    def test_proven_exited_root_is_excluded_before_either_native_query(self):
        native = Native(process=denied, session=denied)
        self.assertEqual(self.members(native, Wait([exited()])), [])
        self.assertEqual(native.reads, [])

    def test_native_root_exit_race_requires_fresh_positive_proof(self):
        native = Native(process=denied)
        self.assertEqual(self.members(native, Wait([None, exited()])), [])
        self.assertEqual(native.reads, [('session', 80), ('process', 80)])

    def test_session_query_exit_race_requires_fresh_positive_proof(self):
        native = Native(session=denied)
        self.assertEqual(self.members(native, Wait([None, exited()])), [])
        self.assertEqual(native.reads, [('session', 80)])

    def test_live_root_is_observed_and_not_excluded(self):
        native = Native()
        self.assertEqual(self.members(native, Wait([None])), [row(80)])
        self.assertEqual(native.reads, [('session', 80), ('process', 80)])

    def test_denied_root_with_no_exit_proof_stays_unknown(self):
        native = Native(process=denied)
        with self.assertRaises(PermissionError):
            self.members(native, Wait([None, None]))

    def test_wrong_child_proof_cannot_hide_native_denial(self):
        native = Native(process=denied)
        with self.assertRaises(RuntimeError):
            self.members(native, Wait([None, exited(81)]))

    def test_proven_root_exit_does_not_hide_unreadable_descendant(self):
        native = Native((80, 81), process=denied)
        with self.assertRaises(PermissionError):
            self.members(native, Wait([exited()]))
        self.assertEqual(native.reads, [('session', 81), ('process', 81)])

    def test_proven_root_exit_keeps_live_descendant_in_census(self):
        native = Native((80, 81))
        self.assertEqual(self.members(native, Wait([exited()])), [row(81)])
        self.assertEqual(native.reads, [('session', 81), ('process', 81)])

    def test_unreadable_unknown_session_is_not_assumed_unrelated(self):
        native = Native((81,), session=denied)
        with self.assertRaises(PermissionError):
            self.members(native, Wait([]))

    def test_startup_exit_before_capture_mints_no_native_identity(self):
        native = Native(process=denied)
        self.assertIsNone(module.capture_retained_root(native, 'fixture', 80, self.probe(Wait([exited()]))))
        self.assertEqual(native.reads, [])

    def test_startup_native_exit_race_requires_exact_exit(self):
        native = Native(process=denied)
        self.assertIsNone(module.capture_retained_root(native, 'fixture', 80, self.probe(Wait([None, exited()]))))
        self.assertEqual(native.reads, [('process', 80)])
        with self.assertRaises(PermissionError):
            module.capture_retained_root(native, 'fixture', 80, self.probe(Wait([None, None])))


class PendingRootTests(unittest.TestCase):
    def test_initial_esrch_without_exit_is_pending_not_a_native_identity(self):
        native = Native(process=lambda _pid: (_ for _ in ()).throw(ProcessLookupError('transition')))
        with self.assertRaises(module.PendingRootObservation) as error:
            module.capture_retained_root(native, 'fixture', 80, lambda: False)
        self.assertEqual(error.exception.pid, 80)

    def test_census_esrch_without_exit_is_pending_not_an_empty_session(self):
        for stage in ('session', 'process'):
            failure = lambda _pid: (_ for _ in ()).throw(ProcessLookupError('transition'))
            native = Native(**{stage: failure})
            with self.subTest(stage=stage), self.assertRaises(module.PendingRootObservation):
                module.retained_session_members(native, 'fixture', 80, lambda: False, native.getsid)

    def test_pending_retry_requires_eventual_positive_exit_or_readable_native_row(self):
        for final in (None, row(80)):
            answers = iter([module.PendingRootObservation(80, 'fixture'), final])
            now = [0.0]
            def operation():
                value = next(answers)
                if isinstance(value, BaseException):
                    raise value
                return value
            result = module.retry_pending(operation, 1, clock=lambda: now[0],
                pause=lambda delay: now.__setitem__(0, now[0] + delay))
            self.assertEqual(result, final)
            self.assertEqual(now[0], 0.01)

    def test_pending_retry_preserves_original_deadline(self):
        now = [0.0]
        def pending():
            raise module.PendingRootObservation(80, 'fixture')
        with self.assertRaises(RuntimeError):
            module.retry_pending(pending, 0.02, clock=lambda: now[0],
                pause=lambda delay: now.__setitem__(0, now[0] + delay))
        self.assertEqual(now[0], 0.02)
        with self.assertRaises(RuntimeError):
            module.retry_pending(pending, 0.02, clock=lambda: now[0],
                pause=lambda _delay: self.fail('deadline renewed'))

    def test_pending_retry_never_waives_permission_child_or_cancellation_errors(self):
        for error in (PermissionError('denied'), ChildProcessError('not our child')):
            with self.subTest(error=type(error)), self.assertRaises(type(error)):
                module.retry_pending(lambda: (_ for _ in ()).throw(error), 1,
                    clock=lambda: 0, pause=lambda _delay: self.fail('unexpected retry'))
        with self.assertRaisesRegex(RuntimeError, 'cancelled'):
            module.retry_pending(lambda: self.fail('operation after cancellation'), 1,
                check=lambda: (_ for _ in ()).throw(RuntimeError('cancelled')), clock=lambda: 0)



class RepresentativeTiming(unittest.TestCase):
    def test_failed_arm_is_retained_with_original_error_without_replacement(self):
        evidence = {'samples': []}; saves = []
        before = dict(monotonic=1, load=[90, 80, 70], cores=10,
                      native_memory_pressure=1, cpu_ticks=[10, 20, 30, 0])
        def fail(*_args): raise RuntimeError('owned cleanup uncertain')
        with self.assertRaisesRegex(RuntimeError, 'cleanup uncertain'):
            module.timed_operation(evidence, 'metadata', 0, 'shim', [], '.', fail,
                                   lambda: before, lambda: saves.append(True))
        self.assertEqual(len(evidence['samples']), 1)
        self.assertEqual(evidence['samples'][0]['status'], 'failed')
        self.assertNotIn('elapsed_ns', evidence['samples'][0])
        self.assertEqual(len(saves), 2)

    def test_high_load_paired_arm_retains_actual_cpu_ticks(self):
        evidence = {'samples': []}
        observations = iter([dict(monotonic=n, load=[90, 80, 70], cores=10,
                                 native_memory_pressure=1, cpu_ticks=[n * 10, 20, 30, 0]) for n in (1, 2)])
        row = module.timed_operation(evidence, 'metadata', 0, 'direct', [], '.',
                    lambda *_: types.SimpleNamespace(elapsed_ns=123, stdout=b'ok'),
                    lambda: next(observations), lambda: None)
        self.assertEqual(row['status'], 'complete')
        self.assertEqual(row['elapsed_ns'], 123)
        self.assertEqual(row['cpu_interval']['idle_percent'], 0)

    def test_shared_five_minute_deadline_never_resets_per_command(self):
        self.assertEqual(module.remaining(300, clock=lambda: 299), 1)
        with self.assertRaisesRegex(RuntimeError, 'five-minute'):
            module.remaining(300, clock=lambda: 300)
        self.assertEqual(module.PAIR_COUNT * 2 * 3, 240)


if __name__ == '__main__':
    with (patch.object(module.subprocess, 'Popen', side_effect=AssertionError('process forbidden')),
          patch.object(module.os, 'kill', side_effect=AssertionError('signal forbidden')),
          patch.object(module.os, 'getsid', side_effect=AssertionError('native session query forbidden'))):
        unittest.main()
