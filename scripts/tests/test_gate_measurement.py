#!/usr/bin/env python3
"""SH-801: scheduling measurements must prove what ran, not cached success."""

import copy
import json
import os
from pathlib import Path
import resource
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from gate_measurement_data import IdleWindow, hook_context, parse_wall, schedule, summarize
from verifier_state import Refusal
import gate_measurement_context as context
import gate_measurement_runtime as runtime
import gate_measurement_setup as setup
import gate_measurement as collector
from gate_measurement_probes import Probes
sys.path.insert(0, str(Path(__file__).resolve().parent))
import load_grace

SCRIPTS = Path(__file__).resolve().parents[1]


def cohort():
    """Provide explicit evidence for twenty independently identified samples."""
    manifest = {"version": 1, "tree": "a" * 40, "day": "2026-10-03", "pairs": 10}
    records = []
    for index in range(20):
        records.append({"kind": "sample", "index": index,
                        "condition": "control" if index % 2 == 0 else "utility",
                        "tree": manifest["tree"], "day": manifest["day"],
                        "wall_seconds": index + 1.0, "exit_code": 0,
                        "cleanup": "complete", "valid": True, "reasons": [],
                        "probes": [{"name": name, "seconds": seconds,
                                    "ok": True, "overlap": True}
                                   for name, seconds in [("list", .1), ("hook", .2)]]})
    return manifest, records


class Evidence(unittest.TestCase):
    """Incomplete, duplicated or contaminated evidence never claims completion."""

    def test_schedule_and_statistics(self):
        manifest, rows = cohort()
        self.assertEqual(schedule(), ["control", "utility"] * 10)
        result = summarize(manifest, rows)
        self.assertTrue(result["complete"])
        self.assertEqual(result["control"]["gate"], {"median": 10, "min": 1, "max": 19})
        self.assertEqual(result["utility"]["gate"], {"median": 11, "min": 2, "max": 20})
        self.assertAlmostEqual(result["median_change_percent"], 10)
        self.assertEqual(result["control"]["probes"], {"list": .1, "hook": .2})

    def test_warmups_do_not_count(self):
        manifest, rows = cohort()
        warm = dict(rows[0], kind="warmup", wall_seconds=1000)
        self.assertEqual(summarize(manifest, [warm, *rows]), summarize(manifest, rows))
        self.assertFalse(summarize(manifest, [warm])["complete"])

    def test_failure_is_reported_and_cannot_be_replaced_by_retry(self):
        manifest, rows = cohort()
        rows[4]["exit_code"] = 1
        result = summarize(manifest, rows)
        self.assertFalse(result["complete"])
        self.assertEqual(result["control"]["failed"], 1)
        self.assertEqual(result["control"]["attempted"], 10)
        with self.assertRaisesRegex(Refusal, "duplicate"):
            summarize(manifest, [*rows, dict(rows[4], exit_code=0)])

    def test_missing_or_invalid_evidence_is_not_a_success(self):
        manifest, rows = cohort()
        cases = [("tree", "b" * 40), ("day", "2026-10-04"),
                 ("condition", "utility"), ("wall_seconds", float("nan")),
                 ("wall_seconds", -1), ("index", True)]
        for key, value in cases:
            with self.subTest(key=key, value=value):
                changed = copy.deepcopy(rows)
                changed[0][key] = value
                with self.assertRaises(Refusal):
                    summarize(manifest, changed)
        self.assertFalse(summarize(manifest, rows[:-1])["complete"])
        for key, value in [("cleanup", "unknown"), ("valid", False)]:
            changed = copy.deepcopy(rows)
            changed[0][key] = value
            self.assertFalse(summarize(manifest, changed)["complete"])
        for alteration in [[], [{"name": "list", "seconds": .1, "ok": True, "overlap": False}]]:
            changed = copy.deepcopy(rows)
            changed[0]["probes"] = alteration
            self.assertFalse(summarize(manifest, changed)["complete"])

    def test_time_parser_is_strict(self):
        self.assertEqual(parse_wall("real 12.25\nuser 1.0\nsys 2.0\n"), 12.25)
        for raw in ["", "real nan", "real inf", "real -1", "real 1\nreal 2", "real 1oops"]:
            with self.subTest(raw=raw), self.assertRaises(Refusal):
                parse_wall(raw)

    def test_started_gate_without_exit_stays_in_attempt_denominator(self):
        manifest, _ = cohort()
        result = summarize(manifest, [{'kind': 'start', 'index': 0}])
        self.assertFalse(result['complete'])
        self.assertEqual(result['control']['attempted'], 1)
        self.assertEqual(result['control']['unresolved'], 1)
        self.assertEqual(result['control']['failed'], 0)

    def test_degraded_hook_is_not_fast_success(self):
        good = {"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": "1 story"}}
        self.assertEqual(hook_context(json.dumps(good)), "1 story")
        for raw in ["{}", "", "not JSON", "null", '"text"',
                    json.dumps({"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": ""}})]:
            with self.subTest(raw=raw), self.assertRaises(Refusal):
                hook_context(raw)

    def test_idle_window_resets_after_pressure_or_unknown_sensor(self):
        idle = IdleWindow()
        self.assertFalse(idle.observe(0, 4.9, 10))
        self.assertFalse(idle.observe(59, 4.9, 10))
        self.assertTrue(idle.observe(60, 4.9, 10))
        self.assertFalse(idle.observe(61, 5, 10))
        self.assertFalse(idle.observe(62, 0, 10))
        self.assertFalse(idle.observe(121, None, 10))
        self.assertFalse(idle.observe(122, 0, 10))
        self.assertTrue(idle.observe(182, 0, 10))
        self.assertFalse(idle.observe(183, 0, 0))


