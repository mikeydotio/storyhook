#!/usr/bin/env python3
"""SH-793: process isolation, bounded concurrency and trustworthy case results."""

import ast
import importlib
import io
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import types
import unittest
from unittest import mock

sys.dont_write_bytecode = True
import load_grace
import run_verifier_tests as runner

DIRECTORY = Path(__file__).resolve().parent
FIXTURE = r'''
import fcntl, json, os, sys, time, unittest
from pathlib import Path
root = Path(__file__).parent
changed = False

def event(kind, name):
    with (root / 'events').open('a') as log:
        fcntl.flock(log, fcntl.LOCK_EX)
        log.write(json.dumps([kind, name, os.getpid(), sys.executable]) + '\n')
        log.flush()

class Cases(unittest.TestCase):
    def exercise(self):
        global changed
        self.assertFalse(changed)
        self.assertNotIn('SH793_CASE_CHANGED', os.environ)
        changed = True
        os.environ['SH793_CASE_CHANGED'] = '1'
        name = self._testMethodName
        event('start', name)
        self.addCleanup(event, 'end', name)
        if (root / 'hold').exists():
            (root / name).touch()
            deadline = time.monotonic() + float(os.environ['SH793_FIXTURE_PATIENCE'])
            while not (root / 'release').exists():
                if time.monotonic() >= deadline:
                    self.fail('fixture release never arrived')
                time.sleep(.01)
        print('stdout-' + name)
        print('stderr-' + name, file=sys.stderr)
    def test_a(self): self.exercise()
    def test_b(self): self.exercise()
    def test_c(self): self.exercise()

class Outcomes(unittest.TestCase):
    def test_fail(self): self.fail('named failure evidence')
    def test_error(self): raise RuntimeError('named error evidence')
    def test_crash(self): os.kill(os.getpid(), 9)
    def test_large(self):
        print('out-' + 'x' * 100000)
        print('err-' + 'y' * 100000, file=sys.stderr)
    @unittest.skip('intentional skip')
    def test_skip(self): pass
    @unittest.expectedFailure
    def test_expected_failure(self): self.fail('expected')
    @unittest.expectedFailure
    def test_unexpected_success(self): pass

if __name__ == '__main__': unittest.main()
'''


class Discovery(unittest.TestCase):
    """Selection neither drops local cases nor duplicates imported fixtures."""

    def module(self, body):
        """Construct a module without reading or writing the checkout."""
        module = types.ModuleType('cases')
        exec('import unittest\n' + body, module.__dict__)
        return module

    def test_local_classes_only_and_exact_selection(self):
        """Local cases are sorted, while explicit selectors retain their identity."""
        module = self.module('class Local(unittest.TestCase):\n'
                             ' def test_b(self): pass\n def test_a(self): pass\n')
        module.Foreign = unittest.FunctionTestCase
        self.assertEqual(runner.discover(module), ['Local.test_a', 'Local.test_b'])
        self.assertEqual(runner.discover(module, ['Local.test_b']), ['Local.test_b'])

    def test_empty_unknown_and_duplicate_selections_fail(self):
        """A bad selection never silently reports an empty or repeated success."""
        with self.assertRaisesRegex(ValueError, 'no tests'):
            runner.discover(self.module(''))
        module = self.module('class Local(unittest.TestCase):\n def test_a(self): pass\n')
        for selected in (['absent'], ['Local'], ['Local.test_a', 'Local.test_a']):
            with self.subTest(selected=selected), self.assertRaises(ValueError):
                runner.discover(module, selected)
        module.Alias = module.Local
        with self.assertRaisesRegex(ValueError, 'duplicate'):
            runner.discover(module)

    def test_loader_errors_are_not_a_successful_inventory(self):
        """Discovery diagnostics survive even when the loader returns a suite."""
        module = self.module('class Local(unittest.TestCase):\n def test_a(self): pass\n')
        with mock.patch.object(unittest.TestLoader, 'loadTestsFromTestCase') as load:
            load.side_effect = lambda cls: unittest.TestSuite()
            with mock.patch.object(unittest, 'TestLoader', return_value=types.SimpleNamespace(
                    errors=['broken discovery'], loadTestsFromTestCase=load)):
                with self.assertRaisesRegex(ValueError, 'broken discovery'):
                    runner.discover(module)

    def test_verdict_does_not_select_imported_lifecycle_tests(self):
        """The runner matches the verdict file's explicit defaultTest boundary."""
        module = importlib.import_module('test_verifier_verdict')
        tree = ast.parse(Path(module.__file__).read_text())
        declarations = [keyword.value for call in ast.walk(tree)
                        if isinstance(call, ast.Call) and isinstance(call.func, ast.Attribute)
                        and isinstance(call.func.value, ast.Name)
                        and (call.func.value.id, call.func.attr) == ('unittest', 'main')
                        for keyword in call.keywords if keyword.arg == 'defaultTest']
        self.assertEqual(len(declarations), 1, 'one explicit direct-file inventory')
        expected = unittest.TestLoader().loadTestsFromNames(
            ast.literal_eval(declarations[0]), module)
        names = sorted(case.id().removeprefix(module.__name__ + '.')
                       for group in expected for case in group)
        self.assertEqual(runner.discover(module), names)
        self.assertFalse(any(name.startswith('VerifierLifecycle.') for name in names))


