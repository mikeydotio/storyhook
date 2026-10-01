"""SH-676: classifier transport, isolation, and bounded process lifetime."""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'hooks'))
import codex_classifier as classifier

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / 'scripts/tests'))
import load_grace


def publish_pid(path, expression):
    """Publish a complete PID; file existence must never expose an empty write."""
    return (f'pathlib.Path({str(path.with_suffix(".tmp"))!r}).write_text(str({expression})); '
            f'os.replace({str(path.with_suffix(".tmp"))!r}, {str(path)!r}); ')


def wait_for_pid(path, process):
    """Wait for a real child to publish readiness, within the fixture allowance."""
    deadline = time.monotonic() + load_grace.patience(10, load_grace.contention())
    while not path.exists():
        if process.poll() is not None or time.monotonic() >= deadline:
            raise AssertionError(f'child did not publish its PID: exit={process.poll()}')
        time.sleep(0.02)
    return int(path.read_text())


def response(answer):
    """A provider-shaped completed text turn, with a controlled model answer."""
    return '\n'.join(json.dumps(x) for x in [
        {'type': 'thread.started'}, {'type': 'turn.started'},
        {'type': 'item.completed', 'item': {'type': 'agent_message', 'text': json.dumps(answer)}},
        {'type': 'turn.completed'},
    ])


