"""Constrained public CA dependencies; real files, no certificate changes."""
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from gate_measurement_bounds import Deadline
from gate_measurement_inputs import python_linked_packages, snapshot, _snapshot, check_audit
from verifier_state import Refusal


class PythonCABundleInputs(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='sh872-ca-', dir='/tmp')
        self.addCleanup(self.temp.cleanup)
        self.prefix = Path(self.temp.name).resolve()
        self.runtime = self.prefix / 'Cellar/python@3.14/3.14.8'
        self.library = self.prefix / 'lib/python3.14/site-packages'
        self.package = self.prefix / 'Cellar/certifi/2026.8.17'
        self.certifi = self.package / 'lib/python3.14/site-packages/certifi'
        self.bundle = self.prefix / 'etc/ca-certificates/cert.pem'
        for path in (self.runtime, self.library, self.certifi, self.bundle.parent):
            path.mkdir(parents=True)
        self.bundle.write_bytes(b'synthetic public certificate bundle\n')
        self.source = self.certifi / 'cacert.pem'
        self.source.symlink_to(self.bundle)
        (self.library / 'certifi').symlink_to(self.certifi)
        self.files = {}

    def discover(self):
        return python_linked_packages(self.runtime, [self.library], external_files=self.files)

    def capture(self, path=None, audit=None):
        kwargs = dict(allowed=(self.package, self.bundle), external_files=self.files)
        if audit is None:
            return snapshot(path or self.package, Deadline(30), **kwargs)
        return _snapshot(path or self.package, Deadline(30), audit=audit, **kwargs)

    def test_certifi_canonical_public_bundle_is_an_exact_dependency(self):
        dependencies = self.discover()
        self.assertCountEqual(dependencies.values(), [str(self.package), str(self.bundle)])
        self.assertEqual(self.files, {str(self.bundle): {'kind': 'public-ca-bundle', 'sources': [str(self.source)]}})
        before = self.capture()
        self.bundle.write_bytes(b'changed public bundle')
        self.assertNotEqual(before, self.capture())

    def test_wrong_origins_refuse_before_and_after_valid_source(self):
        for name, valid_first in (('aaa', True), ('zzz', False)):
            with self.subTest(valid_first=valid_first):
                self.files.clear()
                rogue_package = self.prefix / 'Cellar' / name / '1.0'
                rogue_package.mkdir(parents=True)
                (rogue_package / 'rogue').symlink_to(self.bundle)
                with self.assertRaises(Refusal):
                    python_linked_packages(self.runtime, [self.package, rogue_package],
                                           external_files=self.files)
                self.assertEqual(bool(self.files), valid_first)

    def test_wrong_origins_cannot_use_an_already_declared_target(self):
        self.discover()
        rogue = self.library / 'secret'
        rogue.symlink_to(self.bundle)
        with self.assertRaises(Refusal): self.capture(rogue)

    def test_other_prefix_lookalikes_unversioned_and_source_alias_refuse(self):
        self.source.unlink()
        for relative in ('Cellar/certifi/current/lib/python3.14/site-packages/certifi/cacert.pem',
                         'Cellar/certifi-extra/1/lib/python3.14/site-packages/certifi/cacert.pem',
                         'Cellar/certifi/1/lib/pythonX/site-packages/certifi/cacert.pem',
                         'other/Cellar/certifi/1/lib/python3.14/site-packages/certifi/cacert.pem'):
            with self.subTest(relative=relative):
                origin = self.prefix / relative
                origin.parent.mkdir(parents=True)
                origin.symlink_to(self.bundle)
                with self.assertRaises(Refusal):
                    python_linked_packages(self.runtime, [origin.parent], external_files={})
                origin.unlink()
        self.source.symlink_to(self.bundle)
        self.discover()
        alias = self.prefix / 'alias'; alias.symlink_to(self.package)
        with self.assertRaises(Refusal):
            self.capture(alias / 'lib/python3.14/site-packages/certifi/cacert.pem')

    def substitute(self, kind):
        self.bundle.unlink()
        if kind == 'directory': self.bundle.mkdir()
        elif kind == 'symlink': self.bundle.symlink_to(self.prefix / 'outside')
        elif kind == 'fifo': os.mkfifo(self.bundle)
        elif kind == 'hardlink': os.link(self.prefix / 'outside', self.bundle)

    def restore(self):
        if self.bundle.is_dir(): self.bundle.rmdir()
        elif self.bundle.exists() or self.bundle.is_symlink(): self.bundle.unlink()
        self.bundle.write_bytes(b'public bundle')

    def test_nonregular_absent_and_multiple_link_targets_refuse_discovery(self):
        (self.prefix / 'outside').write_bytes(b'private bytes must not be read')
        for kind in ('absent', 'directory', 'symlink', 'fifo', 'hardlink'):
            with self.subTest(kind=kind):
                self.substitute(kind)
                with self.assertRaises(Refusal): self.discover()
                self.restore()

    def test_substitutions_after_discovery_refuse_before_recursive_reads(self):
        (self.prefix / 'outside').write_bytes(b'private bytes must not be read')
        self.discover()
        for kind in ('absent', 'directory', 'symlink', 'fifo', 'hardlink'):
            with self.subTest(kind=kind):
                self.substitute(kind)
                reads = []
                original = os.read
                def read(fd, size):
                    data = original(fd, size); reads.append(data); return data
                with mock.patch('os.read', side_effect=read):
                    with self.assertRaises(Refusal): self.capture()
                self.assertEqual(reads, [])
                self.restore()

    def test_bundle_link_into_allowed_package_refuses_discovery_and_capture(self):
        self.discover()
        internal = self.certifi / 'other'; internal.write_bytes(b'not the public bundle')
        self.bundle.unlink(); self.bundle.symlink_to(internal)
        with self.assertRaises(Refusal): self.discover()
        reads = []
        original = os.read
        def read(fd, size):
            data = original(fd, size); reads.append(data); return data
        with mock.patch('os.read', side_effect=read):
            with self.assertRaises(Refusal): self.capture(self.source)
        self.assertEqual(reads, [])

    def test_serialized_inventory_preserves_constraints_in_observe(self):
        from gate_measurement_inputs import observe, WORKERS
        from gate_measurement_storage import directory_identity
        dependencies = self.discover()
        tool = self.prefix / 'tool'; tool.write_bytes(b'executable')
        target = self.prefix / 'target'; target.mkdir()
        record = json.loads(json.dumps({'version': 1, 'tools': {'rustc': str(tool)},
                 'dependency_roots': {'certifi-first': str(self.package), **dependencies},
                 'external_files': self.files, 'cargo_configs': []}))
        manifest = {'commit': 'a'*40, 'tree': 'b'*40, 'gate': {'argv': ['make', 'test']},
                    'applicable_legs': ['fmt']}
        env = {name: '1' for name in WORKERS}; env['CARGO_TARGET_DIR'] = str(target)
        def capture(versions):
            return observe(manifest, record, env, directory_identity(target), Deadline(30),
                           validate_source=lambda: None, versions=versions,
                           resolve_inventory=lambda: record, limits=lambda: {})
        capture(lambda: {})
        def mutate():
            os.link(self.bundle, self.prefix / 'extra-link')
            return {}
        with self.assertRaises(Refusal): capture(mutate)

    def test_bundle_constraint_applies_before_top_level_bundle_visit(self):
        self.discover()
        audit = {}
        self.capture(audit=audit)
        os.link(self.bundle, self.prefix / 'extra-link')
        with self.assertRaises(Refusal): self.capture(self.bundle, audit)
        with self.assertRaises(Refusal): check_audit(audit, Deadline(30))

    def test_mutation_during_hashing_refuses(self):
        self.discover()
        original = os.read
        mutated = []
        def read(fd, size):
            data = original(fd, size)
            if data and not mutated:
                mutated.append(True)
                self.bundle.write_bytes(b'concurrent change')
            return data
        with mock.patch('os.read', side_effect=read):
            with self.assertRaises(Refusal): self.capture()

    def test_mode_mutation_during_hashing_refuses(self):
        self.discover()
        original = os.read
        changed = []
        def read(fd, size):
            data = original(fd, size)
            if data and not changed:
                changed.append(True)
                self.bundle.chmod(0o600)
            return data
        with mock.patch('os.read', side_effect=read):
            with self.assertRaises(Refusal): self.capture()

    def test_leaf_substitution_between_stat_and_open_refuses_before_read(self):
        self.discover()
        original_open, original_read = os.open, os.read
        substituted, reads = [], []
        def open_file(path, flags, *args, **kwargs):
            if str(path) == 'cert.pem' and not substituted:
                substituted.append(True)
                self.bundle.rename(self.prefix / 'original-bundle')
                self.bundle.write_bytes(b'substituted file')
            return original_open(path, flags, *args, **kwargs)
        def read(fd, size):
            data = original_read(fd, size); reads.append(data); return data
        with mock.patch('os.open', side_effect=open_file), mock.patch('os.read', side_effect=read):
            with self.assertRaises(Refusal): self.capture()
        self.assertTrue(substituted)
        self.assertEqual(reads, [])

    def test_hardlink_created_during_hashing_refuses(self):
        self.discover()
        original = os.read
        mutated = []
        def read(fd, size):
            data = original(fd, size)
            if data and not mutated:
                mutated.append(True)
                os.link(self.bundle, self.prefix / 'extra-link')
            return data
        with mock.patch('os.read', side_effect=read):
            with self.assertRaises(Refusal): self.capture()

    def test_late_byte_mutation_refuses_shared_final_audit(self):
        self.discover()
        audit = {}; self.capture(audit=audit)
        self.bundle.write_bytes(b'late change')
        with self.assertRaises(Refusal): check_audit(audit, Deadline(30))

    def test_ancestor_swap_before_open_never_reads_external_bytes(self):
        self.discover()
        outside = self.prefix / 'outside'; outside.mkdir()
        (outside / 'cert.pem').write_bytes(b'private bytes must not be read')
        opened, read = os.open, os.read
        swapped, reads = [], []
        def open_file(path, flags, *args, **kwargs):
            if str(path) == 'ca-certificates' and not swapped:
                swapped.append(True)
                self.bundle.parent.rename(self.prefix / 'retained')
                self.bundle.parent.symlink_to(outside)
            return opened(path, flags, *args, **kwargs)
        def read_file(fd, size):
            data = read(fd, size); reads.append(data); return data
        with mock.patch('os.open', side_effect=open_file), mock.patch('os.read', side_effect=read_file):
            with self.assertRaises(Refusal): self.capture()
        self.assertTrue(swapped)
        self.assertEqual(reads, [])

    def test_ancestor_swap_during_hashing_refuses_reopened_ancestry(self):
        self.discover()
        original = os.read
        changed = []
        def read(fd, size):
            data = original(fd, size)
            if data and not changed:
                changed.append(True)
                self.bundle.parent.rename(self.prefix / 'retained')
                self.bundle.parent.mkdir()
                self.bundle.write_bytes(b'substituted')
            return data
        with mock.patch('os.read', side_effect=read):
            with self.assertRaises(Refusal): self.capture()


if __name__ == '__main__':
    unittest.main()
