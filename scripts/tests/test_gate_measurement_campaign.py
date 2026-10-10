"""SH-801/872 input, reservation and target lifecycle checks; no native jobs."""
import copy
from contextlib import nullcontext
import json
import os
from pathlib import Path
import sys
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from gate_measurement_bounds import Deadline
from gate_measurement_campaign import begin_window, campaign_environment, competing_work, owned_resources
from gate_measurement_cohorts import Cohort
from gate_measurement_inputs import snapshot, observe, WORKERS, cargo_config_paths, git_config_origins, supported_compiler_profile, rust_linked_components, python_distribution
from gate_measurement_targets import TargetPool, remove_exact
from gate_measurement_storage import directory_identity
from gate_measurement_runtime import records
import gate_measurement_campaign as campaign_module
from verifier_state import Refusal
from gate_measurement_optional import complete
from test_gate_measurement_cohorts import identity


class Fixture(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='sh872-campaign-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()


class Inputs(Fixture):
    def test_external_transport_fixture_selector_survives_environment_filter(self):
        self.assertEqual(campaign_environment({'STORY_TEST_REVIVIFY_REPO':'/fixture/repository'})[
            'STORY_TEST_REVIVIFY_REPO'], '/fixture/repository')

    def test_python_framework_is_bound_to_its_selected_distribution(self):
        package = self.root / 'Cellar/python@3.14/3.14.7'
        prefix = package / 'Frameworks/Python.framework/Versions/3.14'
        prefix.mkdir(parents=True)
        self.assertEqual(python_distribution(prefix), package)
        ordinary = self.root / 'python'; ordinary.mkdir()
        self.assertEqual(python_distribution(ordinary), ordinary)

    def test_selected_homebrew_llvm_package_is_included_as_input(self):
        cellar = self.root / 'Cellar'
        sysroot = cellar / 'rust/1.98.0'
        component = sysroot / 'lib/rustlib/host/bin/rust-objcopy'
        component.parent.mkdir(parents=True)
        package = cellar / 'llvm@22/22.1.8'
        executable = package / 'bin/llvm-objcopy'
        executable.parent.mkdir(parents=True); executable.write_bytes(b'tool')
        component.symlink_to(executable)
        self.assertEqual(list(rust_linked_components(sysroot).values()), [str(package)])
        before = snapshot(component, Deadline(30), allowed=(sysroot, package))
        executable.write_bytes(b'changed tool')
        self.assertNotEqual(before, snapshot(component, Deadline(30), allowed=(sysroot, package)))
        config = self.root / 'etc/clang'; config.mkdir(parents=True)
        (package / 'etc').mkdir()
        (package / 'etc/clang').symlink_to(config)
        self.assertEqual(rust_linked_components(sysroot)['rust-linked-llvm-clang-config'], str(config))
        (package / 'etc/clang').unlink()
        (package / 'etc/clang').symlink_to(self.root)
        with self.assertRaises(Refusal): rust_linked_components(sysroot)

    def test_unreviewed_rust_component_link_is_not_adopted(self):
        sysroot = self.root / 'Cellar/rust/1.98.0'
        component = sysroot / 'lib/rustlib/host/bin/rust-objcopy'
        component.parent.mkdir(parents=True)
        target = self.root / 'unrelated'; target.write_text('keep')
        component.symlink_to(target)
        with self.assertRaises(Refusal): rust_linked_components(sysroot)
        self.assertEqual(target.read_text(), 'keep')

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

    def test_declared_internal_link_is_hashed_but_escape_or_unresolvable_cycle_refuses(self):
        link = self.deps / 'current'; link.symlink_to('lib')
        before = snapshot(self.deps, Deadline(30), allowed=(self.deps,))
        self.library.write_text('changed target')
        self.assertNotEqual(snapshot(self.deps, Deadline(30), allowed=(self.deps,)), before)
        link.unlink(); link.symlink_to(self.tool)
        with self.assertRaises(Refusal): snapshot(self.deps, Deadline(30), allowed=(self.deps,))
        link.unlink(); link.symlink_to(link)
        with self.assertRaises(Refusal): snapshot(self.deps, Deadline(30), allowed=(self.deps,))

    def test_ancestor_header_alias_hashes_complete_directory_and_edge(self):
        link = self.deps / 'headers'; link.symlink_to(self.deps, target_is_directory=True)
        before = snapshot(self.deps, Deadline(30), allowed=(self.deps,))
        self.library.write_text('changed ancestor content')
        self.assertNotEqual(snapshot(self.deps, Deadline(30), allowed=(self.deps,)), before)
        before = snapshot(self.deps, Deadline(30), allowed=(self.deps,))
        link.unlink(); link.symlink_to('.', target_is_directory=True)
        self.assertNotEqual(snapshot(self.deps, Deadline(30), allowed=(self.deps,)), before)

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

    def test_late_change_to_an_already_hashed_tool_refuses_complete_inventory(self):
        def versions():
            self.tool.write_text('changed after initial byte capture')
            return {'rustc': 'fixture unchanged version'}
        with self.assertRaises(Refusal):
            observe(self.manifest, self.plan, self.env, self.target, Deadline(30),
                    validate_source=lambda: None, versions=versions,
                    resolve_inventory=lambda: copy.deepcopy(self.plan), limits=lambda: {})

    def test_late_change_to_selected_dependency_locations_refuses(self):
        resolutions = iter([copy.deepcopy(self.plan), {}])
        with self.assertRaises(Refusal):
            observe(self.manifest, self.plan, self.env, self.target, Deadline(30),
                    validate_source=lambda: None, versions=lambda: {},
                    resolve_inventory=lambda: next(resolutions), limits=lambda: {})

    def test_git_include_origins_are_discovered_without_config_values(self):
        self.config.write_text('[core]\n bare = false\n')
        raw = f'file:{self.config}\tcore.bare\nfile:{self.config}\tinclude.path\n'
        self.assertEqual(git_config_origins(raw, self.root), [str(self.config)])
        for raw in ['command line:\tcore.bare', 'file:"escaped name"\tcore.bare', 'unreadable']:
            with self.assertRaises(Refusal): git_config_origins(raw, self.root)

    def test_real_local_git_names_only_output_tracks_included_configuration(self):
        include = self.root / 'included'; include.write_text('[core]\n bare = false\n')
        self.config.write_text('[include]\n path = included\n')
        env = {'PATH': '/usr/bin:/bin', 'HOME': str(self.root), 'GIT_CONFIG_NOSYSTEM': '1',
               'GIT_CONFIG_GLOBAL': '/dev/null', 'GIT_CONFIG_SYSTEM': '/dev/null'}
        result = subprocess.run(['/usr/bin/git', 'config', '--file', str(self.config),
                                 '--includes', '--show-origin', '--name-only', '--list'],
                                env=env, capture_output=True, text=True, timeout=5, check=True)
        self.assertEqual(git_config_origins(result.stdout, self.root), sorted([str(self.config), str(include)]))
        self.assertNotIn('false', result.stdout)

    def test_tracked_compiler_wrappers_are_supported_but_unknown_inputs_refuse(self):
        self.config.write_text('[build]\nrustc-wrapper="compiler"\n')
        supported_compiler_profile([self.config], self.root, {}, lambda argv: argv[-1])
        with self.assertRaises(Refusal):
            supported_compiler_profile([self.config], self.root, {'RUSTFLAGS': '@external-response-file'}, lambda _: '')
        for content in ['[env]\nINJECTED="value"\n', '[alias]\nclippy="custom"\n',
                        '[build]\nrustflags=["-L/unknown"]\n']:
            self.config.write_text(content)
            with self.assertRaises(Refusal): supported_compiler_profile([self.config], self.root, {}, lambda _: '')


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
            cohort.finish(telemetry=complete(), exit_code=0, settled=True, executed=[] if reuse else value['applicable_legs'],
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

    def test_resource_totals_include_only_observed_owned_descendants(self):
        raw = '10 1 0.5 20 python\n11 10 50.0 100 rustc\n20 1 99.0 999 xcodebuild\n'
        self.assertEqual(owned_resources(raw, 10), {'cpu_percent_sum': 50.5,
                                                   'rss_bytes_sum': 120 * 1024, 'process_count': 2})
        with self.assertRaises(Refusal): owned_resources('10 1 nan 20 python\n', 10)
        with self.assertRaises(Refusal): owned_resources('20 1 1.0 20 python\n', 10)


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
             mock.patch('gate_measurement_command.subprocess.Popen', return_value=SimpleNamespace(pid=123, wait=lambda:0)) as run:
            self.assertEqual(execute(self.root), 0)
        self.assertEqual(run.call_args.kwargs['pass_fds'], (9, 12))

    def test_private_probe_stop_still_runs_after_admission_deadline(self):
        from gate_measurement_probes import Probes
        probe = Probes.__new__(Probes)
        probe.story, probe.root, probe.project, probe.env = '/fixture/story', self.root, self.root, {}
        with mock.patch.dict(os.environ, {'STORYHOOK_MEASUREMENT_END': '0'}), \
             mock.patch('gate_measurement_probes.bounded', return_value=SimpleNamespace(returncode=0)) as run:
            probe.stop()
        self.assertEqual(run.call_args.args[0], ['/fixture/story', 'daemon', 'stop'])
        self.assertEqual(run.call_args.kwargs['seconds'], 35)
        with mock.patch('gate_measurement_probes.bounded', return_value=SimpleNamespace(returncode=7, stderr='fixture refusal')):
            with self.assertRaises(Refusal): probe.stop()

    def test_class_sample_refuses_changed_inputs_before_starting_a_gate(self):
        import gate_measurement as measurement
        value = {'input_inventory': {}, 'worktree': str(self.root), 'storage': {'targets': [{}]},
                 'pinned_inputs': {'fixture': 'original'}}
        directory = self.root / 'sample'
        with mock.patch.object(measurement, 'observe', return_value={'fixture': 'changed'}), \
             mock.patch.object(measurement, 'require_same_day'):
            with self.assertRaisesRegex(Refusal, 'between matched samples'):
                measurement.run_sample(value, {'day': 'fixture'}, 0, directory, None)
        self.assertFalse(directory.exists())


class Controller(Fixture):
    def setUp(self):
        super().setUp()
        self.revision = self.root / 'baseline'; self.revision.mkdir()
        self.worktree = self.revision / 'worktree'
        self.path = self.revision / 'manifest.json'
        now = campaign_module.time.monotonic()
        self.value = {'version': 1, 'kind': 'gate-throughput-measurement',
                      'campaign_root': str(self.root), 'revision': 'baseline',
                      'source': str(self.root), 'common': str(self.root),
                      'worktree': str(self.worktree), 'commit': 'a'*40, 'tree': 'b'*40,
                      'gate': {'argv': ['make', 'test']}, 'applicable_legs': ['fmt'],
                      'policy': campaign_module.POLICY,
                      'window': {'end': now + 36000}, 'campaign': {'end': now + 72000},
                      'preparation_end': now + 2400}
        self.path.write_text(json.dumps(self.value))
        self.slots = []

    def run_controller(self, fail_slot=None):
        def capture(argv):
            if 'worktree' in argv: self.worktree.mkdir()
            return ''
        def observe(_value, _inputs, _env, target, *_args, **_kwargs):
            result = identity(); result.update(target_identity=target, applicable_legs=['fmt'])
            return result
        def launch(slot, directory, deadline):
            self.slots.append(slot['mode'])
            if slot['slot'] == fail_slot: raise InterruptedError('fixture interruption')
            from test_gate_measurement_execution import progress
            return dict(exit_code=0, settled=True, telemetry=complete(), progress=progress(['fmt'], slot['mode']))
        def bounded(argv, **_kwargs):
            remove_exact(argv[-3], int(argv[-2]), int(argv[-1]))
            return SimpleNamespace(returncode=0, stderr='')
        with mock.patch.dict(os.environ), \
             mock.patch.object(campaign_module, 'capture', side_effect=capture), \
             mock.patch.object(campaign_module, 'validate', return_value=self.value), \
             mock.patch.object(campaign_module, 'inventory', return_value={'version': 1}), \
             mock.patch.object(campaign_module, 'observe', side_effect=observe), \
             mock.patch.object(campaign_module, 'OwnedGate', return_value=launch), \
             mock.patch.object(campaign_module, 'pressure', return_value={'load': [90, 60, 20], 'cores': 10, 'monotonic': 123, 'native_memory_pressure': 1, 'cpu_ticks': [1, 2, 0, 0], 'processes': f'{os.getpid()} 1 0.0 python\n999999 1 800.0 xcodebuild\n', 'resource_processes': f'{os.getpid()} 1 0.0 10 python\n'}), \
             mock.patch.object(campaign_module, 'check_storage', return_value={'free_bytes': 200 * 1024**3}), \
             mock.patch.object(campaign_module, 'paths', return_value=(None, None, self.root / 'owner')), \
             mock.patch('build_products.namespace', return_value=self.root), \
             mock.patch('build_products.ProductLease', return_value=nullcontext()), \
             mock.patch('gate_measurement_command.bounded', side_effect=bounded):
            (self.root / 'owner.owner').write_text(json.dumps({'version': 1, 'gate_started': False, 'gate_session': None}))
            cwd = os.getcwd()
            try: return campaign_module.owned(str(self.path))
            finally: os.chdir(cwd)

    def test_complete_controller_runs_exact_nine_slots_and_settled_target_turnover(self):
        self.assertEqual(self.run_controller(), 0)
        self.assertEqual(self.slots, ['cold', 'warm', 'reuse'] * 3)
        self.assertEqual(TargetPool(self.root).state(), {})
        rows = records(self.root / 'target-lifecycle.jsonl')
        self.assertEqual(sum(r['kind'] == 'created' for r in rows), 3)
        self.assertEqual(sum(r['kind'] == 'deleted' for r in rows), 3)
        self.assertTrue((self.revision / 'complete.json').exists())
        summary = json.loads((self.revision / 'summary.json').read_text())
        self.assertTrue(summary['complete'])
        exposure = records(self.root / 'pressure.jsonl')
        self.assertTrue(all(r['competing_pids'] == [999999] for r in exposure))
        self.assertTrue(all(r['load'][0] == 90 for r in exposure))
        self.assertIn('inconclusive', summary['inference'])
        self.assertEqual(summary['modes']['reuse']['accepted'], 3)
        self.assertEqual(summary['observed_peak_resources']['rss_bytes_sum'], 10240)

    def test_interrupted_warm_stops_before_reuse_and_retains_target(self):
        with self.assertRaises(InterruptedError): self.run_controller(fail_slot=1)
        self.assertEqual(self.slots, ['cold', 'warm'])
        self.assertEqual(set(TargetPool(self.root).state()), {'baseline-0'})
        self.assertEqual(Cohort(self.root, 'baseline').history()[1]['slot'], 1)
        self.assertFalse((self.revision / 'complete.json').exists())
        summary = json.loads((self.revision / 'summary.json').read_text())
        self.assertFalse(summary['complete'])
        self.assertEqual(summary['pending_slot'], 1)


if __name__ == '__main__': unittest.main()