class ClassifierTests(unittest.TestCase):
    """Only transport data is substituted; process execution is real."""

    def test_completed_text_turn_is_required(self):
        answer = {'decision': 'other', 'evidence': ''}
        self.assertEqual(classifier.parse_response(response(answer)), answer)
        for output in ['', '{}', response(answer).rsplit('\n', 1)[0],
                       response(answer) + '\n' + json.dumps({'type': 'error'}),
                       response(answer) + '\n' + json.dumps({'type': 'item.completed', 'item': {'type': 'command_execution'}}),
                       'not JSON', '[]', '{"type":"item.completed","item":[]}' ]:
            with self.subTest(output=output):
                with self.assertRaises((ValueError, RuntimeError)):
                    classifier.parse_response(output)

    def test_only_auth_discovery_and_transport_environment_survives(self):
        env = dict(HOME='/users/test', PATH='/bin', CODEX_HOME='/users/test/.codex',
                   STORYHOOK_AUTO='SH-1', STORYHOOK_FULL_AUTO='SH-1', STORYHOOK_PROJECT='other',
                   CODEX_THREAD_ID='parent', CODEX_SESSION_ID='parent',
                   PLUGIN_ROOT='/plugin', CLAUDE_PLUGIN_ROOT='/plugin',
                   PYTHONPATH='/untrusted', BASH_ENV='/untrusted', OPENAI_API_KEY='not-copied')
        self.assertEqual(classifier.classifier_environment(env),
                         {k: env[k] for k in ('HOME', 'PATH', 'CODEX_HOME')})

    def test_unverified_runtime_never_reaches_the_model(self):
        with patch.object(classifier, 'run_process', return_value='codex-cli 0.999.0') as run:
            with self.assertRaisesRegex(RuntimeError, 'unverified classifier runtime'):
                classifier.classify('Approve the plan')
        run.assert_called_once()
        self.assertEqual(run.call_args.args[0], ['codex', '--version'])

    def test_command_has_no_execution_or_project_policy_capabilities(self):
        command = classifier.classifier_command('/tmp/classifier')
        self.assertIn('--ignore-user-config', command)
        self.assertIn('--ephemeral', command)
        self.assertIn('--strict-config', command)
        self.assertEqual(command[command.index('-m') + 1], 'gpt-5.6-luna')
        for setting in ('agents.enabled=false', 'project_doc_max_bytes=0',
                        'approval_policy="never"', 'web_search="disabled"',
                        'skills.include_instructions=false'):
            self.assertIn(setting, command)
        model = json.loads((classifier.ROOT / 'luna-classifier.json').read_text())['models'][0]
        self.assertIsNone(model['apply_patch_tool_type'])
        self.assertEqual(model['shell_type'], 'disabled')
        self.assertEqual(model['tool_mode'], 'normal')
        self.assertNotIn('dangerously-bypass', ' '.join(command))

    def test_timeout_kills_descendants(self):
        with tempfile.TemporaryDirectory(dir='/tmp') as root:
            pidfile = Path(root) / 'pid'
            program = ('import os,subprocess,time,pathlib; '
                       "p=subprocess.Popen(['sleep','60']); "
                       + publish_pid(pidfile, 'p.pid') + 'time.sleep(60)')
            popen = subprocess.Popen

            def ready_process(*args, **kwargs):
                """Start the timeout only after the real descendant exists."""
                child = popen(*args, **kwargs)
                try:
                    wait_for_pid(pidfile, child)
                except BaseException:
                    os.killpg(child.pid, signal.SIGKILL)
                    child.communicate()
                    raise
                return child

            with patch.object(classifier.subprocess, 'Popen', side_effect=ready_process):
                with self.assertRaisesRegex(RuntimeError, 'deadline'):
                    classifier.run_process([sys.executable, '-c', program], timeout=0.3)
            pid = int(pidfile.read_text())
            # A killed descendant can briefly remain a zombie until init reaps it.
            state = subprocess.run(['ps', '-o', 'stat=', '-p', str(pid)], capture_output=True, text=True).stdout.strip()
            self.assertTrue(not state or state.startswith('Z'), state)

    def test_child_errors_carry_context(self):
        with self.assertRaisesRegex(RuntimeError, '42.*classifier refused'):
            classifier.run_process([sys.executable, '-c', "import sys;sys.stderr.write('classifier refused');sys.exit(42)"], timeout=2)

    def test_json_only_nonzero_output_retains_deadline_diagnostics(self):
        error = {'result': 'error', 'error': {'code': 'deadline_exceeded',
                 'message': 'Client deadline expired; daemon operation may still complete'}}
        program = f'import sys; print({json.dumps(error)!r}); sys.exit(12)'
        with self.assertRaisesRegex(RuntimeError, '12.*deadline_exceeded.*may still complete'):
            classifier.run_process([sys.executable, '-c', program], timeout=2)

    def test_nonzero_output_retains_both_bounded_streams(self):
        program = "import sys; print('x' * 2000 + 'stdout-tail'); sys.stderr.write('y' * 2000 + 'stderr-tail'); sys.exit(12)"
        with self.assertRaises(RuntimeError) as failure:
            classifier.run_process([sys.executable, '-c', program], timeout=2)
        self.assertIn('stdout-tail', str(failure.exception))
        self.assertIn('stderr-tail', str(failure.exception))
        self.assertLess(len(str(failure.exception)), 2200)

    def test_parent_termination_kills_owned_child_group(self):
        with tempfile.TemporaryDirectory(dir='/tmp') as root:
            pidfile = Path(root) / 'pid'
            child = ('import os,pathlib,time; ' + publish_pid(pidfile, 'os.getpid()')
                     + 'time.sleep(60)')
            parent = (f'import sys;sys.path.insert(0,{str(classifier.ROOT)!r}); '
                      f'from codex_classifier import run_process;run_process({[sys.executable, "-c", child]!r},timeout=30)')
            worker = subprocess.Popen([sys.executable, '-c', parent], stderr=subprocess.DEVNULL)
            pid = None
            try:
                pid = wait_for_pid(pidfile, worker)
                worker.terminate()
                worker.wait(timeout=load_grace.patience(5, load_grace.contention()))
                state = subprocess.run(['ps', '-o', 'stat=', '-p', str(pid)], capture_output=True, text=True).stdout.strip()
                self.assertTrue(not state or state.startswith('Z'), state)
            finally:
                if worker.poll() is None:
                    worker.kill()
                worker.wait()
                if pid:
                    try:
                        os.kill(pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass


if __name__ == '__main__':
    unittest.main()
