"""SH-835 detached-only recovery and enrollment; entirely private directories."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
def module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value
purge = module('detached_purge', ROOT / 'scripts/purge-detached-products.py')
enrollment = module('enrollment', ROOT / 'plugins/story/lib/product_enrollment.py')

class DetachedProducts(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='sh835-detached-')
        self.root = Path(self.temp.name).resolve()
        self.job = self.root / 'storyhook-detached-products-v1' / 'fixture'
        self.job.mkdir(parents=True, mode=0o700)
        self.products = self.job / 'products'
        self.products.mkdir()
        (self.products / 'old').write_text('old build')
        self.original = self.root / 'live' / 'products'
        self.original.mkdir(parents=True)
        (self.original / 'new').write_text('new build')
        self.journal = self.job / 'journal.json'
        self.record = {'version': 1, 'generation': 1, 'lease': {'worktree_path': str(self.original.parent)},
                       'directory': purge.identity(self.job.stat()), 'product': purge.identity(self.products.stat()),
                       'state': 'detached'}
        self.save()
    def tearDown(self): self.temp.cleanup()
    def save(self):
        self.journal.write_text(json.dumps(self.record))
        self.journal.chmod(0o600)
    def test_detached_purge_preserves_rebuilt_original_and_receipt(self):
        purge.purge(self.journal)
        self.assertFalse(self.products.exists())
        self.assertEqual((self.original / 'new').read_text(), 'new build')
        self.assertEqual(json.loads(self.journal.read_text())['state'], 'purged')
        purge.purge(self.journal)
    def test_crash_after_rename_before_receipt_recovers_exact_detached_inode(self):
        self.record['state'] = 'prepared'; self.save()
        purge.purge(self.journal)
        self.assertFalse(self.products.exists())
        self.assertTrue((self.original / 'new').exists())
    def test_prepared_without_rename_never_inspects_or_deletes_original(self):
        self.record['state'] = 'prepared'; self.save()
        self.products.rename(self.root / 'retained')
        with self.assertRaisesRegex(ValueError, 'not proved'): purge.purge(self.journal)
        self.assertTrue((self.original / 'new').exists())
    def test_interrupted_recursive_purge_is_recoverable(self):
        real = purge.remove_contents
        def interrupted(fd, dev):
            os.unlink('old', dir_fd=fd)
            raise KeyboardInterrupt()
        with patch.object(purge, 'remove_contents', interrupted):
            with self.assertRaises(KeyboardInterrupt): purge.purge(self.journal)
        self.assertEqual(json.loads(self.journal.read_text())['state'], 'purging')
        purge.purge(self.journal)
        self.assertFalse(self.products.exists())
    def test_crash_after_root_removal_before_final_receipt_recovers(self):
        self.record['state'] = 'purging'; self.save()
        (self.products / 'old').unlink(); self.products.rmdir()
        purge.purge(self.journal)
        self.assertEqual(json.loads(self.journal.read_text())['state'], 'purged')
    def test_internal_symlink_never_follows_live_replacement(self):
        (self.products / 'outside').symlink_to(self.original, target_is_directory=True)
        purge.purge(self.journal)
        self.assertTrue((self.original / 'new').exists())
    def test_replaced_detached_root_is_retained(self):
        self.products.rename(self.job / 'retained')
        self.products.mkdir(); (self.products / 'unknown').write_text('preserve')
        with self.assertRaisesRegex(ValueError, 'identity'): purge.purge(self.journal)
        self.assertTrue((self.products / 'unknown').exists())
    def test_swap_between_stat_and_root_open_cannot_redirect_traversal(self):
        actual_open = os.open
        def swap(name, flags, *args, **kwargs):
            if name == 'products' and flags & os.O_DIRECTORY:
                self.products.rename(self.job / 'retained')
                self.products.mkdir(); (self.products / 'unknown').write_text('preserve')
            return actual_open(name, flags, *args, **kwargs)
        with patch.object(purge.os, 'open', swap):
            with self.assertRaisesRegex(ValueError, 'differs from journal'): purge.purge(self.journal)
        self.assertTrue((self.products / 'unknown').exists())
    def test_swapped_root_after_open_cannot_redirect_recursive_deletion(self):
        real_remove = purge.remove_contents
        def swap(fd, dev):
            self.products.rename(self.job / 'retained')
            self.products.mkdir(); (self.products / 'unknown').write_text('preserve')
            real_remove(fd, dev)
        with patch.object(purge, 'remove_contents', swap):
            with self.assertRaisesRegex(ValueError, 'name changed'): purge.purge(self.journal)
        self.assertTrue((self.products / 'unknown').exists())
    def test_symlinked_job_and_hardlinked_journal_are_refused(self):
        old = self.job.with_name('retained'); self.job.rename(old)
        self.job.symlink_to(old, target_is_directory=True)
        with self.assertRaises(OSError): purge.purge(self.journal)
        self.job.unlink(); old.rename(self.job)
        os.link(self.journal, self.job / 'extra-authority')
        with self.assertRaisesRegex(ValueError, 'unsafe detached journal'): purge.purge(self.journal)
    def test_unknown_protocol_and_arbitrary_live_path_are_refused(self):
        self.record['version'] = 2; self.save()
        with self.assertRaises(ValueError): purge.purge(self.journal)
        with self.assertRaises(ValueError): purge.purge(self.original)
        self.assertTrue((self.original / 'new').exists())

class Enrollment(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='sh835-enrollment-')
        self.root = Path(self.temp.name).resolve()
        self.private = self.root / 'git-private'; self.private.mkdir()
        self.config = ('[build_products]\nenabled = true\npath = "products"\n'
                       'managed_entry = "scripts/managed-cargo.sh"\nhook = ["bash", "scripts/purge-detached-products.sh"]\ntimeout_seconds = 30\n')
        (self.root / '.storyhook.toml').write_text(self.config)
        self.marker = self.private / 'storyhook-cleanup-lease-v1.json'
        self.marker.write_text(json.dumps({'version':1, 'worktree_path':str(self.root)})); self.marker.chmod(0o600)
        self.result = type('Result', (), {'stdout':str(self.private)})()
    def tearDown(self): self.temp.cleanup()
    def enroll(self, fresh=True):
        with patch.object(enrollment.subprocess, 'run', return_value=self.result):
            return enrollment.enroll(self.root, fresh)
    def test_fresh_empty_dispatch_enrolls_once_and_receives_contract(self):
        self.assertIn('every build', self.enroll())
        with self.assertRaises(FileExistsError): self.enroll()
        self.assertTrue((self.private / 'storyhook-products-enrollment-v1.json').exists())
    def test_legacy_and_disabled_dispatch_never_enroll(self):
        self.assertEqual(self.enroll(False), '')
        (self.root / '.storyhook.toml').write_text(self.config.replace('true','false'))
        self.assertEqual(self.enroll(), '')
        self.assertFalse((self.private / 'storyhook-products-enrollment-v1.json').exists())
    def test_existing_empty_directory_and_symlink_never_enroll(self):
        target = self.root / 'products'; target.mkdir()
        with self.assertRaisesRegex(ValueError, 'unknown ownership'): self.enroll()
        target.rmdir(); target.symlink_to(self.private, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, 'unknown ownership'): self.enroll()
    def test_external_nested_and_dot_paths_are_refused(self):
        for name in ['/outside', '../products', 'nested/products', '.git']:
            (self.root / '.storyhook.toml').write_text(self.config.replace('path = "products"', 'path = '+json.dumps(name)))
            with self.assertRaises(ValueError): self.enroll()

if __name__ == '__main__': unittest.main()
