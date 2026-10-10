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

    def test_purger_importlib_entry_resolves_its_sibling_without_sys_path_changes(self):
        code = ("import importlib.util; "
                "s=importlib.util.spec_from_file_location('isolated_purger'," +
                repr(str(ROOT / 'scripts/purge-detached-products.py')) +
                "); m=importlib.util.module_from_spec(s); s.loader.exec_module(m)")
        result = subprocess.run([sys.executable, '-I', '-B', '-c', code], cwd=self.root,
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_cli_defaults_to_dry_run_without_creating_authority(self):
        repo = self.root / 'repo'; repo.mkdir()
        subprocess.run(['git', 'init', '-q', str(repo)], check=True)
        command = [sys.executable, '-B', str(ROOT / 'scripts/retain-detached-products.py'), 'prune']
        result = subprocess.run(command, cwd=repo, capture_output=True, text=True, check=True)
        self.assertEqual(json.loads(result.stdout)['mode'], 'dry-run')
        self.assertFalse((repo / '.git' / r.NAMESPACE).exists())





class CommonRetention(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='story-common-retention-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.common = self.root / 'common-git'
        self.common.mkdir()
        self.uuid = '01234567-89ab-cdef-0123-456789abcdef'
        self.other_uuid = '11234567-89ab-cdef-0123-456789abcdef'
        self.common_id = r.purge.identity(self.common.stat())
        self.namespace = self.register_project(self.uuid)
        self.token = '0123456789abcdef0123456789abcdef'
        self.owner = dict(version=1, id=self.token, token=self.token, state='finished', executions=[],
                          command=['fixture'], owner=dict(pid=42,start='fixture:1',boot='fixture'),
                          settled_execution=dict(id='fixture',session=43,guard='lease-'+self.token+'.lock'))
        self.original = self.root / 'original'
        self.original.mkdir()
        (self.original / 'evidence').write_text('preserve')

    def write(self, path, value):
        path.write_text(json.dumps(value)); path.chmod(0o600)

    def register_project(self, uuid):
        root = self.common / r.common.ROOT
        root.mkdir(mode=0o700, exist_ok=True)
        project = root / r.common.project_name(uuid)
        project.mkdir(mode=0o700)
        (project / 'retention.lock').touch(mode=0o600)
        self.write(project / 'namespace.json', dict(version=2, project_uuid=uuid,
                   common_git=dict(path=str(self.common),identity=self.common_id),
                   directory=r.purge.identity(project.stat())))
        return project

    def make(self, generation, *, uuid=None, nonce=None, age=10):
        uuid = uuid or self.uuid
        project = self.common / r.common.ROOT / r.common.project_name(uuid)
        nonce = nonce or f'{generation:032x}'
        source = project / ('source-' + nonce)
        lease = dict(version=1,project_slug='fixture',story_id='SH-1',repository_path=str(self.root),
                     worktree_path=str(self.original),branch='fixture',tmux=dict(socket_path=str(self.root/'tmux.sock')))
        if not source.exists():
            source.mkdir(mode=0o700)
            self.write(source / 'source.json', dict(version=2,project_uuid=uuid,nonce=nonce,
                 directory=r.purge.identity(source.stat()),private_git=dict(path=str(self.root/'removed-private'),
                 identity=dict(dev=1,ino=2)),worktree=dict(dev=1,ino=3),lease=lease,
                 config=dict(enabled=True,path='products',managed_entry='scripts/managed-cargo.sh',hook=['fixture'],
                 timeout_seconds=120,retention=dict(mode='dry-run',keep=2,min_age_days=7,runner=['fixture']))))
        job = source / ('generation-' + str(generation)); job.mkdir(mode=0o700)
        product = job / 'products'; (product / 'debug/deps').mkdir(parents=True)
        (product / 'debug/deps/fixture').write_text('reproducible')
        row = dict(version=2,generation=generation,state='detached',lease=lease,
                   directory=r.purge.identity(job.stat()),product=r.purge.identity(product.stat()),
                   retained=dict(namespace=json.loads((project/'namespace.json').read_text()),
                                 source=json.loads((source/'source.json').read_text()),custody=[self.owner]),
                   retention=dict(version=1,policy=r.POLICY,pinned=False,staged_at=NOW-age*r.DAY))
        journal = job / 'journal.json'; self.write(journal,row)
        r.stage(journal)
        return journal

    def plan(self, **kwargs):
        return r.common.prune(r._api(),self.common,self.uuid,self.common_id,now=NOW,**kwargs)

    def all_keep(self, result):
        return all(item['action'] == 'keep' for item in result['jobs'])

    def edit(self, journal, **changes):
        row = json.loads(journal.read_text());row.update(changes);self.write(journal,row)

    def test_project_pool_keeps_two_across_sources_without_reopening_private_git(self):
        jobs = [self.make(n) for n in range(1,5)]
        result = self.plan(apply=True)
        self.assertEqual([j['action'] for j in result['jobs']], ['purged','keep','keep','keep'])
        self.assertTrue(all(j.exists() for j in jobs))
        self.assertEqual((self.original/'evidence').read_text(),'preserve')
        self.assertFalse((self.root/'removed-private').exists())
        self.plan(apply=True)
        self.assertFalse(jobs[1].parent.joinpath('products').exists())
        self.assertTrue(all(j.parent.joinpath('products').exists() for j in jobs[2:]))

    def test_shared_git_projects_are_isolated_even_with_same_generations(self):
        other = self.register_project(self.other_uuid)
        theirs = [self.make(n,uuid=self.other_uuid) for n in range(1,4)]
        ours = [self.make(n) for n in range(1,4)]
        before = {p:p.read_bytes() for p in other.rglob('*') if p.is_file()}
        self.plan(apply=True)
        self.assertEqual(before,{p:p.read_bytes() for p in other.rglob('*') if p.is_file()})
        self.assertFalse(ours[0].parent.joinpath('products').exists())
        self.assertTrue(all(j.parent.joinpath('products').exists() for j in theirs))

    def test_dry_run_changes_no_bytes_and_future_and_young_are_kept(self):
        first=self.make(1,age=-1);self.make(2,age=1);self.make(3)
        before={p:p.read_bytes() for p in self.common.rglob('*') if p.is_file()}
        self.assertTrue(self.all_keep(self.plan(keep=1)))
        self.assertEqual(before,{p:p.read_bytes() for p in self.common.rglob('*') if p.is_file()})

    def test_pin_persists_across_stage_and_native_source_disappearance(self):
        first=self.make(1);self.make(2);self.make(3)
        r.pin(first,True);before=first.read_bytes();r.stage(first)
        self.assertEqual(first.read_bytes(),before)
        self.assertTrue(self.all_keep(self.plan(apply=True)))
        with self.assertRaisesRegex(ValueError,'retention policy'): r.purge.purge(first)
        r.pin(first,False);self.plan(apply=True)
        self.assertFalse(first.parent.joinpath('products').exists())

    def test_pin_after_selection_wins_and_pin_during_purge_refuses(self):
        first=self.make(1);self.make(2);self.make(3)
        real=r.purge.purge
        def race(path,**kw):
            r.pin(path,True)
            return real(path,**kw)
        with patch.object(r.purge,'purge',side_effect=race):
            self.assertTrue(self.all_keep(self.plan(apply=True)))
        r.pin(first,False)
        remove=r.purge.remove_contents
        def overlap(fd,dev):
            with self.assertRaises(BlockingIOError): r.pin(first,True)
            return remove(fd,dev)
        with patch.object(r.purge,'remove_contents',side_effect=overlap):self.plan(apply=True)

    def test_namespace_released_during_purge_but_second_pruner_defers(self):
        first=self.make(1);self.make(2);self.make(3)
        real=r.purge.remove_contents
        observed=[]
        def overlap(fd,dev):
            lock=r.common.NamespaceLock(r._api(),self.namespace);lock.close()
            second=self.plan(apply=True)
            self.assertEqual(second['action'],'retain');observed.append(True)
            (self.original/'new-build').write_text('new')
            return real(fd,dev)
        with patch.object(r.purge,'remove_contents',side_effect=overlap):self.plan(apply=True)
        self.assertTrue(observed)
        self.assertEqual((self.original/'new-build').read_text(),'new')

    def test_native_namespace_publication_lock_defers_inventory(self):
        first=self.make(1);self.make(2);self.make(3)
        lock=r.common.NamespaceLock(r._api(),self.namespace)
        try:
            with self.assertRaises(BlockingIOError):self.plan(apply=True)
        finally:lock.close()
        self.assertTrue(first.parent.joinpath('products').exists())

    def test_incomplete_publication_and_unknown_jobs_block_whole_pool(self):
        first=self.make(1);self.make(2);self.make(3)
        initial=first.read_bytes()
        for state in ('prepared','purging','unknown'):
            self.edit(first,state=state)
            self.assertEqual(self.plan(apply=True)['action'],'retain')
            self.assertTrue(first.parent.joinpath('products').exists())
        first.write_bytes(initial)
        source=first.parent.parent
        unknown=source/'generation-99';unknown.mkdir(mode=0o700)
        self.assertEqual(self.plan(apply=True)['action'],'retain')

    def test_copied_settlement_must_be_complete_and_enrollment_cannot_be_adopted(self):
        first=self.make(1);self.make(2);self.make(3)
        row=json.loads(first.read_text())
        for proof in ({},dict(namespace=row['retained']['namespace'],source=row['retained']['source'],custody=[]),
                      dict(namespace=row['retained']['namespace'],source=row['retained']['source'],custody=[dict(version=1,state='finished')])):
            self.edit(first,retained=proof)
            self.assertEqual(self.plan(apply=True)['action'],'retain')
            with self.assertRaises(ValueError):r.stage(first)
        self.assertTrue(first.parent.joinpath('products').exists())

    def test_incomplete_native_enrollment_schema_preserves_entire_pool(self):
        first=self.make(1);self.make(2);self.make(3)
        manifest=first.parent.parent/'source.json'
        original=json.loads(manifest.read_text());journal=json.loads(first.read_text())
        mutations=[]
        for group in ('config','lease'):
            for field in original[group]:
                row=copy.deepcopy(original);row[group].pop(field);mutations.append(row)
        for group,field,value in (('config','timeout_seconds',True),('config','hook',[]),
                                  ('config','retention',{}),('lease','tmux',{})):
            row=copy.deepcopy(original);row[group][field]=value;mutations.append(row)
        for row in mutations:
            self.write(manifest,row)
            changed=copy.deepcopy(journal);changed['retained']['source']=row;changed['lease']=row['lease']
            self.write(first,changed)
            with self.subTest(row=row):self.assertEqual(self.plan(apply=True)['action'],'retain')
            self.assertTrue(first.parent.joinpath('products').exists())

    def test_source_nonce_or_project_manifest_substitution_refuses(self):
        first=self.make(1);self.make(2);self.make(3)
        manifest=first.parent.parent/'source.json'
        original=manifest.read_bytes();row=json.loads(original);row['nonce']='f'*32;self.write(manifest,row)
        self.assertEqual(self.plan(apply=True)['action'],'retain')
        manifest.write_bytes(original)
        ns=self.namespace/'namespace.json';row=json.loads(ns.read_text());row['project_uuid']=self.other_uuid;self.write(ns,row)
        with self.assertRaises(ValueError):self.plan(apply=True)
        self.assertTrue(first.parent.joinpath('products').exists())

    def test_changed_common_identity_and_symlink_are_never_followed(self):
        first=self.make(1)
        with self.assertRaises(ValueError):r.common.prune(r._api(),self.common,self.uuid,dict(dev=1,ino=1),apply=True)
        saved=self.root/'saved';self.common.rename(saved);self.common.symlink_to(saved,target_is_directory=True)
        with self.assertRaises(OSError):self.plan(apply=True)
        self.assertTrue((saved/r.common.ROOT/self.namespace.name/first.relative_to(self.namespace)).exists())

    def test_partial_purge_stops_future_automatic_recovery(self):
        first=self.make(1);self.make(2);self.make(3)
        def partial(fd,dev):
            os.unlink('debug/deps/fixture',dir_fd=fd);raise OSError('fixture storage fault')
        with patch.object(r.purge,'remove_contents',side_effect=partial):result=self.plan(apply=True)
        self.assertEqual(result['jobs'][0]['action'],'purge-incomplete')
        self.assertEqual(json.loads(first.read_text())['state'],'purging')
        self.assertEqual(self.plan(apply=True)['action'],'retain')

    def test_unknown_release_layout_at_final_authorization_preserves_products(self):
        first=self.make(1);self.make(2);self.make(3)
        (first.parent/'products/release').mkdir()
        self.assertTrue(self.all_keep(self.plan(apply=True)))
        self.assertEqual(json.loads(first.read_text())['state'],'detached')

    def test_dispatch_retention_enrollment_requires_fresh_lane_and_stable_uuid(self):
        spec=importlib.util.spec_from_file_location('enrollment',ROOT/'plugins/story/lib/product_enrollment.py')
        enrollment=importlib.util.module_from_spec(spec);spec.loader.exec_module(enrollment)
        cfg=('uuid = '+json.dumps(self.uuid)+'\n[build_products]\nenabled = true\npath = "products"\n'
             'managed_entry = "scripts/managed-cargo.sh"\nhook = ["fixture"]\ntimeout_seconds = 120\n'
             '[build_products.retention]\nrunner = ["fixture"]\n')
        config=self.original/'.storyhook.toml';config.write_text(cfg)
        private=self.root/'private';private.mkdir()
        self.write(private/'storyhook-cleanup-lease-v1.json',dict(version=1,worktree_path=str(self.original)))
        result=type('Result',(),{'stdout':str(private)})()
        with patch.object(enrollment.subprocess,'run',return_value=result):
            self.assertEqual(enrollment.enroll(self.original,False),'')
            self.assertFalse((private/'storyhook-products-enrollment-v1.json').exists())
            config.write_text(cfg.replace(self.uuid,'unknown'))
            with self.assertRaises(ValueError):enrollment.enroll(self.original,True)
            config.write_text(cfg)
            enrollment.enroll(self.original,True)
            row=json.loads((private/'storyhook-products-enrollment-v1.json').read_text())
            self.assertEqual(row['project_uuid'],self.uuid)
            self.assertRegex(row['nonce'],r'^[0-9a-f]{32}$')
            with self.assertRaises(FileExistsError):enrollment.enroll(self.original,True)

    def test_missing_namespace_cli_default_is_read_only(self):
        command=[sys.executable,'-B',str(ROOT/'scripts/retain-detached-products.py'),'prune-common',
                 '--project-uuid',self.other_uuid,'--common-git',str(self.common),
                 '--common-dev',str(self.common_id['dev']),'--common-ino',str(self.common_id['ino'])]
        result=subprocess.run(command,capture_output=True,text=True,check=True)
        self.assertEqual(json.loads(result.stdout)['mode'],'dry-run')
        self.assertFalse((self.common/r.common.ROOT/r.common.project_name(self.other_uuid)).exists())


if __name__ == '__main__':
    unittest.main()