class Preparation(unittest.TestCase):
    """The lock waiter must receive an existing, retained regular journal."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='sh801-prepare-', dir='/tmp')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()
        self.source = self.root / 'source'
        self.output = self.root / 'output'
        self.source.mkdir()
        self.output.mkdir()
        self.binary = self.root / 'story'
        self.binary.write_bytes(b'fixture binary identity')
        self.progress = self.output / 'progress.jsonl'

    def prepare(self, launch):
        """Inject unrelated host/Git facts; exercise real preparation and files."""
        def capture(argv):
            if argv[0] != 'git':
                return json.dumps({'result': 'gate-ready', 'argv': ['true']})
            args = argv[3:]
            if args[0] == 'status':
                return ''
            if args[0] == 'show':
                return 'gate-measurement-context.sh'
            if args == ['rev-parse', '--git-common-dir']:
                return str(self.source)
            return 'a' * 40

        flock = setup.fcntl.flock
        descriptors = []
        def lock(fd, operation):
            descriptors.append(fd)
            return flock(fd, operation)

        with mock.patch.object(setup, 'capture', side_effect=capture), \
             mock.patch.object(setup, 'normal_class', return_value=True), \
             mock.patch.object(setup, 'scheduling', return_value={}), \
             mock.patch.object(setup, 'tools_identity', return_value={}), \
             mock.patch.object(setup, 'check_storage', return_value={}), \
             mock.patch.object(setup, 'pressure_level', return_value=1), \
             mock.patch.object(setup.fcntl, 'flock', side_effect=lock), \
             mock.patch.object(setup.os, 'chdir'), \
             mock.patch.dict(os.environ), \
             mock.patch.object(setup.os, 'execvpe', side_effect=launch):
            try:
                setup.prepare(str(self.source), 'a' * 40, str(self.output), str(self.binary))
            finally:
                for fd in descriptors:
                    os.close(fd)

    def test_prepare_creates_progress_before_the_lock_writer_runs(self):
        def launch(_program, _argv, env):
            result = subprocess.run([sys.executable, '-B', str(SCRIPTS / 'gate-progress-writer.py'),
                                     'cost', 'start', 'resource-wait', 'fixture', 'locks/gate'],
                                    env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(self.progress.read_text())['phase'], 'resource-wait')
            self.assertEqual(self.progress.stat().st_mode & 0o777, 0o600)
        self.prepare(launch)

    def test_prepare_preserves_existing_progress_bytes_and_identity(self):
        retained = b'{"kind":"cost","event":"end"}\n'
        self.progress.write_bytes(retained)
        inode = self.progress.stat().st_ino
        def launch(*_args):
            self.assertEqual(self.progress.read_bytes(), retained)
            self.assertEqual(self.progress.stat().st_ino, inode)
        self.prepare(launch)

    def test_prepare_refuses_nonregular_progress_before_exec(self):
        target = self.root / 'keep'
        target.write_text('untouched')
        for shape in ('symlink', 'fifo', 'directory'):
            with self.subTest(shape=shape):
                if shape == 'symlink':
                    self.progress.symlink_to(target)
                elif shape == 'fifo':
                    os.mkfifo(self.progress)
                else:
                    self.progress.mkdir()
                launch = mock.Mock()
                try:
                    with self.assertRaises((Refusal, OSError)):
                        self.prepare(launch)
                    launch.assert_not_called()
                finally:
                    if shape == 'directory':
                        self.progress.rmdir()
                    else:
                        self.progress.unlink()
        self.assertEqual(target.read_text(), 'untouched')


class Authority(unittest.TestCase):
    """No receipt/class exception is granted from a flag, stale owner or sibling."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="sh801-authority-", dir="/tmp")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()
        self.path = self.root / "manifest.json"
        self.value = {"version": 1, "kind": "gate-class-measurement",
                      "common": str(self.root / "common"),
                      "worktree": str(self.root / "worktree"),
                      "commit": "b" * 40, "tree": "a" * 40}
        self.path.write_text(json.dumps(self.value))
        self.owner = {"measurement": str(self.path), "measurement_sha256": runtime.sha256(self.path)}

    def validate(self, **overrides):
        """Inject OS facts without replacing the context validator's policy."""
        with mock.patch.object(context, "paths", return_value=(Path(self.value['common']), Path(self.value['worktree']), self.root / 'owner')), \
             mock.patch.object(context, "held", return_value=overrides.get('held', True)), \
             mock.patch.object(context, "read", side_effect=lambda path: overrides.get('owner', self.owner) if str(path).endswith('.owner') else json.loads(Path(path).read_text())), \
             mock.patch.object(context.subprocess, "check_output", side_effect=overrides.get('git', [self.value['worktree'], self.value['commit'], self.value['tree'], '']) + [overrides.get('common', self.value['common'])]):
            return context.validate(str(self.path))

    def test_exact_owner_and_tree_are_required(self):
        self.assertEqual(self.validate(), self.value)
        for overrides in [{'held': False}, {'owner': {}}, {'owner': {'measurement': '/other'}}, {'common': '/foreign'},
                          {'git': ['/sibling', self.value['commit'], self.value['tree'], '']},
                          {'git': [self.value['worktree'], self.value['commit'], 'c' * 40, '']},
                          {'git': [self.value['worktree'], self.value['commit'], self.value['tree'], ' M file']}]:
            with self.subTest(overrides=overrides), self.assertRaises(Refusal):
                self.validate(**overrides)

    def test_missing_foreign_and_symlink_contexts_refuse(self):
        with mock.patch.dict(context.os.environ, {}, clear=True), self.assertRaises(Refusal):
            context.validate()
        alias = self.root / 'alias.json'
        alias.symlink_to(self.path)
        with self.assertRaises(Refusal):
            context.manifest(str(alias))
        for key, value in [('version', 2), ('kind', 'ordinary-verification'),
                           ('tree', 'not-an-oid'), ('worktree', '/unrelated')]:
            wrong = dict(self.value, **{key: value})
            self.path.write_text(json.dumps(wrong))
            with self.subTest(key=key), self.assertRaises(Refusal):
                context.manifest(str(self.path))


