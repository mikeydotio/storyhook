"""Installed Python link closure; isolated files, no package installation."""
from pathlib import Path
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from gate_measurement_bounds import Deadline
from gate_measurement_inputs import python_linked_packages, snapshot
from verifier_state import Refusal


class PythonInputs(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='sh872-python-inputs-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.cellar = self.root / 'Cellar'
        self.runtime = self.cellar / 'python@3.14/3.14.7'
        self.runtime.mkdir(parents=True)
        self.library = self.root / 'lib/python3.14/site-packages'
        self.library.mkdir(parents=True)

    def package(self, name, version='1.0'):
        p = self.cellar / name / version
        p.mkdir(parents=True)
        (p / 'module.so').write_bytes(b'installed module')
        return p

    def closure(self, **kwargs):
        return python_linked_packages(self.runtime, [self.library], **kwargs)

    def test_external_installed_package_and_adjacent_bytes_are_fingerprinted(self):
        p = self.package('qscintilla2')
        (self.library / 'module.so').symlink_to(p / 'module.so')
        extra = p / 'adjacent-library'; extra.write_bytes(b'original')
        self.assertEqual(list(self.closure().values()), [str(p)])
        before = snapshot(p, Deadline(30), allowed=(p,))
        extra.write_bytes(b'changed')
        self.assertNotEqual(snapshot(p, Deadline(30), allowed=(p,)), before)

    def test_transitive_package_links_and_back_edges_terminate(self):
        first, second = self.package('first'), self.package('second')
        (self.library / 'first').symlink_to(first)
        (first / 'dependency').symlink_to(second)
        (second / 'back').symlink_to(first)
        self.assertEqual(set(self.closure().values()), {str(first), str(second)})
        snapshot(first, Deadline(30), allowed=(first, second))

    def test_retargeting_an_installed_package_changes_inventory(self):
        first, second = self.package('extension', '1.0'), self.package('extension', '2.0')
        link = self.library / 'extension'; link.symlink_to(first)
        before = self.closure()
        link.unlink(); link.symlink_to(second)
        self.assertNotEqual(before, self.closure())

    def test_other_prefix_and_external_configuration_are_not_adopted(self):
        for target in (self.root / 'private', self.root / 'other/Cellar/extension/1.0'):
            target.mkdir(parents=True)
            link = self.library / 'escape'; link.symlink_to(target)
            with self.assertRaises(Refusal): self.closure()
            link.unlink()

    def test_missing_or_unversioned_package_refuses(self):
        link = self.library / 'extension'
        link.symlink_to(self.cellar / 'missing/1.0/module.so')
        with self.assertRaises(Refusal): self.closure()
        link.unlink()
        p = self.package('extension', 'current')
        link.symlink_to(p)
        with self.assertRaises(Refusal): self.closure()

    def test_entry_and_time_bounds_refuse_without_partial_inventory(self):
        for n in range(5): (self.library / str(n)).write_text('input')
        with self.assertRaises(Refusal): self.closure(entry_limit=3)
        clock = [0]
        deadline = Deadline(1, clock=lambda: clock[0]); clock[0] = 2
        with self.assertRaises(Refusal): self.closure(deadline=deadline)


if __name__ == '__main__':
    unittest.main()
