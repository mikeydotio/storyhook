"""SH-801/872 input, reservation and target lifecycle checks; no native jobs."""
import copy
from contextlib import nullcontext
import json
import os
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from gate_measurement_bounds import Deadline
from gate_measurement_campaign import begin_window, campaign_environment, competing_work
from gate_measurement_cohorts import Cohort
from gate_measurement_inputs import snapshot, observe, WORKERS, cargo_config_paths
from gate_measurement_targets import TargetPool, remove_exact
from gate_measurement_storage import directory_identity
from gate_measurement_runtime import records
from verifier_state import Refusal
from test_gate_measurement_cohorts import identity


class Fixture(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='sh872-campaign-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()


class Inputs(Fixture):
    def setUp(self):
        super().setUp()
        self.tool = self.root / 'compiler'; self.tool.write_bytes(b'compiler bytes')
        self.deps = self.root / 'deps'; self.deps.mkdir()
        self.library = self.deps / 'lib'; self.library.write_text('dependency bytes')
        self.config = self.root / 'config.toml'
        target = self.root / 'target'; target.mkdir()
        self.target = directory_identity(target)
        self.env = dict({name: '1' for name in WORKERS}, HOME=str(self.root),
                        CARGO_TARGET_DIR=str(target), PATH='/fixture/tools')
        self.plan = {'version': 1, 'tools': {'rustc': str(self.tool)},
                     'dependency_roots': {'deps': str(self.deps)}, 'cargo_configs': [str(self.config)]}
        self.manifest = {'commit': 'a'*40, 'tree': 'b'*40, 'gate': {'argv': ['make', 'test']},
                         'applicable_legs': ['fmt']}

    def capture(self):
        return observe(self.manifest, self.plan, self.env, self.target, Deadline(30),
                       validate_source=lambda: None, versions=lambda: {'rustc': 'fixture version'},
                       resolve_inventory=lambda: copy.deepcopy(self.plan), limits=lambda: {'NOFILE': [1024, 1024]})

    def test_tool_dependency_config_and_unknown_environment_invalidate(self):
        original = self.capture()
        for path in (self.tool, self.library, self.config):
            old = path.read_bytes() if path.exists() else None
            path.write_text('changed input')
            self.assertNotEqual(self.capture(), original)
            if old is None: path.unlink()
            else: path.write_bytes(old)
        self.env['UNKNOWN_COMPILER_INPUT'] = 'changed'
        self.assertNotEqual(self.capture(), original)

    def test_target_substitution_and_changed_inventory_refuse(self):
        target = Path(self.target['path']); target.rename(target.with_name('retained')); target.mkdir()
        with self.assertRaises(Refusal): self.capture()
        with self.assertRaises(Refusal):
            observe(self.manifest, self.plan, self.env, self.target, Deadline(30),
                    validate_source=lambda: None, versions=lambda: {}, resolve_inventory=lambda: {})

    def test_declared_internal_link_is_hashed_but_escape_or_cycle_refuses(self):
        link = self.deps / 'current'; link.symlink_to('lib')
        before = snapshot(self.deps, Deadline(30), allowed=(self.deps,))
        self.library.write_text('changed target')
        self.assertNotEqual(snapshot(self.deps, Deadline(30), allowed=(self.deps,)), before)
        link.unlink(); link.symlink_to(self.tool)
        with self.assertRaises(Refusal): snapshot(self.deps, Deadline(30), allowed=(self.deps,))
        link.unlink(); link.symlink_to(self.deps, target_is_directory=True)
        with self.assertRaises(Refusal): snapshot(self.deps, Deadline(30), allowed=(self.deps,))

    def test_nonregular_or_expired_input_capture_refuses(self):
        pipe = self.root / 'pipe'; os.mkfifo(pipe)
        with self.assertRaises(Refusal): snapshot(pipe, Deadline(30))
        now = [0]; deadline = Deadline(1, clock=lambda: now[0]); now[0] = 1
        with self.assertRaises(Refusal): snapshot(self.tool, deadline)

    def test_identity_contains_hashes_not_environment_secret_values(self):
        self.env['UNEXPECTED_TOKEN'] = 'fixture-private-value'
        raw = json.dumps(self.capture())
        self.assertNotIn('fixture-private-value', raw)
        self.assertNotIn('UNEXPECTED_TOKEN', raw)

    def test_workers_are_explicit_and_configuration_absence_is_recorded(self):
        for value in [None, '0', '9', '-1', 'many']:
            self.env[WORKERS[0]] = value
            with self.assertRaises(Refusal): self.capture()
        paths = cargo_config_paths(self.root, {'HOME': str(self.root)})
        self.assertIn(self.root / '.cargo' / 'config.toml', paths)
        self.assertIn(Path('/.cargo/config'), paths)


class Windows(Fixture):
    def approval(self, revision):
        path = self.root / (revision + '-approval.json')
        path.write_text(json.dumps({'version': 1, 'kind': 'coordinated-measurement-start',
                                   'story': 'SH-872', 'revision': revision,
                                   'campaign_root': str(self.root), 'authority': 'fixture explicit start'}))
        return path

    def test_reservations_are_fixed_and_cannot_restart_or_cross_boot(self):
        auth = self.approval('baseline')
        campaign, window = begin_window(self.root, 'baseline', auth, clock=lambda: 100, boot_id='boot-a')
        self.assertEqual(window['end'], 36100)
        self.assertEqual(campaign['end'], 72100)
        for boot in ('boot-a', 'boot-b'):
            with self.assertRaises(Refusal): begin_window(self.root, 'baseline', auth, clock=lambda: 200, boot_id=boot)
        self.assertEqual(len(records(self.root / 'windows.jsonl')), 1)

    def test_optimization_requires_complete_baseline_and_fits_original_campaign(self):
        begin_window(self.root, 'baseline', self.approval('baseline'), clock=lambda: 100, boot_id='boot-a')
        auth = self.approval('optimization')
        with self.assertRaises(Refusal): begin_window(self.root, 'optimization', auth, clock=lambda: 200, boot_id='boot-a')
        cohort = Cohort(self.root, 'baseline'); value = identity()
        for slot in range(9):
            value['target_identity']['inode'] = slot // 3 + 2
            row = cohort.begin(value, remaining_window=36000, remaining_campaign=72000)
            reuse = row['mode'] == 'reuse'
            cohort.finish(exit_code=0, settled=True, executed=[] if reuse else value['applicable_legs'],
                          reused=value['applicable_legs'] if reuse else [], elapsed=1)
        _, window = begin_window(self.root, 'optimization', auth, clock=lambda: 40000, boot_id='boot-a')
        self.assertEqual(window['end'], 72100)
        self.assertLess(window['end'] - window['started'], 36000)

    def test_start_receipt_is_bound_to_exact_campaign_and_revision(self):
        with self.assertRaises(Refusal): begin_window(self.root, 'optimization', self.approval('baseline'), boot_id='fixture')
        self.assertFalse((self.root / 'campaign.json').exists())

    def test_environment_drops_credentials_authority_and_pins_workers_offline(self):
        env = campaign_environment({'HOME': '/home', 'PATH': '/tools', 'GITHUB_TOKEN': 'secret',
                                    'STORYHOOK_VERIFIER_OWNER': 'foreign', 'CARGO_BUILD_JOBS': '100'})
        self.assertEqual(env['CARGO_BUILD_JOBS'], '1')
        self.assertEqual(env['CARGO_NET_OFFLINE'], 'true')
        self.assertNotIn('GITHUB_TOKEN', env)
        self.assertNotIn('STORYHOOK_VERIFIER_OWNER', env)

    def test_external_build_is_distinct_from_owned_descendants(self):
        raw = '10 1 0.0 python\n11 10 0.0 cargo\n12 11 10.0 rustc\n20 1 2.0 /usr/bin/xcodebuild\n'
        self.assertEqual(competing_work(raw, 10), [20])
        for text in ['bad observation', '20 1 2.0 cargo\n']:
            with self.assertRaises(Refusal): competing_work(text, 10)


class Targets(Fixture):
    def setUp(self):
        super().setUp(); self.pool = TargetPool(self.root)

    def remove(self, target):
        remove_exact(target['path'], target['device'], target['inode'])

    def dispose(self, name, **kwargs):
        return self.pool.dispose(name, settled=kwargs.get('settled', lambda: True),
                                 lease=kwargs.get('lease', nullcontext), remove=kwargs.get('remove', self.remove))

    def test_two_target_bound_and_fresh_names_after_exact_settled_turnover(self):
        first = self.pool.create('baseline-0')
        Path(first['path'], 'artifact').write_text('disposable fixture artifact')
        self.pool.create('baseline-1')
        with self.assertRaises(Refusal): self.pool.create('baseline-2')
        self.dispose('baseline-0')
        self.pool.create('baseline-2')
        with self.assertRaises(Refusal): self.pool.create('baseline-0')
        self.assertEqual(set(self.pool.state()), {'baseline-1', 'baseline-2'})

    def test_unknown_symlink_and_substituted_targets_are_preserved(self):
        target = self.pool.create('baseline-0'); path = Path(target['path'])
        foreign = self.root / 'foreign'; foreign.write_text('keep')
        (path / 'link').symlink_to(foreign)
        with self.assertRaises(Refusal): self.dispose('baseline-0')
        self.assertEqual(foreign.read_text(), 'keep')
        (path / 'link').unlink()
        path.rename(path.with_name('retained')); path.mkdir()
        with self.assertRaises(Refusal): self.dispose('baseline-0')
        self.assertTrue(path.exists())

    def test_unsettled_owner_or_held_product_lease_prevents_any_removal(self):
        from build_products import ProductLease
        target = self.pool.create('baseline-0')
        custody = self.root / 'custody'; custody.mkdir(mode=0o700)
        with ProductLease(custody):
            with self.assertRaises(BlockingIOError):
                self.dispose('baseline-0', lease=lambda: ProductLease(custody, reclaim=True))
        with self.assertRaises(Refusal): self.dispose('baseline-0', settled=lambda: False)
        self.assertTrue(Path(target['path']).is_dir())
        self.assertEqual(len(records(self.pool.path)), 2)

    def test_interrupted_deletion_keeps_pending_lifecycle_and_blocks_admission(self):
        self.pool.create('baseline-0')
        with self.assertRaises(InterruptedError):
            self.dispose('baseline-0', remove=mock.Mock(side_effect=InterruptedError('fixture stop')))
        with self.assertRaises(Refusal): self.pool.create('baseline-1')
        self.assertEqual(records(self.pool.path)[-1]['kind'], 'delete-start')

    def test_target_container_substitution_is_not_adopted(self):
        original = self.root / 'targets'; original.rename(self.root / 'retained'); original.mkdir()
        with self.assertRaises(Refusal): self.pool.create('baseline-0')


class HelperCustody(Fixture):
    def test_nested_helper_passes_all_inherited_lifetime_descriptors(self):
        from gate_measurement_command import execute
        (self.root / 'input').write_text('')
        (self.root / 'request.json').write_text(json.dumps({'argv': ['fixture-command']}))
        with mock.patch('host_admission.supervisor.inherited_descriptors', return_value={12, 9}), \
             mock.patch('gate_measurement_command.subprocess.run', return_value=SimpleNamespace(returncode=0)) as run:
            self.assertEqual(execute(self.root), 0)
        self.assertEqual(run.call_args.kwargs['pass_fds'], (9, 12))


if __name__ == '__main__': unittest.main()
