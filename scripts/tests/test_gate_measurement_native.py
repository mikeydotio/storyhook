"""SH-872 native ownership and target turnover, using only disposable fixtures."""
import json
import ctypes
import errno
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.dont_write_bytecode = True
SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
from gate_measurement_command import bounded
from gate_measurement_targets import TargetPool
from build_products import ProductLease
from host_admission import native


class NativeObservation(unittest.TestCase):
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


@unittest.skipUnless(sys.platform == 'darwin', 'native macOS custody')
class NativeIntegration(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='sh872-native-', dir='/tmp')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()

    def test_bounded_child_inherits_product_exclusion_until_settlement(self):
        custody = self.root / 'products'; custody.mkdir(mode=0o700)
        probe = """import fcntl, os, sys
actual = os.fstat(int(sys.argv[2]))
expected = os.stat(sys.argv[1])
assert (actual.st_dev, actual.st_ino) == (expected.st_dev, expected.st_ino)
with open(sys.argv[1], 'a') as lock:
    try: fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError: print('excluded')
    else: raise SystemExit('inherited product exclusion was lost')
"""
        with ProductLease(custody, reclaim=True) as lease:
            result = bounded([sys.executable, '-B', '-c', probe, str(custody / 'products.lock'), str(lease.fd)],
                             root=self.root / 'operations', seconds=30)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.strip(), 'excluded')
        with ProductLease(custody, reclaim=True):
            pass

    def test_exact_target_turnover_through_real_lease_and_bounded_child(self):
        campaign = self.root / 'campaign'; campaign.mkdir(mode=0o700)
        pool = TargetPool(campaign)
        first = pool.create('baseline-0')
        Path(first['path'], 'owned-product').write_bytes(b'disposable fixture')
        second = pool.create('baseline-1')
        custody = self.root / 'products'; custody.mkdir(mode=0o700)

        def remove(target):
            result = bounded([sys.executable, '-B', str(SCRIPTS / 'gate_measurement_targets.py'),
                              'remove', target['path'], str(target['device']), str(target['inode'])],
                             root=self.root / 'operations', seconds=30)
            self.assertEqual(result.returncode, 0, result.stderr)

        pool.dispose('baseline-0', settled=lambda: True,
                     lease=lambda: ProductLease(custody, reclaim=True), remove=remove)
        self.assertEqual(pool.state(), {'baseline-1': second})
        self.assertFalse(Path(first['path']).exists())
        third = pool.create('baseline-2')
        self.assertNotEqual(first['path'], third['path'])

    def test_real_owned_cold_warm_reuse_preserves_production_receipts(self):
        source = self.root / 'source'; source.mkdir()
        shutil.copytree(SCRIPTS, source / 'scripts', ignore=shutil.ignore_patterns('__pycache__'))
        campaign = self.root / 'campaign'; campaign.mkdir(mode=0o700)
        revision = campaign / 'baseline'; revision.mkdir(mode=0o700)
        worktree = revision / 'worktree'
        env = {k: v for k, v in os.environ.items() if not k.startswith(('STORYHOOK_', 'GIT_'))}
        env.update(GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL='/dev/null',
                   GIT_AUTHOR_NAME='Fixture', GIT_AUTHOR_EMAIL='fixture@example.invalid',
                   GIT_COMMITTER_NAME='Fixture', GIT_COMMITTER_EMAIL='fixture@example.invalid',
                   STORYHOOK_PYTHON=sys.executable, PYTHONDONTWRITEBYTECODE='1')

        def git(*args):
            return subprocess.check_output(['git', '-C', str(source), *args], env=env, text=True).strip()

        git('init', '-q'); git('add', '.'); git('commit', '-qm', 'native fixture')
        commit, tree = git('rev-parse', 'HEAD'), git('rev-parse', 'HEAD^{tree}')
        git('worktree', 'add', '-q', '--detach', str(worktree), commit)
        target = campaign / 'target'; target.mkdir()
        argv = ['bash', 'scripts/leg.sh', '--reuse', 'fixture', '--', 'sh', '-c',
                'printf x >> "$CARGO_TARGET_DIR/executions"']
        manifest = revision / 'manifest.json'
        manifest.write_text(json.dumps(dict(version=1, kind='gate-throughput-measurement',
            campaign_root=str(campaign), revision='baseline', worktree=str(worktree),
            common=str(source / '.git'), commit=commit, tree=tree, gate={'argv': argv})))
        env.update(STORYHOOK_GATE_MEASUREMENT=str(manifest), CARGO_TARGET_DIR=str(target),
                   STORYHOOK_GATE_PROGRESS=str(campaign / 'progress.jsonl'))
        Path(env['STORYHOOK_GATE_PROGRESS']).touch()
        body = f"""
import json, sys
from pathlib import Path
sys.path.insert(0, {str(SCRIPTS)!r})
sys.path.insert(0, {str(SCRIPTS / 'tests')!r})
from test_gate_measurement_cohorts import identity
from gate_measurement_cohorts import Cohort
from gate_measurement_execution import OwnedGate, run_observation
from gate_measurement_storage import directory_identity
value=identity()
value.update(source_commit={commit!r},source_tree={tree!r},gate_argv={argv!r},
             applicable_legs=['fixture'],target_identity=directory_identity({str(target)!r}))
cohort=Cohort({str(campaign)!r}, 'baseline')
for _ in range(3):
    result=run_observation(cohort, observe=lambda: value,
        launch=OwnedGate({str(manifest)!r}, {argv!r}, health=lambda: None),
        remaining_window=36000,remaining_campaign=72000)
    assert result['accepted'], result
print('accepted cold warm reuse')
"""
        result = bounded([sys.executable, '-B', str(SCRIPTS / 'verifier-owner.py'),
                          'measurement-run', str(source / '.git'), str(worktree), '--',
                          sys.executable, '-B', '-c', body],
                         root=self.root / 'operations', seconds=120, cwd=worktree, env=env)
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        self.assertEqual((target / 'executions').read_text(), 'xx')
        self.assertIn('accepted cold warm reuse', result.stdout)
        self.assertFalse((source / '.git/storyhook/gate-leg-receipts').exists())
        self.assertFalse((source / '.git/storyhook/gate-receipts').exists())


if __name__ == '__main__':
    unittest.main()
