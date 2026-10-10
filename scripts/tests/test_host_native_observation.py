"""Native custody observation without granting authority from partial identity."""

import ctypes
import errno
import json
import os
from pathlib import Path
import sys
import tempfile
import time
from types import SimpleNamespace
import unittest
from unittest import mock

sys.dont_write_bytecode = True
SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
from build_products import ProductCustody
from host_admission import native
from host_admission.policy import Refusal
from host_admission.supervisor import ManagedProcess


class NativeObservation(unittest.TestCase):
    def short_status(self, pid=123, status=2, size=64, error=0, session=456):
        library = mock.Mock()
        def result(observed, flavor, _arg, destination, capacity):
            self.assertEqual((observed, flavor, capacity), (123, 13, 64))
            info = ctypes.cast(destination, ctypes.POINTER(native.BsdShortInfo)).contents
            info.pid, info.status = pid, status
            ctypes.set_errno(error)
            return size
        library.proc_pidinfo.side_effect = result
        with mock.patch.object(native.sys, 'platform', 'darwin'), \
             mock.patch.object(native.ctypes, 'CDLL', return_value=library), \
             mock.patch.object(native.os, 'getsid', return_value=session):
            return native.session_member_is_live(123, 456, 'boot')

    def test_short_status_requires_positive_live_status_and_current_session(self):
        self.assertTrue(self.short_status())
        self.assertFalse(self.short_status(status=5))
        self.assertFalse(self.short_status(session=999))

    def test_short_status_denial_truncation_and_identity_mismatch_refuse(self):
        for kwargs in ({'size': 0, 'error': errno.EPERM}, {'size': 12},
                       {'pid': 999}, {'status': 0}):
            with self.subTest(kwargs=kwargs), self.assertRaises((OSError, native.Refusal)):
                self.short_status(**kwargs)

    def failed_info(self, failure, probe):
        library = mock.Mock()
        def result(*_):
            self.assertEqual(ctypes.get_errno(), 0, 'stale errno must not become current evidence')
            ctypes.set_errno(failure)
            return 0
        library.proc_pidinfo.side_effect = result
        ctypes.set_errno(errno.EPERM)
        with mock.patch.object(native.ctypes, 'CDLL', return_value=library), \
             mock.patch.object(native.os, 'kill', side_effect=probe) as check:
            try:
                native.bsd_info(123)
            finally:
                check.assert_called_once_with(123, 0)

    def test_failed_observation_is_gone_only_after_kernel_confirms_esrch(self):
        with self.assertRaises(ProcessLookupError):
            self.failed_info(errno.EPERM, ProcessLookupError(errno.ESRCH, 'gone'))

    def test_live_or_denied_process_preserves_observation_failure(self):
        for probe in (None, PermissionError(errno.EPERM, 'denied')):
            with self.subTest(probe=probe), self.assertRaises(PermissionError):
                self.failed_info(errno.EPERM, probe)

    def test_missing_errno_is_unknown_not_stale_permission_or_success(self):
        with self.assertRaises(OSError) as raised:
            self.failed_info(0, None)
        self.assertEqual(raised.exception.errno, errno.EIO)


class Observation(unittest.TestCase):
    def test_unreadable_session_member_is_retained_for_drain(self):
        process = ManagedProcess.__new__(ManagedProcess)
        process.child = SimpleNamespace(pid=100)
        process.boot = 'fixture'
        process.trace = None
        process.leader_signals = set()
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


@unittest.skipUnless(sys.platform == 'darwin', 'native macOS custody')
class NativeIntegration(unittest.TestCase):
    def test_managed_setuid_ps_is_observable_without_privileged_identity(self):
        # Retain the private custody record, including failed attempts.
        root = Path(tempfile.mkdtemp(prefix='host-native-observation-', dir='/tmp')).resolve()
        command = ['/bin/ps', '-p', str(os.getpid()), '-o', 'pid=']
        custody = ProductCustody(root, command)
        deadline = time.monotonic() + 30

        class Bound:
            def publish(self):
                if time.monotonic() >= deadline:
                    raise Refusal('native custody fixture exceeded its 30-second bound')

        process = ManagedProcess(custody, custody.lease, command,
                                 publisher=Bound(), grant_environment=False)
        try:
            self.assertEqual(process.wait(), 0)
        finally:
            process.close()
        record = json.loads((custody.root / 'record.json').read_text())
        self.assertEqual((record['state'], record['executions']), ('finished', []))


if __name__ == '__main__':
    unittest.main()