class RuntimeEvidence(unittest.TestCase):
    """Crash evidence and progress must not imply execution that did not occur."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='sh801-journal-', dir='/tmp')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()
        self.path = self.root / 'events.jsonl'

    def test_journal_and_interrupted_sample(self):
        runtime.journal(self.path, {'kind': 'start', 'index': 0})
        self.assertTrue(runtime.pending_sample(runtime.records(self.path)))
        runtime.journal(self.path, {'kind': 'sample', 'index': 0})
        self.assertFalse(runtime.pending_sample(runtime.records(self.path)))
        self.assertEqual(len(runtime.records(self.path)), 2)
        with self.path.open('a') as stream:
            stream.write('{partial')
        with self.assertRaises(Refusal):
            runtime.records(self.path)

    def test_resource_limits_are_observed_and_missing_or_changed_limits_refuse(self):
        observed = runtime.resource_limits()
        soft, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
        self.assertEqual(observed['RLIMIT_NOFILE'], {'soft': soft, 'hard': hard})
        identity = {'resource_limits': observed}
        self.assertEqual(runtime.require_resource_limits(identity), observed)
        with self.assertRaisesRegex(Refusal, 'resource limits'):
            runtime.require_resource_limits({})
        changed = copy.deepcopy(identity)
        changed['resource_limits']['RLIMIT_NOFILE']['soft'] = 64
        with self.assertRaisesRegex(Refusal, 'resource limits'):
            runtime.require_resource_limits(changed)

    def test_only_active_execution_triggers_probes(self):
        self.assertFalse(runtime.execution_active(self.path))
        runtime.journal(self.path, {'kind': 'item', 'path': 'release gate/rust-suite', 'status': 'running'})
        self.assertFalse(runtime.execution_active(self.path))
        runtime.journal(self.path, {'kind': 'case', 'path': 'release gate/rust-suite', 'outcome': 'pass'})
        self.assertTrue(runtime.execution_active(self.path))
        runtime.journal(self.path, {'kind': 'item', 'path': 'release gate/rust-suite', 'status': 'passed'})
        self.assertFalse(runtime.execution_active(self.path))

    def test_probe_isolation_removes_parent_authority(self):
        hostile = {'PATH': '/bin', 'HOME': '/home/real', 'STORYHOOK_STORE_PATH': '/real/store',
                   'STORYHOOK_GATE_MEASUREMENT': '/capability', 'STORYHOOK_VERIFIER_OWNER': 'nonce',
                   'STORYHOOK_DISPATCH': '1', 'STORYHOOK_FULL_AUTO': 'SH-801', 'GIT_DIR': '/other'}
        isolated = runtime.probe_environment(hostile, self.root, 123)
        for key in ['STORYHOOK_GATE_MEASUREMENT', 'STORYHOOK_VERIFIER_OWNER', 'STORYHOOK_DISPATCH', 'STORYHOOK_FULL_AUTO', 'GIT_DIR']:
            self.assertNotIn(key, isolated)
        self.assertEqual(isolated['STORYHOOK_STORE_PATH'], str(self.root / 'store.db'))
        self.assertEqual(isolated['STORYHOOK_PARENT_PID'], '123')
        self.assertTrue(isolated['PATH'].startswith(str(self.root / 'bin') + ':'))

    def test_cohort_restart_requires_complete_same_day_prefix(self):
        manifest, rows = cohort()
        self.assertEqual(collector.resume_index(manifest, [], manifest['day']), 0)
        events = [{'kind': 'start', 'index': 0}, rows[0]]
        self.assertEqual(collector.resume_index(manifest, events, manifest['day']), 1)
        for bad in [[{'kind': 'start', 'index': 0}],
                    [*events, {'kind': 'start', 'index': 1}],
                    [{'kind': 'start', 'index': 0}, dict(rows[0], valid=False)]]:
            with self.subTest(events=bad), self.assertRaises(Refusal):
                collector.resume_index(manifest, bad, manifest['day'])
        with self.assertRaises(Refusal):
            collector.resume_index(manifest, events, '2026-10-04')

    def test_report_requires_probe_cleanup_after_all_samples(self):
        manifest, rows = cohort()
        for row in rows:
            runtime.journal(self.root / 'samples.jsonl', row)
        result = collector.report(self.root, manifest)
        self.assertTrue(result['samples_complete'])
        self.assertFalse(result['complete'])
        from verifier_state import save
        save(self.root / 'cleanup.json', {'version': 1, 'ok': True})
        self.assertTrue(collector.report(self.root, manifest)['complete'])
        save(self.root / 'cleanup.json', {'version': 1, 'ok': False})
        self.assertFalse(collector.report(self.root, manifest)['complete'])

    def test_progress_mirror_waits_for_complete_lines_and_never_duplicates(self):
        output = self.root / 'mirror.jsonl'
        self.path.write_bytes(b'{"kind":"case"}\n{"kind":')
        offset = runtime.mirror_progress(self.path, output, 0)
        self.assertEqual(output.read_bytes(), b'{"kind":"case"}\n')
        self.assertEqual(runtime.mirror_progress(self.path, output, offset), offset)
        with self.path.open('ab') as stream:
            stream.write(b'"item"}\n')
        offset = runtime.mirror_progress(self.path, output, offset)
        self.assertEqual(output.read_bytes(), self.path.read_bytes())
        self.path.write_bytes(b'')
        with self.assertRaises(Refusal):
            runtime.mirror_progress(self.path, output, offset)


class RealProbes(unittest.TestCase):
    """Exercise the production binary, isolated daemon and pinned hook together."""

    @unittest.skipUnless(sys.platform == 'darwin' and os.environ.get('STORYHOOK_MEASUREMENT_TEST_BINARY'), 'requires the macOS Cargo-built CLI')
    def test_fixture_survives_warmup_and_restart_without_degraded_hook(self):
        with tempfile.TemporaryDirectory(prefix='sh801-real-probe-', dir='/tmp') as temp:
            root = Path(temp).resolve()
            (root / 'probe/bin').mkdir(parents=True)
            binary = Path(os.environ['STORYHOOK_MEASUREMENT_TEST_BINARY'])
            shutil.copy2(binary, root / 'probe/bin/story')
            identity = {'worktree': str(SCRIPTS.parent), 'binary_sha256': runtime.sha256(binary)}
            for _ in range(2):
                probes = Probes(root, identity)
                try:
                    probes.start()
                    self.assertEqual(len(probes.expected['stories']), 10)
                    for name in ('list', 'hook'):
                        result = probes.run(name, root / name)
                        self.assertTrue(result['ok'], result)
                finally:
                    probes.stop()


class OwnedExecution(unittest.TestCase):
    """Exercise production owner, class and receipt boundaries with real Git."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="sh801-execution-", dir="/tmp")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()
        self.source = self.root / 'source'
        self.source.mkdir()
        self.output = self.root / 'measurement'
        self.output.mkdir()
        self.worktree = self.output / 'worktree'
        self.env = {k: v for k, v in os.environ.items() if not k.startswith(('STORYHOOK_', 'GIT_'))}
        self.env.update({'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': '/dev/null',
                         'GIT_AUTHOR_NAME': 'Fixture', 'GIT_AUTHOR_EMAIL': 'fixture@example.invalid',
                         'GIT_COMMITTER_NAME': 'Fixture', 'GIT_COMMITTER_EMAIL': 'fixture@example.invalid',
                         'PYTHONDONTWRITEBYTECODE': '1', 'STORYHOOK_PYTHON': sys.executable})
        self.git('init', '-q')
        (self.source / 'scripts').mkdir()
        for name in ['leg.sh', 'gate-receipt.sh', 'tree-receipt.sh', 'gate-measurement-context.sh',
                     'gate_measurement_context.py', 'verifier_state.py', 'gate-progress.sh',
                     'python-runtime.sh', 'activity-log.sh', 'activity-run.py', 'test_output.py',
                     'gate-progress-writer.py', 'gate_cost.py', 'progress_journal.py']:
            shutil.copy2(SCRIPTS / name, self.source / 'scripts' / name)
        (self.source / 'scripts/python-bin').mkdir()
        shutil.copy2(SCRIPTS / 'python-bin/python3', self.source / 'scripts/python-bin/python3')
        self.git('add', '.')
        self.git('commit', '-qm', 'fixture')
        commit, tree = self.git('rev-parse', 'HEAD'), self.git('rev-parse', 'HEAD^{tree}')
        self.git('worktree', 'add', '-q', '--detach', str(self.worktree), commit)
        self.common = self.source / '.git'
        self.path = self.output / 'manifest.json'
        self.path.write_text(json.dumps({'version': 1, 'kind': 'gate-class-measurement',
                                       'worktree': str(self.worktree), 'common': str(self.common),
                                       'commit': commit, 'tree': tree,
                                       'tools': {'locale': runtime.capture(['locale'])},
                                       'resource_limits': runtime.resource_limits()}))
        self.env[context.VARIABLE] = str(self.path)

    def git(self, *args):
        """Run fixture Git without inherited project configuration."""
        return subprocess.check_output(['git', '-C', str(self.source), *args], env=self.env, text=True).strip()

    def owned(self, body, mode='measurement-run', prefix=()):
        """Run a script within the real lifecycle's durable session."""
        self.env['STORYHOOK_MEASUREMENT_GATE_DEADLINE'] = str(time.monotonic() + 300)
        return subprocess.run([*prefix, sys.executable, '-B', str(SCRIPTS / 'verifier-owner.py'), mode,
                               str(self.common), str(self.worktree), '--', sys.executable, '-B', '-c', body],
                              cwd=self.worktree, env=self.env, text=True, capture_output=True)

    def test_measured_legs_execute_twice_without_receipts(self):
        body = '''
import subprocess, sys
for _ in range(2):
    subprocess.run(['bash', 'scripts/leg.sh', '--reuse', 'fixture', '--', 'sh', '-c', 'echo ran >> executions'], check=True)
for script in ['gate-receipt.sh', 'tree-receipt.sh']:
    for phase in ['preflight', 'postlude']:
        subprocess.run(['bash', 'scripts/' + script, phase], check=True)
'''
        result = self.owned(body)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.worktree / 'executions').read_text(), 'ran\nran\n')
        self.assertFalse((self.common / 'storyhook/gate-leg-receipts').exists())
        self.assertFalse((self.common / 'storyhook/gate-receipts').exists())

    def test_ordinary_owner_cannot_grant_measurement_authority(self):
        result = self.owned("import subprocess; subprocess.run(['bash','scripts/tree-receipt.sh','postlude'],check=True)", mode='run')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('no matching live verifier owner', result.stderr)
        self.assertFalse((self.common / 'storyhook/gate-receipts').exists())

    @unittest.skipUnless(sys.platform == 'darwin' and os.environ.get('STORYHOOK_MEASUREMENT_TEST_BINARY'), 'requires the macOS Cargo-built CLI')
    def test_active_gate_overlaps_both_real_probes(self):
        self.assert_active_gate(1)

    @unittest.skipUnless(sys.platform == 'darwin' and os.environ.get('STORYHOOK_MEASUREMENT_TEST_BINARY'), 'requires the macOS Cargo-built CLI')
    def test_control_gate_preserves_launch_evidence_and_inherited_class(self):
        self.assert_active_gate(0)

    def assert_active_gate(self, index):
        """Retain launch evidence and enforce the observed scheduling input."""
        parent_class = runtime.scheduling()
        sample_dir = self.output / 'sample'
        allowance = load_grace.patience(20, load_grace.contention())
        gate = f'''import json, os, time
from pathlib import Path
progress = Path(os.environ['STORYHOOK_GATE_PROGRESS'])
with progress.open('a') as stream:
    stream.write(json.dumps(dict(kind='item',path='release gate/rust-suite',status='running'))+'\\n'+json.dumps(dict(kind='case',path='release gate/rust-suite',outcome='pass'))+'\\n')
probes = Path({str(sample_dir / 'probes.jsonl')!r})
deadline = time.monotonic() + {allowance!r}
while not probes.exists() or len(probes.read_text().splitlines()) != 2:
    if time.monotonic() >= deadline: raise SystemExit(6)
    time.sleep(.05)
with progress.open('a') as stream:
    stream.write(json.dumps(dict(kind='item',path='release gate/rust-suite',status='passed'))+'\\n')
'''
        value = json.loads(self.path.read_text())
        value['gate'] = {'argv': [sys.executable, '-c', gate]}
        self.path.write_text(json.dumps(value))
        (self.output / 'probe/bin').mkdir(parents=True)
        binary = Path(os.environ['STORYHOOK_MEASUREMENT_TEST_BINARY'])
        shutil.copy2(binary, self.output / 'probe/bin/story')
        body = f'''
import sys, os, json
from pathlib import Path
from unittest import mock
sys.path.insert(0, {str(SCRIPTS)!r})
from gate_measurement import run_sample, today
from gate_measurement_probes import Probes
identity=json.loads(Path({str(self.path)!r}).read_text())
identity.update(input_inventory={{}}, pinned_inputs={{'fixture': True}}, storage={{'targets':[{{}}]}}, applicable_legs=['rust-suite'])
os.environ['STORYHOOK_GATE_PROGRESS']={str(self.output / 'progress.jsonl')!r}
probes=Probes({str(self.output)!r}, {{'worktree':{str(SCRIPTS.parent)!r},'binary_sha256':{runtime.sha256(binary)!r}}})
try:
    probes.start()
    with mock.patch('gate_measurement.observe',return_value={{'fixture': True}}), \
         mock.patch('gate_measurement.check_storage',return_value={{}}), \
         mock.patch('gate_measurement.pressure_level',return_value=1), \
         mock.patch('gate_measurement.pressure',return_value={{'processes':f'{{os.getpid()}} 1 0.0 python\\n','resource_processes':f'{{os.getpid()}} 1 0.0 10 python\\n'}}):
        result=run_sample(identity,dict(tree=identity['tree'],day=today(),pairs=10),{index},Path({str(sample_dir)!r}),probes)
    print(json.dumps(result))
finally:
    probes.stop()
'''
        result = self.owned(body)
        events = runtime.records(sample_dir / 'progress.jsonl')
        contexts = [event for event in events if event['kind'] == 'context']
        self.assertEqual(len(contexts), 1, events)
        expected_class = [] if index == 0 else ['/usr/sbin/taskpolicy', '-c', 'utility']
        self.assertEqual(contexts[0]['inputs']['resources']['scheduling_argv'], expected_class)
        self.assertEqual(events[0], contexts[0], 'launch evidence must precede test progress')
        if index == 0 and not runtime.normal_class(parent_class):
            # Utility gates run this test too; their ancestry is not a valid control.
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('control gate has the wrong scheduling class',
                          (sample_dir / 'supervisor.log').read_text())
            self.assertFalse((sample_dir / 'time.txt').exists())
            self.assertFalse((sample_dir / 'exit.json').exists())
            return
        self.assertEqual(result.returncode, 0, result.stderr)
        sample = json.loads(result.stdout)
        self.assertTrue(sample['valid'], sample)
        self.assertEqual(sample['exit_code'], 0)
        self.assertEqual([p['name'] for p in sample['probes']], ['list', 'hook'])
        self.assertTrue(all(p['ok'] and p['overlap'] for p in sample['probes']))

    def test_conflicting_owner_refuses_and_cancellation_reaps_gate_subgroup(self):
        ready = self.output / 'child.json'
        worker = f'''import os, json, signal, time
from pathlib import Path
os.setpgid(0, 0)
signal.signal(signal.SIGTERM, signal.SIG_IGN)
Path({str(ready)!r}).write_text(json.dumps([os.getpid(), os.getsid(0)]))
while True: time.sleep(1)
'''
        gate = f'''import subprocess, sys, time
subprocess.Popen([sys.executable, '-c', {worker!r}])
while True: time.sleep(1)
'''
        body = f'''import subprocess, sys
subprocess.run([sys.executable, '-B', {str(SCRIPTS / 'verifier-owner.py')!r},
 'measurement-gate', {str(self.common)!r}, {str(self.worktree)!r}, '--', 'utility',
 sys.executable, '-c', {gate!r}], check=True)
'''
        allowance = load_grace.patience(30, load_grace.contention())
        env = dict(self.env, STORYHOOK_VERIFIER_CLEANUP_GRACE_MS=str(int(allowance * 1000)),
                   STORYHOOK_MEASUREMENT_GATE_DEADLINE=str(time.monotonic() + 300))
        child = subprocess.Popen([sys.executable, '-B', str(SCRIPTS / 'verifier-owner.py'),
                                  'measurement-run', str(self.common), str(self.worktree), '--',
                                  sys.executable, '-c', body], cwd=self.worktree, env=env,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            patience = load_grace.Patience(allowance, 1, 30, time.monotonic())
            while not ready.exists():
                if child.poll() is not None:
                    out, err = child.communicate()
                    self.fail(f'owned gate did not start: exit={child.returncode} fixture={self.root}: {out}\n{err}')
                if patience.expired(time.monotonic()):
                    self.fail(f'owned gate did not start before its allowance: fixture={self.root}')
                time.sleep(.05)
            conflict = self.owned('pass')
            self.assertNotEqual(conflict.returncode, 0)
            self.assertIn('live verifier owner', conflict.stderr)
            child.terminate()
            out, err = child.communicate(timeout=allowance * 2)
            self.assertNotEqual(child.returncode, 0, (out, err))
            _, sid = json.loads(ready.read_text())
            from verifier_state import session_members
            self.assertEqual(session_members(sid), [], err)
            retry = self.owned('pass')
            self.assertEqual(retry.returncode, 0, retry.stderr)
        finally:
            if child.poll() is None:
                child.terminate()
            child.communicate(timeout=allowance * 2)

    @unittest.skipUnless(sys.platform == 'darwin', 'macOS timed command')
    def test_changed_resource_limits_refuse_before_timed_command(self):
        value = json.loads(self.path.read_text())
        marker = self.output / 'must-not-run'
        value['gate'] = {'argv': ['touch', str(marker)]}
        self.path.write_text(json.dumps(value))
        destination = self.output / 'sample'
        destination.mkdir()
        body = f'''
import resource, subprocess, sys
soft, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
resource.setrlimit(resource.RLIMIT_NOFILE, (64, hard))
sys.exit(subprocess.call([sys.executable, '-B', {str(SCRIPTS / 'verifier-owner.py')!r},
 'measurement-gate', {str(self.common)!r}, {str(self.worktree)!r}, '--', 'utility',
 sys.executable, '-B', {str(SCRIPTS / 'gate_measurement.py')!r}, 'gate-exec',
 {str(self.path)!r}, 'utility', {str(destination)!r}]))
'''
        result = self.owned(body)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('resource limits', result.stderr)
        self.assertFalse(marker.exists())
        self.assertFalse((destination / 'time.txt').exists())

    @unittest.skipUnless(sys.platform == 'darwin', 'macOS timed command')
    def test_collector_observes_real_command_failure_and_cleanup(self):
        value = json.loads(self.path.read_text())
        value['gate'] = {'argv': [sys.executable, '-c', 'import sys; sys.exit(7)']}
        self.path.write_text(json.dumps(value))
        body = f'''
import sys, os, json
from pathlib import Path
sys.path.insert(0, {str(SCRIPTS)!r})
from gate_measurement import run_sample, today
identity = json.loads(Path({str(self.path)!r}).read_text())
from unittest import mock
identity.update(input_inventory={{}}, pinned_inputs={{'fixture': True}}, storage={{'targets':[{{}}]}}, applicable_legs=['rust-suite'])
os.environ['STORYHOOK_GATE_PROGRESS'] = {str(self.output / 'progress.jsonl')!r}
cohort = dict(version=1, tree=identity['tree'], pairs=10, day=today())
with mock.patch('gate_measurement.observe', return_value={{'fixture': True}}), \
     mock.patch('gate_measurement.check_storage', return_value={{}}), \
     mock.patch('gate_measurement.pressure_level', return_value=1), \
     mock.patch('gate_measurement.pressure', return_value={{'processes':f'{{os.getpid()}} 1 0.0 python\\n','resource_processes':f'{{os.getpid()}} 1 0.0 10 python\\n'}}):
    sample = run_sample(identity, cohort, 1, Path({str(self.output / 'sample')!r}), None)
print(json.dumps(sample))
'''
        result = self.owned(body)
        self.assertEqual(result.returncode, 0, result.stderr)
        sample = json.loads(result.stdout)
        self.assertEqual(sample['exit_code'], 7)
        self.assertEqual(sample['supervisor_exit'], 7)
        self.assertEqual(sample['cleanup'], 'complete')
        self.assertGreaterEqual(sample['wall_seconds'], 0)
        self.assertFalse(sample['valid'])
        self.assertEqual(sample['class']['qos'], 0x11)
        self.assertEqual(sample['class']['locale'], runtime.capture(['locale']))

    @unittest.skipUnless(sys.platform == 'darwin', 'macOS scheduling probe')
    def test_class_changes_only_for_treatment_gate(self):
        reporter = "import ctypes; q=ctypes.CDLL(None).qos_class_self; q.restype=ctypes.c_uint; print(q())"
        body = f'''
import importlib.util, json, sys
sys.path.insert(0, {str(SCRIPTS)!r})
spec = importlib.util.spec_from_file_location('measurement_owner', {str(SCRIPTS / 'verifier-owner.py')!r})
owner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(owner)
common, worktree = {str(self.common)!r}, {str(self.worktree)!r}
common, worktree, key = owner.paths(common, worktree)
execute = owner.execute
def observed_execute(*args, **kwargs):
    print(json.dumps(kwargs['gate_prefix']), flush=True)
    return execute(*args, **kwargs)
owner.execute = observed_execute
for condition in ['control', 'utility']:
    code = owner.run('measurement-gate', common, worktree, key,
                     [condition, sys.executable, '-c', {reporter!r}], owner.Cancellation())
    if code: raise SystemExit(code)
'''
        result = self.owned(body)
        self.assertEqual(result.returncode, 0, result.stderr)
        values = [json.loads(s) for s in result.stdout.splitlines()]
        self.assertEqual(values[0], [])
        self.assertEqual(values[2], ['/usr/sbin/taskpolicy', '-c', 'utility'])
        self.assertEqual(values[3], 0x11)
        # The unclamped control inherits the lifecycle's class, whatever the
        # enclosing test gate chose. Compare a direct child of the same owner.
        control = self.owned(reporter)
        self.assertEqual(values[1], int(control.stdout.strip()))


if __name__ == "__main__":
    unittest.main()
