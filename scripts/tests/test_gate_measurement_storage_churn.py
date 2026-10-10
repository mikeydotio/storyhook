"""Live storage observations tolerate deletion, but never identity substitution."""

from contextlib import contextmanager
import errno
import os
from pathlib import Path
import shutil
import socket
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import gate_measurement_storage as storage
from verifier_state import Refusal


class StorageChurn(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='sh872-storage-churn-', dir='/tmp')
        self.addCleanup(self.temp.cleanup)
        self.parent = Path(self.temp.name).resolve()
        self.root = self.parent / 'root'
        self.root.mkdir()
        self.stable = self.root / 'retained.rlib'
        self.stable.write_bytes(b'retained compiler output\n' * 256)
        self.stable_bytes = self.stable.stat().st_blocks * 512
        self.assertGreater(self.stable_bytes, 0)

    @contextmanager
    def after_enumeration(self, directory, action):
        real = os.scandir
        identity = directory.stat()
        done = []

        @contextmanager
        def scan(path):
            observed = os.fstat(path) if isinstance(path, int) else Path(path).stat()
            with real(path) as entries:
                yield entries
            if (observed.st_dev, observed.st_ino) == (identity.st_dev, identity.st_ino) and not done:
                done.append(True)
                action()

        with mock.patch.object(storage.os, 'scandir', scan):
            yield
        self.assertEqual(done, [True])

    @contextmanager
    def before_child_open(self, child, action):
        real = os.open
        done = []

        def opening(path, flags, *args, **kwargs):
            if path == child.name and kwargs.get('dir_fd') is not None and not done:
                done.append(True)
                action()
            return real(path, flags, *args, **kwargs)

        with mock.patch.object(storage.os, 'open', opening):
            yield
        self.assertEqual(done, [True])

    def test_removed_enumerated_object_does_not_abort_stable_file_accounting(self):
        transient = self.root / 'compiler.rcgu.o'
        transient.write_bytes(b'temporary compiler output\n' * 256)
        self.assertEqual(storage.usage(self.root),
                         self.stable_bytes + transient.stat().st_blocks * 512)
        with self.after_enumeration(self.root, transient.unlink):
            observed = storage.usage(self.root, live=True)
        self.assertFalse(transient.exists())
        self.assertEqual(observed, self.stable_bytes)

    def test_nested_directory_disappearance_after_enumeration(self):
        child = self.root / 'nested'
        child.mkdir()
        with self.after_enumeration(self.root, child.rmdir):
            self.assertEqual(storage.usage(self.root, live=True), self.stable_bytes)

    def test_nested_directory_disappearance_between_stat_and_open(self):
        child = self.root / 'nested'
        child.mkdir()
        with self.before_child_open(child, child.rmdir):
            self.assertEqual(storage.usage(self.root, live=True), self.stable_bytes)

    def test_nested_directory_disappearance_after_open(self):
        child = self.root / 'nested'
        child.mkdir()
        with self.after_enumeration(child, child.rmdir):
            self.assertEqual(storage.usage(self.root, live=True), self.stable_bytes)

    def test_nested_disappearance_before_enumeration_is_tolerated_only_when_absent(self):
        for missing in (False, True):
            with self.subTest(missing=missing):
                child = self.root / 'nested'
                child.mkdir()
                identity = child.stat()
                real = os.scandir
                triggered = []

                def scan(fd):
                    observed = os.fstat(fd)
                    if observed.st_ino == identity.st_ino:
                        triggered.append(True)
                        if missing:
                            child.rmdir()
                        raise FileNotFoundError(errno.ENOENT, 'directory enumeration failed', str(child))
                    return real(fd)

                with mock.patch.object(storage.os, 'scandir', scan):
                    if missing:
                        self.assertEqual(storage.usage(self.root, live=True), self.stable_bytes)
                    else:
                        with self.assertRaises(FileNotFoundError):
                            storage.usage(self.root, live=True)
                self.assertEqual(triggered, [True])
                if child.exists():
                    child.rmdir()

    def test_replacement_after_open_before_enumeration_stays_anchored_and_refuses(self):
        child = self.root / 'nested'
        child.mkdir()
        original = child.stat().st_ino
        real = os.scandir
        seen = []

        def scan(fd):
            inode = os.fstat(fd).st_ino
            seen.append(inode)
            if inode == original:
                child.rename(self.parent / 'retained')
                child.mkdir()
                (child / 'foreign').write_text('must not inspect')
            return real(fd)

        with mock.patch.object(storage.os, 'scandir', scan), self.assertRaises(Refusal):
            storage.usage(self.root, live=True)
        self.assertIn(original, seen)
        self.assertNotIn(child.stat().st_ino, seen)

    def test_missing_root_is_never_a_tolerated_live_descendant(self):
        expected = storage.directory_identity(self.root)
        self.stable.unlink()
        self.root.rmdir()
        with self.assertRaises(FileNotFoundError):
            storage.usage(self.root, live=True, expected=expected)

    def test_settled_nested_scan_keeps_disappearance_strict(self):
        child = self.root / 'nested'
        child.mkdir()
        with self.before_child_open(child, child.rmdir), self.assertRaises(FileNotFoundError):
            storage.usage(self.root)

    def test_directory_replacement_before_open_refuses_without_enumerating_replacement(self):
        for link in (False, True):
            with self.subTest(link=link):
                child = self.root / 'nested'
                child.mkdir()
                retained = self.parent / 'retained'
                replacement = self.parent / 'replacement'
                replacement.mkdir()
                (replacement / 'foreign').write_text('must not inspect')
                seen = []
                real = os.scandir

                def scan(path):
                    observed = os.fstat(path) if isinstance(path, int) else Path(path).stat()
                    seen.append(observed.st_ino)
                    return real(path)

                replacement_inode = replacement.stat().st_ino

                def substitute():
                    child.rename(retained)
                    if link:
                        child.symlink_to(replacement, target_is_directory=True)
                    else:
                        replacement.rename(child)

                with mock.patch.object(storage.os, 'scandir', scan), self.before_child_open(child, substitute):
                    with self.assertRaises((Refusal, OSError)):
                        storage.usage(self.root, live=True)
                self.assertNotIn(replacement_inode, seen)
                if link:
                    child.unlink()
                    shutil.rmtree(replacement)
                else:
                    shutil.rmtree(child)
                retained.rmdir()

    def test_directory_replacement_after_open_refuses(self):
        child = self.root / 'nested'
        child.mkdir()

        def substitute():
            child.rename(self.parent / 'retained')
            child.mkdir()

        with self.after_enumeration(child, substitute), self.assertRaises(Refusal):
            storage.usage(self.root, live=True)

    def test_root_disappearance_replacement_and_link_substitution_refuse(self):
        for replacement in ('absent', 'directory', 'link'):
            with self.subTest(replacement=replacement):
                retained = self.parent / 'retained'

                def substitute():
                    self.root.rename(retained)
                    if replacement == 'directory':
                        self.root.mkdir()
                    elif replacement == 'link':
                        self.root.symlink_to(retained, target_is_directory=True)

                with self.after_enumeration(self.root, substitute):
                    with self.assertRaises((Refusal, FileNotFoundError)):
                        storage.usage(self.root, live=True)
                if self.root.is_symlink():
                    self.root.unlink()
                elif self.root.exists():
                    self.root.rmdir()
                retained.rename(self.root)

    def test_admitted_root_identity_is_bound_to_opened_descriptor(self):
        expected = storage.directory_identity(self.root)
        real = os.open
        done = []

        def substitute(path, flags, *args, **kwargs):
            if Path(path) == self.root and not done:
                done.append(True)
                self.root.rename(self.parent / 'retained')
                self.root.mkdir()
            return real(path, flags, *args, **kwargs)

        with mock.patch.object(storage.os, 'open', substitute), self.assertRaises(Refusal):
            storage.usage(self.root, live=True, expected=expected)
        self.assertEqual(done, [True])

    def test_settled_scan_keeps_disappearance_strict(self):
        with self.after_enumeration(self.root, self.stable.unlink), self.assertRaises(FileNotFoundError):
            storage.usage(self.root)

    def test_permission_io_and_unexpected_errors_are_not_swallowed(self):
        for operation in ('stat', 'scandir', 'open'):
            for code in (errno.EACCES, errno.EPERM, errno.EIO, errno.ENOTDIR):
                with self.subTest(operation=operation, code=code):
                    real = getattr(os, operation)

                    def fail(*args, **kwargs):
                        if operation != 'stat' or kwargs.get('dir_fd') is not None:
                            raise OSError(code, 'injected sensor failure')
                        return real(*args, **kwargs)

                    with mock.patch.object(storage.os, operation, fail), self.assertRaises(OSError) as caught:
                        storage.usage(self.root, live=True)
                    self.assertEqual(caught.exception.errno, code)

    def test_descriptors_close_on_success_disappearance_and_refusal(self):
        for outcome in ('success', 'disappearance', 'refusal'):
            with self.subTest(outcome=outcome):
                child = self.root / 'nested'
                child.mkdir()
                opened = []
                real = os.open

                def opening(*args, **kwargs):
                    fd = real(*args, **kwargs)
                    opened.append(fd)
                    return fd

                def action():
                    if outcome == 'disappearance':
                        child.rmdir()
                    elif outcome == 'refusal':
                        (child / 'link').symlink_to(self.stable)

                with mock.patch.object(storage.os, 'open', opening), self.after_enumeration(self.root, action):
                    if outcome == 'refusal':
                        with self.assertRaises(Refusal):
                            storage.usage(self.root, live=True)
                    else:
                        storage.usage(self.root, live=True)
                self.assertTrue(opened)
                for fd in opened:
                    with self.assertRaises(OSError) as caught:
                        os.fstat(fd)
                    self.assertEqual(caught.exception.errno, errno.EBADF)
                if child.exists():
                    shutil.rmtree(child)

    def test_evidence_links_sockets_and_exact_exclusions(self):
        excluded = self.root / 'target'
        excluded.mkdir()
        (excluded / 'refused-link').symlink_to(self.stable)
        link = self.root / 'link'
        link.symlink_to(self.stable)
        sock = socket.socket(socket.AF_UNIX)
        self.addCleanup(sock.close)
        sock.bind(str(self.root / 'socket'))
        with self.assertRaises(Refusal):
            storage.usage(self.root, live=True)
        self.assertEqual(storage.usage(self.root, live=True, excluded=(excluded,),
                                       allow_links=True, allow_sockets=True),
                         self.stable_bytes + link.lstat().st_blocks * 512)
        with self.assertRaises(Refusal):
            storage.usage(self.root, live=True, excluded=(excluded,), allow_links=True)

    def test_target_and_campaign_identities_rechecked_after_all_scans(self):
        description = storage.reserve_description(self.root)
        target = Path(description['targets'][0]['path'])
        for replaced in (target, self.root):
            with self.subTest(replaced=replaced):
                retained = self.parent / 'retained'

                def size(path, **kwargs):
                    if Path(path) == self.root:
                        replaced.rename(retained)
                        replaced.mkdir()
                    return 0

                with self.assertRaises(Refusal):
                    storage.check_storage(description, size=size,
                        disk=lambda _: SimpleNamespace(free=storage.INITIAL_FREE))
                replaced.rmdir()
                retained.rename(replaced)

    def test_churn_preserves_caps_and_remaining_growth_headroom(self):
        description = storage.reserve_description(self.root)
        target = Path(description['targets'][0]['path'])
        transient = target / 'compiler.rcgu.o'
        observed = []

        def size(path, **kwargs):
            count = storage.usage(path, **kwargs)
            observed.append((Path(path), count))
            return count

        for low in (False, True):
            transient.write_bytes(b'temporary' * 1024)
            evidence = storage.usage(self.root, excluded=(target,), allow_links=True, allow_sockets=True)
            required = storage.SYSTEM_HEADROOM + storage.TARGET_CAP + storage.EVIDENCE_CAP - evidence
            with self.after_enumeration(target, transient.unlink):
                if low:
                    with self.assertRaisesRegex(Refusal, 'free-space'):
                        storage.check_storage(description, size=size,
                            disk=lambda _: SimpleNamespace(free=required - 1))
                else:
                    result = storage.check_storage(description, size=size,
                        disk=lambda _: SimpleNamespace(free=required))
                    self.assertEqual(result['target_bytes'][str(target)], 0)
                    self.assertEqual(result['required_free_bytes'], required)
                    self.assertEqual(result['remaining_growth_bytes'], required - storage.SYSTEM_HEADROOM)
                    self.assertFalse(result['disk_reserved'])
        self.assertTrue(observed)
        for path, cap in ((target, storage.TARGET_CAP), (self.root, storage.EVIDENCE_CAP)):
            def oversized(actual, **kwargs):
                return cap + 1 if Path(actual) == path else 0
            with self.subTest(cap=cap), self.assertRaisesRegex(Refusal, 'exceeded'):
                storage.check_storage(description, size=oversized,
                    disk=lambda _: SimpleNamespace(free=storage.INITIAL_FREE))


if __name__ == '__main__':
    unittest.main()