class Processes(unittest.TestCase):
    """Use real children and release barriers, never speed-based overlap claims."""

    def setUp(self):
        """Give every probe its own source, events, captures and release markers."""
        self.tmp = tempfile.TemporaryDirectory(prefix='sh793-', dir='/tmp')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.script = self.root / 'fixture_cases.py'
        self.script.write_text(FIXTURE)
        self.patience = load_grace.patience(30, load_grace.contention())
        self.env = dict(os.environ, SH793_FIXTURE_PATIENCE=str(self.patience))
        self.log = self.root / 'runner.log'

    def launch(self, selectors, jobs=2, hold=False):
        """Run the production scheduler from an independent Python parent."""
        if hold:
            (self.root / 'hold').touch()
        code = ('import sys\nfrom pathlib import Path\n'
                f'sys.path.insert(0, {str(DIRECTORY)!r})\n'
                'import run_verifier_tests as r\n'
                f'sys.exit(r.run_cases(Path({str(self.script)!r}), {selectors!r}, {jobs!r}))\n')
        with self.log.open('wb') as log:
            child = subprocess.Popen([sys.executable, '-B', '-c', code], env=self.env,
                                     stdout=log, stderr=subprocess.STDOUT)
        self.addCleanup(self.finish, child)
        return child

    def finish(self, child):
        """Always release probes and reap their parent, including assertion failures."""
        (self.root / 'release').touch()
        try:
            child.wait(timeout=self.patience)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait()
            raise

    def wait_for(self, predicate, child):
        """Wait for a causal publication, reporting child output on failure."""
        deadline = time.monotonic() + self.patience
        while not predicate():
            if child.poll() is not None or time.monotonic() >= deadline:
                self.fail(self.log.read_text())
            time.sleep(.01)

    def events(self):
        """Read only after the corresponding children are reaped."""
        return [json.loads(line) for line in (self.root / 'events').read_text().splitlines()]

    def test_overlap_bound_exactly_once_and_process_isolation(self):
        """Two held children coexist; every case owns a fresh interpreter."""
        child = self.launch(['Cases.test_a', 'Cases.test_b', 'Cases.test_c'], hold=True)
        self.wait_for(lambda: all((self.root / n).exists() for n in ('test_a', 'test_b')), child)
        self.assertFalse((self.root / 'test_c').exists())
        self.finish(child)
        self.assertEqual(child.returncode, 0, self.log.read_text())
        active = peak = 0
        starts = []
        for kind, name, pid, executable in self.events():
            active += 1 if kind == 'start' else -1
            peak = max(peak, active)
            if kind == 'start':
                starts.append((name, pid))
                self.assertEqual(executable, sys.executable)
        self.assertEqual((active, peak), (0, 2))
        self.assertEqual(sorted(n for n, _ in starts), ['test_a', 'test_b', 'test_c'])
        self.assertEqual(len({p for _, p in starts}), 3)
        text = self.log.read_text()
        for name in ('test_a', 'test_b', 'test_c'):
            self.assertIn('stdout-' + name, text)
            self.assertIn('stderr-' + name, text)
        self.assertIn('selected=3 completed=3 failed=0', text)
        self.assertFalse(list(self.root.rglob('*.pyc')))

    def test_serial_mode(self):
        """One slot completes each case before admitting the next one."""
        child = self.launch(['Cases.test_a', 'Cases.test_b', 'Cases.test_c'], jobs=1)
        self.finish(child)
        self.assertEqual(child.returncode, 0, self.log.read_text())
        self.assertEqual([e[0] for e in self.events()], ['start', 'end'] * 3)

    def test_failures_crash_and_large_output_do_not_drop_later_cases(self):
        """Every outcome retains diagnostics and every selected case is attempted."""
        names = ['fail', 'error', 'crash', 'large', 'skip', 'expected_failure', 'unexpected_success']
        child = self.launch(['Outcomes.test_' + n for n in names])
        self.finish(child)
        self.assertNotEqual(child.returncode, 0)
        text = self.log.read_text()
        for evidence in ('named failure evidence', 'named error evidence', 'out-' + 'x' * 100000,
                         'err-' + 'y' * 100000, 'selected=7 completed=7 failed=4'):
            self.assertIn(evidence, text)
        self.assertIn('Outcomes.test_crash', text)

    def test_launch_failure_is_named_and_nonzero(self):
        """An OS launch refusal is a named failure, never a missing green case."""
        output = io.StringIO()
        with mock.patch.object(subprocess, 'Popen', side_effect=OSError('cannot spawn')):
            status = runner.run_cases(self.script, ['Cases.test_a'], stream=output)
        self.assertNotEqual(status, 0)
        self.assertIn('Cases.test_a', output.getvalue())
        self.assertIn('cannot spawn', output.getvalue())

    def test_signals_stop_admissions_and_drain_active_cases(self):
        """All supported runner signals drain the two active fixture cleanups."""
        for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
            with self.subTest(signal=sig):
                for path in self.root.glob('test_*'):
                    path.unlink()
                (self.root / 'release').unlink(missing_ok=True)
                child = self.launch(['Cases.test_a', 'Cases.test_b', 'Cases.test_c'], hold=True)
                self.wait_for(lambda: all((self.root / n).exists() for n in ('test_a', 'test_b')), child)
                child.send_signal(sig)
                self.wait_for(lambda: 'cancelling' in self.log.read_text(), child)
                self.assertIsNone(child.poll())
                self.finish(child)
                self.assertEqual(child.returncode, 128 + sig, self.log.read_text())
                self.assertFalse((self.root / 'test_c').exists())
                self.assertIn('selected=3 completed=2 failed=0', self.log.read_text())

    def test_cli_selection_listing_and_argument_errors(self):
        """The command supports listing and exact reruns, rejecting bad arguments."""
        case = 'ContentionGrace.test_multiplier_is_exactly_one_without_contention'
        for args in (['verdict', '--list'], ['lifecycle', case],
                     ['lifecycle', '--jobs', '1', case], ['--jobs', '1', 'lifecycle', case]):
            result = subprocess.run([sys.executable, '-B', str(DIRECTORY / 'run_verifier_tests.py'), *args],
                                    capture_output=True, text=True, timeout=self.patience)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertNotIn('VerifierLifecycle.', result.stdout)
            if '--list' in args:
                listed = result.stdout.splitlines()
                self.assertTrue(listed)
                self.assertEqual(len(listed), len(set(listed)))
            else:
                jobs = 1 if '--jobs' in args else 2
                self.assertIn(f'selected=1 completed=1 failed=0 jobs={jobs}', result.stdout)
        for args in (['unknown'], ['verdict', '--jobs', '0'], ['verdict', '--jobs', '-1'],
                     ['verdict', '--jobs', 'nan'], ['verdict', 'Missing.test_case']):
            result = subprocess.run([sys.executable, '-B', str(DIRECTORY / 'run_verifier_tests.py'), *args],
                                    capture_output=True, text=True, timeout=self.patience)
            self.assertEqual(result.returncode, 2, result.stderr)


if __name__ == '__main__':
    unittest.main()
