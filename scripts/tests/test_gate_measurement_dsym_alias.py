"""Observe a real Cargo dSYM alias without counting its destination twice."""

from contextlib import contextmanager, nullcontext
import errno
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import gate_measurement_storage as storage
from verifier_state import Refusal


class DsymAlias(unittest.TestCase):
    def test_live_target_accounts_internal_dsym_alias_without_following_it(self):
        with tempfile.TemporaryDirectory(prefix='sh872-dsym-alias-', dir='/tmp') as temp:
            root = Path(temp).resolve()
            description = storage.reserve_description(root)
            target = Path(description['targets'][0]['path'])
            debug = target / 'debug'
            deps = debug / 'deps'
            bundle = deps / 'story-0123456789abcdef.dSYM'
            bundle.mkdir(parents=True)
            payload = bundle / 'symbols'
            payload.write_bytes(b'owned debug symbols\n' * 512)
            self.assertGreater(payload.stat().st_blocks * 512, 0)
            entries = [debug, deps, bundle, payload]

            def allocated():
                return sum(path.lstat().st_blocks * 512 for path in entries)

            def ample_disk(_path):
                return SimpleNamespace(free=200 * storage.GIB)

            control = storage.check_storage(description, disk=ample_disk)
            self.assertEqual(control['target_bytes'][str(target)], allocated())

            alias = debug / 'story.dSYM'
            alias.symlink_to('deps/story-0123456789abcdef.dSYM')
            self.assertTrue(alias.is_symlink())
            self.assertEqual(alias.resolve(), bundle)
            entries.append(alias)
            try:
                observed = storage.check_storage(description, disk=ample_disk)
            except Refusal as error:
                self.assertEqual(str(error), f'symlink in measurement storage: {alias}')
                raise
            self.assertEqual(observed['target_bytes'][str(target)], allocated())



