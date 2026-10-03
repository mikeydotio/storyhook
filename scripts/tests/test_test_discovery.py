#!/usr/bin/env python3
"""SH-795: authoritative libtest discovery fails before executing a battery."""

import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest

sys.dont_write_bytecode = True
SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
from test_discovery import Discovery, DiscoveryError


class DiscoveryTests(unittest.TestCase):
    """Exercise real child processes; shorten only an injected listing deadline."""

    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix='sh795-discovery-', dir='/tmp')
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.env = os.environ.copy()
        self.discovery = Discovery(self.env, lambda: None)

    def executable(self, body, name='listing'):
        """Write a fixture executable with this interpreter, independent of PATH."""
        path = self.root / name
        path.write_text(f'#!{sys.executable}\n' + body)
        path.chmod(0o755)
        return str(path)

    def test_runnable_counts_preserve_selection_and_subtract_ignored(self):
        calls = self.root / 'calls'
        executable = self.executable(f'''
import json, sys
with open({str(calls)!r}, 'a') as f: f.write(json.dumps(sys.argv[1:])+'\\n')
if '--ignored' in sys.argv:
    print('ignored_case: test')
else:
    print('runs_case: test\\nignored_case: test\\nbench_case: benchmark\\n')
''')
        for flags, expected, invocations in [([], 2, 2), (['--ignored'], 1, 1),
                                               (['--include-ignored'], 3, 1)]:
            with self.subTest(flags=flags):
                calls.write_text('')
                selection = ['some_filter', '--exact', '--skip', 'skipped', *flags]
                self.assertEqual(self.discovery.count_tests(executable, selection), expected)
                recorded = [json.loads(line) for line in calls.read_text().splitlines()]
                self.assertEqual(len(recorded), invocations)
                self.assertEqual(recorded[0], ['--list', *selection])

    def test_zero_selected_cases_is_an_exact_zero(self):
        executable = self.executable("print('0 tests, 0 benchmarks')\n")
        self.assertEqual(self.discovery.count_tests(executable, []), 0)

    def test_failures_keep_command_context_and_stderr(self):
        for ignored in [False, True]:
            with self.subTest(ignored=ignored):
                executable = self.executable(f'''
import sys
if {ignored!r} and '--ignored' not in sys.argv:
    print('case: test')
else:
    print('fixture listing diagnostic', file=sys.stderr)
    sys.exit(42)
''')
                with self.assertRaisesRegex(DiscoveryError, 'fixture listing diagnostic') as failure:
                    self.discovery.count_tests(executable, [])
                self.assertIn(executable, str(failure.exception))
                self.assertIn('42', str(failure.exception))

    def test_missing_executable_fails_loudly(self):
        with self.assertRaisesRegex(DiscoveryError, 'missing-executable'):
            self.discovery.count_tests(str(self.root / 'missing-executable'), [])

    def test_inconsistent_ignored_listing_is_rejected(self):
        executable = self.executable("import sys\nif '--ignored' in sys.argv: print('extra: test')\n")
        with self.assertRaisesRegex(DiscoveryError, 'ignored.*exceeds'):
            self.discovery.count_tests(executable, [])

    def test_listing_timeout_reaps_the_owned_process(self):
        pid_file = self.root / 'pid'
        executable = self.executable(f'''
import os, signal
from pathlib import Path
Path({str(pid_file)!r}).write_text(str(os.getpid()))
signal.pause()
''')
        from load_grace import contention, patience
        discovery = Discovery(self.env, lambda: None, list_timeout=patience(1, contention()))
        with self.assertRaisesRegex(DiscoveryError, 'timed out'):
            discovery.count_tests(executable, [])
        self.assertTrue(pid_file.exists(), "listing did not publish readiness before its deadline")
        with self.assertRaises(ProcessLookupError):
            os.kill(int(pid_file.read_text()), 0)

    def test_cancellation_prevents_launch(self):
        executable = self.executable("raise AssertionError('must not start')\n")
        with self.assertRaisesRegex(DiscoveryError, 'cancelled'):
            Discovery(self.env, lambda: signal.SIGTERM).count_tests(executable, [])

    def test_cancellation_reaps_a_listing_and_its_descendant(self):
        # Readiness is a fixture event, not an elapsed-time guess.
        executable = self.executable(f'''
import os, signal, subprocess, sys
child = subprocess.Popen([sys.executable, '-c', 'import signal; signal.pause()'])
with open({str(self.root / 'child')!r}, 'w') as f: f.write(str(child.pid))
with open({str(self.root / 'ready-pending')!r}, 'w') as f: f.write(str(os.getpid()))
os.replace({str(self.root / 'ready-pending')!r}, {str(self.root / 'ready')!r})
signal.pause()
''')
        ready = self.root / 'ready'
        # The cancellation predicate observes fixture readiness on each production poll.
        discovery = Discovery(self.env, lambda: signal.SIGTERM if ready.exists() else None)
        with self.assertRaisesRegex(DiscoveryError, 'cancelled'):
            discovery.count_tests(executable, [])
        for path in [ready, self.root / 'child']:
            pid = int(path.read_text())
            result = subprocess.run(['ps', '-o', 'state=', '-p', str(pid)], capture_output=True, text=True)
            self.assertTrue(not result.stdout.strip() or result.stdout.strip().startswith('Z'), result.stdout)

    def test_companion_application_binary_cannot_replace_test_artifact(self):
        spec = importlib.util.spec_from_file_location('pool_under_test', SCRIPTS / 'test-pool.py')
        pool = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(pool)
        jobs = [pool.Job(0, 'fixture', 'test', 'same_name')]
        records = [dict(reason='compiler-artifact', executable='/test-executable',
                        target=dict(kind=['test'], name='same_name')),
                   dict(reason='compiler-artifact', executable='/application-executable',
                        target=dict(kind=['bin'], name='same_name'))]
        self.executable('print(' + repr('\n'.join(json.dumps(r) for r in records)) + ')\n', 'cargo')
        env = dict(self.env, PATH=f'{self.root}:{self.env["PATH"]}')
        found = Discovery(env, lambda: None).executables(jobs, [])
        self.assertEqual(found[('fixture', 'test', 'same_name')], '/test-executable')

    def test_artifact_lookup_requires_success_and_every_selected_target(self):
        spec = importlib.util.spec_from_file_location('pool_under_test', SCRIPTS / 'test-pool.py')
        pool = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(pool)
        jobs = [pool.Job(0, 'fixture-package', 'test', 'fixture_target')]
        for body, diagnostic in [("import sys\nprint('artifact lookup failed',file=sys.stderr)\nsys.exit(42)\n", 'artifact lookup failed'),
                                 ("print('{}')\n", 'fixture-package:test:fixture_target')]:
            with self.subTest(body=body):
                self.executable(body, 'cargo')
                env = dict(self.env, PATH=f'{self.root}:{self.env["PATH"]}')
                with self.assertRaisesRegex(DiscoveryError, diagnostic):
                    Discovery(env, lambda: None).executables(jobs, [])

