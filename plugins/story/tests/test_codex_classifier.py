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


def install_codex_fixture(root, version, output, *, status=0, error=''):
    """Atomically replace a transport fixture and retain its actual invocations."""
    executable = root / 'codex'
    replacement = root / 'codex.new'
    replacement.write_text(
        f'#!{sys.executable}\n'
        'import json, os, pathlib, sys\n'
        f'with pathlib.Path({str(root / "calls.jsonl")!r}).open("a") as log:\n'
        '    log.write(json.dumps({"argv": sys.argv[1:], "cwd": os.getcwd(), '
        '"stdin": sys.stdin.read()}) + "\\n")\n'
        'if sys.argv[1:] == ["--version"]:\n'
        f'    print({version!r})\n'
        'else:\n'
        f'    sys.stdout.write({output!r})\n'
        f'    sys.stderr.write({error!r})\n'
        f'    sys.exit({status})\n')
    replacement.chmod(0o700)
    replacement.replace(executable)


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

    def test_runtime_replacement_between_calls_needs_no_version_approval(self):
        """An updated executable remains usable without a version subprocess."""
        with tempfile.TemporaryDirectory(dir='/tmp') as tmp:
            root = Path(tmp)
            with patch.dict(os.environ, {'PATH': tmp}):
                for version in ('codex-cli 0.154.0', 'codex-cli 0.159.3', 'codex-cli 0.999.0'):
                    with self.subTest(version=version):
                        answer = {'decision': 'approve_plan', 'evidence': version}
                        install_codex_fixture(root, version, response(answer))
                        self.assertEqual(classifier.classify(version), answer)
            calls = [json.loads(line) for line in (root / 'calls.jsonl').read_text().splitlines()]
            self.assertEqual(len(calls), 3)
            for call in calls:
                self.assertEqual(call['argv'][0], 'exec')
                workdir = call['argv'][call['argv'].index('-C') + 1]
                self.assertEqual(Path(workdir).resolve(), Path(call['cwd']).resolve())
                self.assertEqual(call['argv'], classifier.classifier_command(workdir)[1:])
                self.assertNotEqual(call['cwd'], os.getcwd())
                self.assertFalse(Path(call['cwd']).exists())
                self.assertIn(json.loads(call['stdin'])['assistant_message'],
                              ('codex-cli 0.154.0', 'codex-cli 0.159.3', 'codex-cli 0.999.0'))

    def test_runtime_contract_failures_are_not_retried_or_accepted(self):
        """Transport and protocol failures cannot trigger a weaker second call."""
        valid = response({'decision': 'approve_plan', 'evidence': 'Approve'})
        cases = [(valid, 42, 'unsupported configuration', RuntimeError),
                 ('not JSON', 0, '', ValueError),
                 (valid.rsplit('\n', 1)[0], 0, '', RuntimeError),
                 (valid + '\n' + json.dumps({'type': 'item.completed',
                  'item': {'type': 'command_execution'}}), 0, '', RuntimeError)]
        for output, status, error, exception in cases:
            with self.subTest(output=output, status=status), tempfile.TemporaryDirectory(dir='/tmp') as tmp:
                root = Path(tmp)
                install_codex_fixture(root, 'codex-cli 0.159.3', output, status=status, error=error)
                with patch.dict(os.environ, {'PATH': tmp}), self.assertRaises(exception) as failure:
                    classifier.classify('Approve')
                if error:
                    self.assertIn(error, str(failure.exception))
                calls = (root / 'calls.jsonl').read_text().splitlines()
                self.assertEqual(len(calls), 1)
                self.assertEqual(json.loads(calls[0])['argv'][0], 'exec')

    def test_missing_runtime_fails_loud(self):
        """A missing executable remains an explicit failure."""
        with tempfile.TemporaryDirectory(dir='/tmp') as tmp:
            with patch.dict(os.environ, {'PATH': tmp}), self.assertRaises(FileNotFoundError):
                classifier.classify('Approve')

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