class DsymPolicy(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='sh872-dsym-policy-', dir='/tmp')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.description = storage.reserve_description(self.root)
        self.target = Path(self.description['targets'][0]['path'])
        self.debug = self.target / 'debug'
        self.bundle = self.debug / 'deps' / 'story-0123456789abcdef.dSYM'
        self.bundle.mkdir(parents=True)
        (self.bundle / 'symbols').write_bytes(b'symbols' * 2048)
        self.alias = self.debug / 'story.dSYM'
        self.text = 'deps/' + self.bundle.name
        self.alias.symlink_to(self.text)

    def allocated(self):
        return sum(p.lstat().st_blocks * 512 for p in self.target.rglob('*'))

    def observe(self, **kwargs):
        return storage.usage(self.target, cargo_dsym=True, **kwargs)

    @contextmanager
    def on_readlink(self, action, occurrence=1):
        original = os.readlink
        calls = []

        def readlink(path, *args, **kwargs):
            result = original(path, *args, **kwargs)
            if path == self.alias.name and kwargs.get('dir_fd') is not None:
                calls.append(path)
                if len(calls) == occurrence:
                    action()
            return result

        with mock.patch.object(os, 'readlink', readlink):
            yield
        self.assertGreaterEqual(len(calls), occurrence)

    def test_strict_and_live_count_payload_once_for_distinct_stems(self):
        for stem in ('story', 'Tool_2-name'):
            with self.subTest(stem=stem):
                self.alias.unlink()
                self.bundle = self.bundle.rename(self.debug / 'deps' / (stem + '-0123456789abcdef.dSYM'))
                self.alias = self.debug / (stem + '.dSYM')
                self.alias.symlink_to('deps/' + self.bundle.name)
                expected = self.allocated()
                self.assertGreater(expected, 0)
                for live in (False, True):
                    self.assertEqual(self.observe(live=live), expected)

    def test_default_policy_still_refuses_alias(self):
        with self.assertRaisesRegex(Refusal, 'symlink'):
            storage.usage(self.target)

    def test_invalid_alias_texts_are_refused(self):
        for text in (str(self.bundle), '../deps/' + self.bundle.name,
                     'deps/other-0123456789abcdef.dSYM',
                     'deps/story-0123456789abcde.dSYM',
                     'deps/story-0123456789abcdeF.dSYM',
                     self.text + '/.', self.text + '\n', 'deps/missing.dSYM'):
            with self.subTest(text=text):
                self.alias.unlink()
                self.alias.symlink_to(text)
                with self.assertRaises((Refusal, OSError)):
                    self.observe(live=True)

    def test_wrong_layout_and_non_ascii_stem_are_refused(self):
        self.alias.unlink()
        for relative in ('story.dSYM', 'release/story.dSYM',
                         'x/debug/story.dSYM', 'debug/störy.dSYM', 'debug/.dSYM'):
            with self.subTest(relative=relative):
                alias = self.target / relative
                alias.parent.mkdir(parents=True, exist_ok=True)
                alias.symlink_to(self.text)
                with self.assertRaises(Refusal):
                    self.observe()
                alias.unlink()

    def test_dangling_file_and_chained_destinations_refuse(self):
        shutil.rmtree(self.bundle)
        external = self.root / 'external'
        external.mkdir()
        for kind in ('missing', 'file', 'chain'):
            with self.subTest(kind=kind):
                if kind == 'file':
                    self.bundle.write_text('not a directory')
                elif kind == 'chain':
                    self.bundle.symlink_to(external, target_is_directory=True)
                with self.assertRaises((Refusal, OSError)):
                    self.observe(live=True)
                if os.path.lexists(self.bundle):
                    self.bundle.unlink()

    def test_symlink_deps_component_refuses(self):
        deps = self.debug / 'deps'
        deps.rename(self.root / 'retained-deps')
        deps.symlink_to(self.root / 'retained-deps', target_is_directory=True)
        with self.assertRaises((Refusal, OSError)):
            self.observe()

    def test_alias_identity_and_text_replacement_refuse(self):
        for changed_text in (False, True):
            with self.subTest(changed_text=changed_text):
                def change():
                    self.alias.rename(self.root / 'old-alias')
                    self.alias.symlink_to('deps/other-0123456789abcdef.dSYM' if changed_text else self.text)
                with self.on_readlink(change), self.assertRaises(Refusal):
                    self.observe(live=True)
                self.alias.unlink()
                (self.root / 'old-alias').rename(self.alias)

    def test_alias_mutation_during_final_readlink_refuses(self):
        def change():
            self.alias.rename(self.root / 'old-alias')
            self.alias.symlink_to(self.text)
        with self.on_readlink(change, occurrence=2), self.assertRaises(Refusal):
            self.observe(live=True)

    def test_live_alias_disappearance_only_is_tolerated(self):
        with self.on_readlink(self.alias.unlink):
            observed = self.observe(live=True)
        self.assertEqual(observed, self.allocated())

    def test_settled_alias_disappearance_is_strict(self):
        with self.on_readlink(self.alias.unlink), self.assertRaises(FileNotFoundError):
            self.observe()

    def test_missing_destination_does_not_hide_persistent_alias(self):
        with self.on_readlink(lambda: shutil.rmtree(self.bundle)):
            with self.assertRaises(FileNotFoundError):
                self.observe(live=True)
        self.assertTrue(self.alias.is_symlink())

    def test_unexplained_enoent_does_not_hide_persistent_alias(self):
        with mock.patch.object(os, 'readlink', side_effect=FileNotFoundError(errno.ENOENT, 'sensor')):
            with self.assertRaises(FileNotFoundError):
                self.observe(live=True)

    def test_permission_and_io_failures_propagate(self):
        for code in (errno.EACCES, errno.EPERM, errno.EIO):
            with self.subTest(code=code):
                with mock.patch.object(os, 'readlink', side_effect=OSError(code, 'sensor')):
                    with self.assertRaises(OSError) as caught:
                        self.observe(live=True)
                self.assertEqual(caught.exception.errno, code)

    def test_directory_substitution_before_and_after_open_refuses(self):
        import gate_measurement_dsym as dsym
        for component in ('deps', self.bundle.name):
            for after_open in (False, True):
                with self.subTest(component=component, after_open=after_open):
                    child = self.debug / 'deps' if component == 'deps' else self.bundle
                    retained = self.root / 'retained'
                    original = os.open
                    changed = []
                    def change():
                        changed.append(True)
                        child.rename(retained)
                        child.mkdir()
                    def opening(path, flags, *args, **kwargs):
                        if path == component and not changed:
                            if after_open:
                                fd = original(path, flags, *args, **kwargs)
                                change()
                                return fd
                            change()
                        return original(path, flags, *args, **kwargs)
                    fd = original(self.debug, os.O_RDONLY | os.O_DIRECTORY)
                    try:
                        with mock.patch.object(os, 'open', opening), self.assertRaises(Refusal):
                            dsym.account_alias(fd, Path('debug/story.dSYM'), self.alias.lstat())
                    finally:
                        os.close(fd)
                    self.assertEqual(changed, [True])
                    child.rmdir()
                    retained.rename(child)

    def test_alias_ctime_and_device_must_match_observation(self):
        import gate_measurement_dsym as dsym
        actual = self.alias.lstat()
        for changed in ('st_ctime_ns', 'st_dev'):
            values = {name: getattr(actual, name) for name in (
                'st_dev', 'st_ino', 'st_mode', 'st_ctime_ns', 'st_blocks')}
            values[changed] += 1
            fd = os.open(self.debug, os.O_RDONLY | os.O_DIRECTORY)
            try:
                with self.subTest(changed=changed), self.assertRaises(Refusal):
                    dsym.account_alias(fd, Path('debug/story.dSYM'), SimpleNamespace(**values))
            finally:
                os.close(fd)

    def test_destination_open_errors_close_all_descriptors(self):
        import gate_measurement_dsym as dsym
        for component in ('deps', self.bundle.name):
            for code in (errno.EACCES, errno.EPERM, errno.EIO):
                with self.subTest(component=component, code=code):
                    original = os.open
                    opened = []
                    def opening(path, flags, *args, **kwargs):
                        if path == component:
                            raise OSError(code, 'sensor')
                        fd = original(path, flags, *args, **kwargs)
                        opened.append(fd)
                        return fd
                    parent = original(self.debug, os.O_RDONLY | os.O_DIRECTORY)
                    try:
                        with mock.patch.object(os, 'open', opening):
                            with self.assertRaises(OSError) as caught:
                                dsym.account_alias(parent, Path('debug/story.dSYM'), self.alias.lstat())
                        self.assertEqual(caught.exception.errno, code)
                        for fd in opened:
                            with self.assertRaises(OSError) as closed:
                                os.fstat(fd)
                            self.assertEqual(closed.exception.errno, errno.EBADF)
                    finally:
                        os.close(parent)

    def test_destination_on_different_device_refuses(self):
        import gate_measurement_dsym as dsym
        original = os.stat
        def observation(path, *args, **kwargs):
            value = original(path, *args, **kwargs)
            if path == self.bundle.name and kwargs.get('dir_fd') is not None:
                return SimpleNamespace(st_mode=value.st_mode, st_dev=value.st_dev + 1)
            return value
        parent = os.open(self.debug, os.O_RDONLY | os.O_DIRECTORY)
        try:
            with mock.patch.object(os, 'stat', observation), self.assertRaises(Refusal):
                dsym.account_alias(parent, Path('debug/story.dSYM'), self.alias.lstat())
        finally:
            os.close(parent)

    def test_remove_exact_revalidates_alias_after_parent_disposal_admission(self):
        from gate_measurement_targets import TargetPool, remove_exact
        campaign = self.root / 'campaign'
        campaign.mkdir(mode=0o700)
        pool = TargetPool(campaign)
        owned = pool.create('baseline-0')
        path = Path(owned['path'])
        shutil.copytree(self.debug, path / 'debug', symlinks=True)
        def replace_then_remove(identity):
            alias = path / 'debug' / 'story.dSYM'
            alias.unlink()
            alias.symlink_to(self.bundle)
            remove_exact(identity['path'], identity['device'], identity['inode'])
        with self.assertRaises(Refusal):
            pool.dispose('baseline-0', settled=lambda: True, lease=nullcontext,
                remove=replace_then_remove)
        self.assertTrue((path / 'debug' / 'deps' / self.bundle.name / 'symbols').exists())
        with self.assertRaisesRegex(Refusal, 'incomplete'):
            pool.state()

    def test_descriptors_close_on_success_and_refusal(self):
        for outcome in ('success', 'error'):
            with self.subTest(outcome=outcome):
                opened = []
                original = os.open
                def opening(*args, **kwargs):
                    fd = original(*args, **kwargs)
                    opened.append(fd)
                    return fd
                def change():
                    self.alias.rename(self.root / 'old-alias')
                    self.alias.symlink_to(self.text)
                mutation = self.on_readlink(change) if outcome == 'error' else nullcontext()
                with mock.patch.object(os, 'open', opening), mutation:
                    if outcome == 'error':
                        with self.assertRaises(Refusal):
                            self.observe()
                    else:
                        self.observe()
                self.assertGreaterEqual(len(opened), 3)
                for fd in opened:
                    with self.assertRaises(OSError) as caught:
                        os.fstat(fd)
                    self.assertEqual(caught.exception.errno, errno.EBADF)

    def test_quota_and_free_space_still_enforced(self):
        amount = self.allocated()
        observed = storage.check_storage(self.description,
            disk=lambda _: SimpleNamespace(free=200 * storage.GIB))
        self.assertEqual(observed['target_bytes'][str(self.target)], amount)
        with self.assertRaisesRegex(Refusal, 'free-space'):
            storage.check_storage(self.description,
                disk=lambda _: SimpleNamespace(free=observed['required_free_bytes'] - 1))
        def oversized(path, **kwargs):
            actual = storage.usage(path, **kwargs)
            return storage.TARGET_CAP + 1 if Path(path) == self.target else actual
        with self.assertRaisesRegex(Refusal, 'exceeded'):
            storage.check_storage(self.description, size=oversized,
                disk=lambda _: SimpleNamespace(free=200 * storage.GIB))

    def test_real_settled_target_lifecycle_removes_valid_alias(self):
        from gate_measurement_targets import TargetPool, remove_exact
        campaign = self.root / 'campaign'
        campaign.mkdir(mode=0o700)
        pool = TargetPool(campaign)
        owned = pool.create('baseline-0')
        path = Path(owned['path'])
        shutil.copytree(self.debug, path / 'debug', symlinks=True)
        pool.dispose('baseline-0', settled=lambda: True, lease=nullcontext,
            remove=lambda identity: remove_exact(identity['path'], identity['device'], identity['inode']))
        self.assertFalse(path.exists())
        self.assertEqual(pool.state(), {})

    def test_disposal_requires_settlement_and_admitted_identity(self):
        from gate_measurement_targets import TargetPool
        campaign = self.root / 'campaign'
        campaign.mkdir(mode=0o700)
        pool = TargetPool(campaign)
        owned = pool.create('baseline-0')
        path = Path(owned['path'])
        shutil.copytree(self.debug, path / 'debug', symlinks=True)
        with self.assertRaisesRegex(Refusal, 'unresolved'):
            pool.dispose('baseline-0', settled=lambda: False, lease=nullcontext,
                remove=lambda _: self.fail('must not delete'))
        def substitute():
            path.rename(self.root / 'retained-target')
            path.mkdir()
            return True
        with self.assertRaisesRegex(Refusal, 'identity changed'):
            pool.dispose('baseline-0', settled=substitute, lease=nullcontext,
                remove=lambda _: self.fail('must not delete'))