class PoolFlowTests(unittest.TestCase):
    """Fault-injected Cargo boundaries drive the production shell and pool."""

    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix='sh795-pool-', dir='/tmp')
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        for name in ['scripts', 'tests', 'bin']:
            (self.root / name).mkdir()
        # Runtime selection requires the tracked PATH launcher beside its shell helper.
        (self.root / 'scripts' / 'python-bin').mkdir()
        (self.root / 'scripts' / 'python-bin' / 'python3').symlink_to(
            SCRIPTS / 'python-bin' / 'python3')
        for script in SCRIPTS.iterdir():
            if script.suffix in ('.py', '.sh', '.awk'):
                (self.root / 'scripts' / script.name).symlink_to(script)
        for name in ['first', 'second']:
            (self.root / 'tests' / f'{name}.rs').touch()
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith('STORYHOOK_')}
        self.env.update(PATH=f'{self.root / "bin"}:{self.env["PATH"]}',
                        STORYHOOK_GATE_LOCK='0', STORYHOOK_TEST_THREAD_BUDGET='2',
                        STORYHOOK_GATE_PROGRESS=str(self.root / 'progress.ndjson'),
                        PYTHONDONTWRITEBYTECODE='1')
        self.write_program(self.root / 'bin' / 'cargo', r'''
import json, os, sys
from pathlib import Path
root = Path.cwd()
a = sys.argv[1:]
mode = os.environ.get('DISCOVERY_FAULT', '')
with (root / 'calls').open('a') as f: f.write(json.dumps(a)+'\n')
if '--no-run' in a:
    if mode == 'compile':
        print('fixture compiler failure', file=sys.stderr)
        sys.exit(101)
    if '--no-fail-fast' not in a:
        if mode == 'artifact-exit':
            print('fixture artifact failure', file=sys.stderr)
            sys.exit(42)
        if mode == 'artifact-missing': sys.exit(0)
    for name in ['first', 'second']:
        exe = root / ('absent' if mode == 'missing-executable' else name)
        print(json.dumps(dict(reason='compiler-artifact', executable=str(exe),
                              target=dict(kind=['test'], name=name))))
    print(json.dumps(dict(reason='build-finished', success=True)))
elif '--list' in a:
    if '--doc' not in a: sys.exit('serial binary discovery is forbidden')
    if mode == 'doctest': sys.exit('fixture doctest discovery failure')
    if '--ignored' not in a: print('src/lib.rs - example (line 1): test')
else:
    with (root / 'executed').open('a') as f: f.write(' '.join(a)+'\n')
    if '--doc' in a:
        print('   Doc-tests storyhook\ntest src/lib.rs - example (line 1) ... ok')
    else:
        name = a[a.index('--test')+1]
        red = mode == 'red' and name == 'first'
        print(f'     Running tests/{name}.rs (target/debug/deps/{name})')
        print(f'test {name}_case ... '+('FAILED' if red else 'ok'))
        sys.exit(101 if red else 0)
''')
        listing = r'''
import fcntl, json, os, sys, time
from pathlib import Path
root = Path.cwd()
mode = os.environ.get('DISCOVERY_FAULT', '')
ignored = '--ignored' in sys.argv
with (root / 'listings').open('a') as f:
    f.write(json.dumps([Path(sys.argv[0]).name, *sys.argv[1:]])+'\n')
if mode == 'listing': sys.exit('fixture listing failure')
if mode == 'ignored' and ignored: sys.exit('fixture ignored-listing failure')
if mode == 'inconsistent':
    if ignored: print('unexpected: test')
else:
    if mode == 'parallel' and not ignored:
        (root / (Path(sys.argv[0]).name + '.ready')).touch()
        # Both listings must reach the barrier before either can finish.
        deadline = time.monotonic() + float(os.environ['BARRIER_PATIENCE'])
        while not all((root / (name + '.ready')).exists() for name in ['first', 'second']):
            if time.monotonic() >= deadline: sys.exit('parallel listing barrier failed')
            time.sleep(0.01)
    if not ignored: print('runnable: test')
    print('ignored: test')
'''
        for name in ['first', 'second']:
            self.write_program(self.root / name, listing)

    def write_program(self, path, body):
        """Install an external command fixture, without replacing runner behavior."""
        path.write_text(f'#!{sys.executable}\n' + body)
        path.chmod(0o755)

    def run_battery(self, fault='', budget='2'):
        """The public shell entry point, including the doctest continuation path."""
        from load_grace import contention, patience
        env = dict(self.env, DISCOVERY_FAULT=fault, STORYHOOK_TEST_THREAD_BUDGET=budget,
                   BARRIER_PATIENCE=str(patience(5, contention())))
        return subprocess.run(['bash', 'scripts/run-tests.sh', '--only', 'first', 'second'],
                              cwd=self.root, env=env, capture_output=True, text=True,
                              timeout=patience(30, contention()))

    def test_discovery_faults_never_execute_a_binary_or_doctest(self):
        for fault in ['compile', 'artifact-exit', 'artifact-missing', 'missing-executable',
                      'listing', 'ignored', 'inconsistent', 'doctest']:
            with self.subTest(fault=fault):
                journal = self.root / 'progress.ndjson'
                journal.write_text('')
                result = self.run_battery(fault)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertFalse((self.root / 'executed').exists(), result.stdout + result.stderr)
                events = [json.loads(line) for line in journal.read_text().splitlines()]
                self.assertFalse(any('total' in event for event in events), events)
                self.assertIn(dict(label='discovering tests', status='failed'),
                              [dict(label=e.get('label'), status=e.get('status')) for e in events])
                self.assertIn('failure' if fault in ['compile', 'artifact-exit', 'listing', 'ignored', 'doctest']
                              else 'discovery failed before execution', result.stdout + result.stderr)

    def test_parallel_listing_owns_total_and_runs_exactly_twice_per_binary(self):
        result = self.run_battery('parallel')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        events = [json.loads(line) for line in (self.root / 'progress.ndjson').read_text().splitlines()]
        totals = [event['total'] for event in events if 'total' in event]
        self.assertEqual(totals, [3])
        listings = [json.loads(line) for line in (self.root / 'listings').read_text().splitlines()]
        for name in ['first', 'second']:
            self.assertEqual(sum(row[0] == name for row in listings), 2, listings)
        total_at = next(i for i, e in enumerate(events) if 'total' in e)
        end_at = next(i for i, e in enumerate(events) if e.get('label') == 'discovering tests' and e['status'] == 'passed')
        case_at = next(i for i, e in enumerate(events) if e['kind'] == 'case')
        self.assertLess(total_at, end_at)
        self.assertLess(end_at, case_at)

    def test_red_binary_still_runs_its_sibling_and_doctests(self):
        result = self.run_battery('red')
        self.assertEqual(result.returncode, 101, result.stdout + result.stderr)
        executed = (self.root / 'executed').read_text()
        for selected in ['--test first', '--test second', '--doc']:
            self.assertIn(selected, executed)

    def test_budget_one_lists_both_targets_without_deadlock(self):
        result = self.run_battery(budget='1')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(len((self.root / 'listings').read_text().splitlines()), 4)


    def pool_command(self, marker):
        """Invoke the same driver as the shell for single-process signal tests."""
        return [sys.executable, str(SCRIPTS / 'test-pool.py'),
                '--budget', '1', '--log', str(self.root / 'output.log'),
                '--work', str(self.root / 'pool'), '--progress', 'release gate/rust-suite',
                '--discovery-ready', str(marker),
                '--job', 'storyhook', 'test', 'first',
                '--job', 'storyhook', 'test', 'second', '--']

    def test_unwritable_progress_never_publishes_readiness_or_executes(self):
        marker = self.root / 'discovery-ready'
        env = dict(self.env, STORYHOOK_GATE_PROGRESS=str(self.root))
        result = subprocess.run(self.pool_command(marker), cwd=self.root, env=env,
                                capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(marker.exists())
        self.assertFalse((self.root / 'executed').exists())
        self.assertIn('could not report failed discovery', result.stderr)

    def test_signalling_pool_during_listing_stops_queued_work_and_reaps_child(self):
        from load_grace import contention, patience
        ready = self.root / 'listing-ready'
        self.write_program(self.root / 'first', f"""
import os, signal
from pathlib import Path
pending = Path({str(ready)!r} + '.pending')
pending.write_text(str(os.getpid()))
pending.replace({str(ready)!r})
signal.pause()
""")
        marker = self.root / 'discovery-ready'
        child = subprocess.Popen(self.pool_command(marker), cwd=self.root, env=self.env,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        def cleanup():
            if child.poll() is None:
                child.terminate()
            child.communicate(timeout=patience(10, contention()))
        self.addCleanup(cleanup)
        deadline = time.monotonic() + patience(10, contention())
        while not ready.exists():
            self.assertIsNone(child.poll(), 'pool exited before listing readiness')
            self.assertLess(time.monotonic(), deadline, 'listing did not start')
            time.sleep(0.01)
        child.send_signal(signal.SIGTERM)
        out, err = child.communicate(timeout=patience(10, contention()))
        self.assertEqual(child.returncode, 128 + signal.SIGTERM, out + err)
        self.assertFalse(marker.exists())
        self.assertFalse((self.root / 'executed').exists())
        self.assertFalse((self.root / 'listings').exists(), 'queued second listing launched')
        with self.assertRaises(ProcessLookupError):
            os.kill(int(ready.read_text()), 0)


if __name__ == '__main__':
    unittest.main()
