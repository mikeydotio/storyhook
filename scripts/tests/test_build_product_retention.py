"""Focused retention regressions; synthetic products and real file locks only."""
import fcntl
import contextlib
import io
import copy
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts'))
spec = importlib.util.spec_from_file_location('retention', ROOT / 'scripts/retain-detached-products.py')
r = importlib.util.module_from_spec(spec)
spec.loader.exec_module(r)
NOW = 2000000000


class Retention(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='story-retention-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.private = self.root / 'private-git'
        self.private.mkdir(mode=0o700)
        self.namespace = self.private / r.NAMESPACE
        self.namespace.mkdir(mode=0o700)
        self.custody = self.private / 'storyhook-build-products-v1'
        self.custody.mkdir(mode=0o700)
        self.lock = self.custody / 'products.lock'
        self.lock.touch(mode=0o600)
        self.token = '0123456789abcdef0123456789abcdef'
        self.owner = self.custody / ('build-' + self.token)
        self.owner.mkdir(mode=0o700)
        self.owner_record = dict(version=1, id=self.token, token=self.token, state='finished',
                                 executions=[], command=['fixture'],
                                 owner={'pid': 42, 'start': 'fixture:1', 'boot': 'fixture'},
                                 settled_execution={'id': 'fixture', 'session': 43,
                                                    'guard': 'lease-' + self.token + '.lock'})
        self.write(self.owner / 'record.json', self.owner_record)
        self.original = self.root / 'current-target'
        self.original.mkdir()
        for name in ('source', 'release', 'frozen-input', 'log', 'receipt'):
            (self.original / name).write_text('preserve ' + name)

    def make(self, generation, age=10, stage=True):
        job = self.namespace / ('generation-' + str(generation))
        job.mkdir(mode=0o700)
        product = job / 'products'
        (product / 'debug/deps').mkdir(parents=True)
        (product / 'debug/deps/test-binary').write_text('reproducible')
        row = dict(version=1, generation=generation, state='detached',
                   directory=r.purge.identity(job.stat()), product=r.purge.identity(product.stat()),
                   lease={'worktree_path': str(self.original)})
        journal = job / 'journal.json'
        self.write(journal, row)
        if stage:
            r.stage(journal, now=NOW-age*r.DAY)
        return journal

    def write(self, path, row):
        path.write_text(json.dumps(row)); path.chmod(0o600)

    def edit(self, path, **values):
        row = json.loads(path.read_text()); row.update(values); self.write(path, row)

    def plan(self, **kw):
        return r.prune(self.private, now=NOW, **kw)

    def actions(self, result):
        return {j['job']: j['action'] for j in result['jobs']}

    def test_dry_run_is_default_and_preserves_every_byte(self):
        jobs = [self.make(n) for n in range(1, 5)]
        before = {p: p.read_bytes() for p in self.root.rglob('*') if p.is_file()}
        result = self.plan()
        self.assertEqual(result['mode'], 'dry-run')
        self.assertEqual(list(self.actions(result).values()), ['would-purge', 'would-purge', 'keep', 'keep'])
        self.assertEqual(before, {p: p.read_bytes() for p in self.root.rglob('*') if p.is_file()})
        self.assertTrue(all(p.parent.joinpath('products').exists() for p in jobs))

    def test_apply_keeps_newest_and_receipts_and_rebuilt_original(self):
        jobs = [self.make(n) for n in range(1, 5)]
        result = self.plan(apply=True)
        self.assertEqual(list(self.actions(result).values()), ['purged', 'keep', 'keep', 'keep'])
        self.assertEqual(json.loads(jobs[0].read_text())['state'], 'purged')
        self.assertTrue(all(p.exists() for p in jobs))
        for name in ('source', 'release', 'frozen-input', 'log', 'receipt'):
            self.assertEqual((self.original / name).read_text(), 'preserve ' + name)
        second = self.plan(apply=True)
        self.assertEqual(second['jobs'][0]['reason'], 'already purged; receipt retained')
        self.assertEqual(self.actions(second)['generation-2'], 'purged')

    def test_young_and_future_timestamps_are_retained(self):
        self.make(1, age=1); self.make(2, age=-1); self.make(3)
        self.assertTrue(all(v == 'keep' for v in self.actions(self.plan(keep=1)).values()))

    def test_boundary_age_and_generation_order_not_mtime(self):
        first = self.make(1, age=7); self.make(9); self.make(10)
        os.utime(first, (NOW + r.DAY, NOW + r.DAY))
        self.assertEqual(self.actions(self.plan())['generation-1'], 'would-purge')

    def test_pins_survive_stage_retry_and_both_purge_entry_points(self):
        first = self.make(1); self.make(2); self.make(3)
        r.pin(first, True); old = first.read_bytes()
        r.stage(first, now=NOW)
        self.assertEqual(old, first.read_bytes())
        self.assertEqual(self.actions(self.plan(apply=True))['generation-1'], 'keep')
        with self.assertRaisesRegex(ValueError, 'retention policy'):
            r.purge.purge(first)
        r.pin(first, False)
        self.assertEqual(self.actions(self.plan(apply=True))['generation-1'], 'purged')

    def test_legacy_unenrolled_job_stops_whole_inventory(self):
        self.make(1); self.make(2); self.make(3, stage=False)
        self.assertTrue(all(v == 'keep' for v in self.actions(self.plan(apply=True)).values()))

    def test_locked_job_stops_inventory(self):
        first = self.make(1); self.make(2); self.make(3)
        with r.job(first):
            self.assertTrue(all(v == 'keep' for v in self.actions(self.plan(apply=True)).values()))

    def test_active_build_lock_defers_apply(self):
        first = self.make(1); self.make(2); self.make(3)
        with r.ProductLease(self.custody):
            with self.assertRaises(BlockingIOError): self.plan(apply=True)
        self.assertTrue(first.parent.joinpath('products').exists())

    def test_unfinished_custody_defers_even_without_owner_process(self):
        first = self.make(1); self.make(2); self.make(3)
        owner = self.custody / 'build-fixture'; owner.mkdir(mode=0o700)
        self.write(owner / 'record.json', dict(version=1, state='running', executions=[]))
        with self.assertRaisesRegex(RuntimeError, 'unsettled'): self.plan(apply=True)
        self.assertTrue(first.parent.joinpath('products').exists())

    def test_missing_custody_is_not_created_by_apply(self):
        self.make(1); self.make(2); self.make(3)
        self.lock.unlink(); (self.owner / 'record.json').unlink(); self.owner.rmdir(); self.custody.rmdir()
        with self.assertRaisesRegex(ValueError, 'no managed'): self.plan(apply=True)
        self.assertFalse(self.custody.exists())

    def test_malformed_record_and_unknown_state_preserve_all(self):
        first = self.make(1); self.make(2); self.make(3)
        for raw in ('{', '{"version": 9}', 'null'):
            first.write_text(raw)
            self.assertTrue(all(v == 'keep' for v in self.actions(self.plan(apply=True)).values()))

    def test_unknown_entry_preserves_valid_jobs(self):
        self.make(1); self.make(2); self.make(3)
        (self.namespace / 'unknown').write_text('preserve')
        self.assertTrue(all(v == 'keep' for v in self.actions(self.plan(apply=True)).values()))

    def test_missing_namespace_dry_run_creates_nothing(self):
        self.namespace.rmdir()
        self.assertEqual(self.plan()['jobs'], [])
        self.assertFalse(self.namespace.exists())

    def test_unsafe_retention_parameters_refuse(self):
        for kw in ({'keep': 0}, {'keep': -1}, {'min_age_days': 0}, {'min_age_days': float('nan')}, {'min_age_days': float('inf')}):
            with self.subTest(kw=kw), self.assertRaises(ValueError): self.plan(**kw)

    def test_symlinked_namespace_and_job_are_not_followed(self):
        first = self.make(1); self.make(2); self.make(3)
        held = self.root / 'held'; first.parent.rename(held)
        first.parent.symlink_to(held, target_is_directory=True)
        self.assertTrue(all(v == 'keep' for v in self.actions(self.plan(apply=True)).values()))
        self.namespace.rename(self.root / 'saved-namespace')
        self.namespace.symlink_to(self.root / 'saved-namespace', target_is_directory=True)
        with self.assertRaises(OSError): self.plan(apply=True)
        self.assertTrue((held / 'products').exists())

    def test_symlinked_product_and_hardlinked_journal_refuse(self):
        first = self.make(1); self.make(2); self.make(3)
        products = first.parent / 'products'; products.rename(first.parent / 'saved')
        products.symlink_to(self.original, target_is_directory=True)
        self.assertTrue(all(v == 'keep' for v in self.actions(self.plan(apply=True)).values()))
        products.unlink(); (first.parent / 'saved').rename(products)
        os.link(first, first.parent / 'alias.json')
        self.assertTrue(all(v == 'keep' for v in self.actions(self.plan(apply=True)).values()))

    def test_product_replacement_refuses(self):
        first = self.make(1); self.make(2); self.make(3)
        p = first.parent / 'products'; p.rename(first.parent / 'old'); p.mkdir()
        (p / 'unknown').write_text('keep')
        self.assertTrue(all(v == 'keep' for v in self.actions(self.plan(apply=True)).values()))
        self.assertEqual((p / 'unknown').read_text(), 'keep')

    def test_release_and_evidence_layouts_cannot_be_enrolled(self):
        for n, extra in enumerate(('release', 'logs', 'receipts', 'debug/real-fixtures'), 1):
            first = self.make(n, stage=False)
            (first.parent / 'products' / extra).mkdir()
            with self.subTest(extra=extra), self.assertRaises(ValueError): r.stage(first, now=NOW)
            self.assertNotIn('retention', json.loads(first.read_text()))

    def test_prepared_and_purging_are_never_automatically_recovered(self):
        first = self.make(1)
        for state in ('prepared', 'purging', 'unknown'):
            self.edit(first, state=state)
            with self.assertRaises(ValueError): r.stage(first, now=NOW)
            self.assertEqual(self.actions(self.plan(apply=True))['generation-1'], 'keep')

    def test_retention_clock_is_not_refreshed_by_repeated_stage(self):
        first = self.make(1); original = json.loads(first.read_text())['retention']['staged_at']
        r.stage(first, now=NOW)
        self.assertEqual(json.loads(first.read_text())['retention']['staged_at'], original)

    def test_pin_between_plan_and_purge_refuses_stale_authority(self):
        first = self.make(1); self.make(2); self.make(3)
        real = r.purge.purge
        def race(path, **kw):
            r.pin(path, True)
            return real(path, **kw)
        with patch.object(r.purge, 'purge', side_effect=race): result = self.plan(apply=True)
        self.assertEqual(self.actions(result)['generation-1'], 'keep')
        self.assertTrue(first.parent.joinpath('products').exists())

    def test_new_inventory_entry_before_exclusion_aborts_apply(self):
        self.make(1); self.make(2); self.make(3)
        real = r.ProductLease
        def race(*a, **kw):
            self.make(4)
            return real(*a, **kw)
        with patch.object(r, 'ProductLease', side_effect=race), self.assertRaisesRegex(ValueError, 'namespace changed'):
            self.plan(apply=True)

    def test_new_release_output_after_preview_refuses_under_purge_lock(self):
        first = self.make(1); self.make(2); self.make(3)
        real = r.purge.purge
        def race(path, **kw):
            (path.parent / 'products/release').mkdir()
            return real(path, **kw)
        with patch.object(r.purge, 'purge', side_effect=race): result = self.plan(apply=True)
        self.assertEqual(self.actions(result)['generation-1'], 'keep')
        self.assertTrue(first.parent.joinpath('products/release').exists())

    def test_interrupted_purge_retains_receipt_and_requires_manual_recovery(self):
        first = self.make(1); self.make(2); self.make(3)
        with patch.object(r.purge, 'remove_contents', side_effect=KeyboardInterrupt), self.assertRaises(KeyboardInterrupt):
            self.plan(apply=True)
        self.assertEqual(json.loads(first.read_text())['state'], 'purging')
        self.assertTrue(all(v == 'keep' for v in self.actions(self.plan(apply=True)).values()))

    def test_stale_missing_product_cannot_publish_a_completion_receipt(self):
        first = self.make(1); self.make(2); self.make(3)
        real = r.purge.purge
        changed = []
        def race(path, **kw):
            (path.parent / 'products').rename(path.parent / 'retained-elsewhere')
            self.edit(path, state='purging')
            changed.append(path.read_bytes())
            return real(path, **kw)
        with patch.object(r.purge, 'purge', side_effect=race): result = self.plan(apply=True)
        self.assertEqual(self.actions(result)['generation-1'], 'purge-incomplete')
        self.assertEqual(first.read_bytes(), changed[0])

    def test_plain_purge_legacy_contract_unchanged(self):
        first = self.make(1, stage=False)
        r.purge.purge(first)
        self.assertEqual(json.loads(first.read_text())['state'], 'purged')
        self.assertTrue((self.original / 'frozen-input').exists())

    def test_rebuild_can_start_during_detached_purge(self):
        first = self.make(1); self.make(2); self.make(3)
        real = r.purge.remove_contents
        observed = []
        def overlap(fd, dev):
            with r.ProductLease(self.custody):
                observed.append(True)
                (self.original / 'rebuilt').write_text('current build')
            # Another pruner sees the held job lock and retains everything.
            self.assertTrue(all(v == 'keep' for v in self.actions(self.plan(apply=True)).values()))
            return real(fd, dev)
        with patch.object(r.purge, 'remove_contents', side_effect=overlap): self.plan(apply=True)
        self.assertTrue(observed)
        self.assertEqual((self.original / 'rebuilt').read_text(), 'current build')
        self.assertFalse(first.parent.joinpath('products').exists())

    def test_pin_while_purging_cannot_report_success(self):
        first = self.make(1); self.make(2); self.make(3)
        real = r.purge.remove_contents
        def overlap(fd, dev):
            with self.assertRaises(BlockingIOError): r.pin(first, True)
            return real(fd, dev)
        with patch.object(r.purge, 'remove_contents', side_effect=overlap): self.plan(apply=True)

    def test_purged_receipt_cannot_authorize_reappeared_products(self):
        first = self.make(1); self.make(2); self.make(3)
        self.plan(apply=True)
        (first.parent / 'products').mkdir()
        (first.parent / 'products/unknown').write_text('preserve')
        self.assertTrue(all(v == 'keep' for v in self.actions(self.plan(apply=True)).values()))
        self.assertEqual((first.parent / 'products/unknown').read_text(), 'preserve')

    def test_incomplete_finished_custody_never_authorizes_deletion(self):
        first = self.make(1); self.make(2); self.make(3)
        mutations = [dict(version=1, state='finished')]
        for field in ('version', 'id', 'token', 'executions', 'command', 'owner', 'settled_execution'):
            row = copy.deepcopy(self.owner_record); row.pop(field); mutations.append(row)
        for field, value in (('id', 'different'), ('token', 'different'), ('command', []),
                             ('command', [5]), ('executions', None), ('owner', []),
                             ('settled_execution', []), ('version', True)):
            row = copy.deepcopy(self.owner_record); row[field] = value; mutations.append(row)
        for group, field, value in (('owner', 'pid', True), ('owner', 'pid', 0),
                                    ('owner', 'start', ''), ('owner', 'boot', ''),
                                    ('settled_execution', 'id', ''),
                                    ('settled_execution', 'guard', 'wrong.lock'),
                                    ('settled_execution', 'session', '43')):
            row = copy.deepcopy(self.owner_record); row[group][field] = value; mutations.append(row)
        for row in mutations:
            self.write(self.owner / 'record.json', row)
            with self.subTest(row=row), self.assertRaises((ValueError, RuntimeError)):
                self.plan(apply=True)
            self.assertTrue(first.parent.joinpath('products/debug/deps/test-binary').exists())

    def test_no_owner_records_and_invalid_token_defer(self):
        first = self.make(1); self.make(2); self.make(3)
        (self.owner / 'record.json').unlink(); self.owner.rmdir()
        with self.assertRaisesRegex(ValueError, 'no completed managed'): self.plan(apply=True)
        self.owner = self.custody / 'build-malformed'; self.owner.mkdir(mode=0o700)
        self.write(self.owner / 'record.json', dict(version=1, state='finished', executions=[]))
        with self.assertRaisesRegex(ValueError, 'unknown product'): self.plan(apply=True)
        self.assertTrue(first.parent.joinpath('products').exists())

    def test_partial_io_failure_reports_incomplete_and_unsuccessful_cli(self):
        first = self.make(1); self.make(2); self.make(3)
        def partial(fd, dev):
            os.unlink('debug/deps/test-binary', dir_fd=fd)
            raise OSError('fixture storage failure after unlink')
        output = io.StringIO()
        with patch.object(r.purge, 'remove_contents', side_effect=partial), \
                patch.object(r, 'git_path', return_value=self.private), \
                patch.object(r.time, 'time', return_value=NOW), contextlib.redirect_stdout(output):
            result = r.main(['prune', '--apply'])
        self.assertEqual(result, 1)
        report = json.loads(output.getvalue())
        item = report['jobs'][0]
        self.assertEqual(item['action'], 'purge-incomplete')
        self.assertEqual(item['journal_state'], 'purging')
        self.assertIn('bytes may be missing', item['recovery'])
        self.assertFalse(first.parent.joinpath('products/debug/deps/test-binary').exists())
        self.assertEqual(json.loads(first.read_text())['state'], 'purging')
        self.assertTrue(all(v == 'keep' for v in self.actions(self.plan(apply=True)).values()))

    def test_cli_defaults_to_dry_run_without_creating_authority(self):
        repo = self.root / 'repo'; repo.mkdir()
        subprocess.run(['git', 'init', '-q', str(repo)], check=True)
        command = [sys.executable, '-B', str(ROOT / 'scripts/retain-detached-products.py'), 'prune']
        result = subprocess.run(command, cwd=repo, capture_output=True, text=True, check=True)
        self.assertEqual(json.loads(result.stdout)['mode'], 'dry-run')
        self.assertFalse((repo / '.git' / r.NAMESPACE).exists())


if __name__ == '__main__':
    unittest.main()