class DsymBundle(unittest.TestCase):
    def test_embedded_bundle_imports_storage_and_accounts_dsym(self):
        repository = Path(__file__).resolve().parents[2]
        manifest = re.search(r'const VERIFIER_SCRIPTS: &\[&str\] = &\[(.*?)\];',
                             (repository / 'build.rs').read_text(), re.DOTALL)
        self.assertIsNotNone(manifest)
        names = re.findall(r'"([^"\n]+)"', manifest[1])
        self.assertTrue(names)
        with tempfile.TemporaryDirectory(prefix='sh872-dsym-bundle-', dir='/tmp') as temp:
            root = Path(temp).resolve()
            bundle = root / 'bundle'
            bundle.mkdir()
            for name in names:
                destination = bundle / name
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_bytes((repository / 'scripts' / name).read_bytes())
            program = """
from pathlib import Path
import sys
from types import SimpleNamespace
bundle = Path.cwd()
sys.path.insert(0, str(bundle))
import gate_measurement_storage as storage
import gate_measurement_dsym as dsym
assert Path(storage.__file__).parent == bundle
assert Path(dsym.__file__).parent == bundle
root = bundle.parent / 'campaign'
root.mkdir()
description = storage.reserve_description(root)
target = Path(description['targets'][0]['path'])
debug = target / 'debug'
destination = debug / 'deps' / 'story-0123456789abcdef.dSYM'
destination.mkdir(parents=True)
payload = destination / 'symbols'
payload.write_bytes(b'packed debug symbols' * 1024)
assert payload.stat().st_blocks > 0
alias = debug / 'story.dSYM'
alias.symlink_to('deps/' + destination.name)
expected = sum(path.lstat().st_blocks * 512 for path in (debug, debug / 'deps', destination, payload, alias))
result = storage.check_storage(description, disk=lambda _: SimpleNamespace(free=200 * storage.GIB))
assert result['target_bytes'][str(target)] == expected
print('bundled dSYM accounting passed')
"""
            result = subprocess.run([sys.executable, '-I', '-S', '-B', '-c', program],
                                    cwd=bundle, capture_output=True, text=True, timeout=30)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(result.stdout.strip(), 'bundled dSYM accounting passed')


if __name__ == '__main__':
    unittest.main()
