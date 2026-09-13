"""Continuation observes session generations without confusing compaction turns."""
import importlib.util
import json
from pathlib import Path
import tempfile
import os
import shutil
import subprocess
import time
import unittest

spec = importlib.util.spec_from_file_location('continuation_runtime', Path(__file__).resolve().parents[1] / 'lib' / 'continuation_runtime.py')
runtime = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runtime)


class NativeBoundaryTests(unittest.TestCase):
    """Drive real transcript interpretation with provider event data."""

    def test_manual_compaction_does_not_require_new_turn_context(self):
        """Codex 0.154 writes task lifecycle but no turn_context for compaction."""
        with tempfile.TemporaryDirectory(dir='/tmp') as directory:
            path = Path(directory) / 'rollout.jsonl'
            events = [
                {'type': 'session_meta', 'payload': {'id': 's1', 'cwd': directory, 'source': 'cli'}},
                {'type': 'turn_context', 'payload': {'turn_id': 'old', 'cwd': directory, 'collaboration_mode': {'mode': 'plan'}}},
                {'type': 'event_msg', 'payload': {'type': 'task_started', 'turn_id': 'compact'}},
                {'type': 'compacted', 'payload': {'message': 'summary'}},
                {'type': 'event_msg', 'payload': {'type': 'task_complete', 'turn_id': 'compact'}},
            ]
            path.write_text(''.join(json.dumps(event) + '\n' for event in events))
            capture = {'provider': 'codex', 'session_id': 's1', 'transcript_path': str(path), 'lease': {'worktree_path': directory}}
            self.assertEqual(runtime.native_state(capture), 'idle')
            with path.open('a') as out:
                out.write(json.dumps({'type': 'event_msg', 'payload': {'type': 'task_started', 'turn_id': 'correction'}}) + '\n')
            self.assertEqual(runtime.native_state(capture), 'busy')

    def test_unknown_or_foreign_history_never_proves_idle(self):
        """A partial or replaced transcript supplies no ownership evidence."""
        with tempfile.TemporaryDirectory(dir='/tmp') as directory:
            path = Path(directory) / 'rollout.jsonl'
            path.write_text(json.dumps({'type': 'session_meta', 'payload': {'id': 'foreign', 'cwd': directory, 'source': 'cli'}}) + '\n')
            capture = {'provider': 'codex', 'session_id': 's1', 'transcript_path': str(path), 'lease': {'worktree_path': directory}}
            with self.assertRaises(RuntimeError):
                runtime.native_state(capture)

    def test_long_history_retains_header_and_current_boundary(self):
        """History size alone must not disable a context-exhaustion handoff."""
        with tempfile.TemporaryDirectory(dir='/tmp') as directory:
            path = Path(directory) / 'long.jsonl'
            rows = [
                {'type': 'session_meta', 'payload': {'id': 's1', 'cwd': directory, 'source': 'cli'}},
                {'type': 'response_item', 'payload': {'text': 'old' * 2000}},
                {'type': 'event_msg', 'payload': {'type': 'task_started', 'turn_id': 'current'}},
                {'type': 'event_msg', 'payload': {'type': 'task_complete', 'turn_id': 'current'}},
            ]
            path.write_text(''.join(json.dumps(row) + '\n' for row in rows))
            capture = {'provider': 'codex', 'session_id': 's1', 'transcript_path': str(path),
                       'lease': {'worktree_path': directory}}
            previous = runtime.MAX_BYTES
            try:
                runtime.MAX_BYTES = 1024
                self.assertEqual(runtime.native_state(capture), 'idle')
            finally:
                runtime.MAX_BYTES = previous


@unittest.skipUnless(shutil.which('tmux'), 'tmux is required for owned process fixtures')
class OwnedRuntimeTests(unittest.TestCase):
    """Exercise real Git, tmux, filesystem and process observations on private resources."""

    def setUp(self):
        """Create only test-owned resources and bind their native fixture session."""
        self.temp = tempfile.TemporaryDirectory(prefix='sh711-runtime-', dir='/tmp')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.repo = self.root / 'repo'
        self.repo.mkdir()
        self.run_command('git', 'init', '-q', str(self.repo))
        self.run_command('git', '-C', str(self.repo), '-c', 'user.name=Fixture',
                         '-c', 'user.email=fixture@example.test', 'commit', '--allow-empty', '-qm', 'fixture')
        self.cwd = self.root / 'CT-1'
        self.run_command('git', '-C', str(self.repo), 'worktree', 'add', '-qb', 'worktree-CT-1', str(self.cwd))
        self.socket = str(self.root / 'tmux.sock')
        # A local inert executable supplies process identity without copying an
        # Apple platform binary whose arm64e signature is path-sensitive.
        executable = self.root / 'codex'
        source = self.root / 'inert.c'
        source.write_text('#include <unistd.h>\nint main(void) { sleep(90); return 0; }\n')
        self.run_command('cc', str(source), '-o', str(executable))
        self.run_command('tmux', '-f', '/dev/null', '-S', self.socket, 'new-session', '-d', '-s', 'fixture', '-n', 'keep', 'sleep 90')
        self.addCleanup(lambda: subprocess.run(['tmux', '-S', self.socket, 'kill-server'],
                                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False))
        self.pane = self.run_command('tmux', '-S', self.socket, 'new-window', '-d', '-t', 'fixture:',
                                     '-n', 'CT-1', '-P', '-F', '#{pane_id}', str(executable) + ' 90').strip()
        self.run_command('tmux', '-S', self.socket, 'set-window-option', '-t', self.pane, 'remain-on-exit', 'on')
        self.run_command('tmux', '-S', self.socket, 'set-window-option', '-t', self.pane, 'automatic-rename', 'off')
        self.run_command('tmux', '-S', self.socket, 'rename-window', '-t', self.pane, 'CT-1')
        self.lease = {'version': 1, 'project_slug': 'fixture', 'story_id': 'CT-1',
                      'repository_path': str(self.repo.resolve()), 'worktree_path': str(self.cwd.resolve()),
                      'branch': 'worktree-CT-1', 'tmux': {'socket_path': self.socket}}
        private = Path(self.run_command('git', '-C', str(self.cwd), 'rev-parse', '--absolute-git-dir').strip())
        (private / runtime.MARKER).write_text(json.dumps(self.lease))
        self.rollout = self.root / 'rollout.jsonl'
        events = [
            {'type': 'session_meta', 'payload': {'id': 'native-s1', 'cwd': str(self.cwd), 'source': 'cli'}},
            {'type': 'event_msg', 'payload': {'type': 'task_started', 'turn_id': 'native-t1'}},
            {'type': 'event_msg', 'payload': {'type': 'task_complete', 'turn_id': 'native-t1'}},
        ]
        self.rollout.write_text(''.join(json.dumps(row) + '\n' for row in events))
        (self.cwd / '.claude').mkdir()
        (self.cwd / '.claude/dispatch-sentinel.json').write_text(json.dumps({
            'protocol_version': 2, 'story_id': 'CT-1', 'session_id': 'native-s1',
            'transcript_path': str(self.rollout)}))
        for _ in range(100):
            rows = runtime.panes(self.socket)
            if any(row[0] == self.pane and 'codex' in row[5] for row in rows):
                break
            time.sleep(.01)
        self.settings = {'story_id': 'CT-1', 'cwd': str(self.cwd), 'socket': self.socket,
                         'pane': self.pane, 'provider': 'codex', 'model': 'fixture',
                         'effort': 'high', 'speed': 'standard', 'autonomy_mode': 'auto'}
        runtime.register(self.settings)
        self.request = {'provider': 'codex', 'handoff': {'story_id': 'CT-1'},
                        'origin': {'cwd': str(self.cwd), 'tmux': self.socket + ',1,0',
                                   'tmux_pane': self.pane, 'session_id': 'native-s1',
                                   'turn_id': 'native-t1', 'collaboration_mode': 'default',
                                   'transcript_path': str(self.rollout)}}
        self.capture = runtime.capture_request(self.request)['capture']
        self.record = {'story_id': 'CT-1', 'id': 'request-1', 'capture': self.capture}

    def run_command(self, *argv):
        """Run an owned fixture command and retain its diagnostic on failure."""
        result = subprocess.run(argv, check=False, text=True, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, timeout=5)
        self.assertEqual(result.returncode, 0, repr(argv) + ': ' + result.stderr)
        return result.stdout

    def stop_provider(self):
        """Terminate only the exact inert process created by this test."""
        os.kill(self.capture['pid'], 15)
        for _ in range(100):
            if runtime.owner(self.capture) == 'absent':
                return
            time.sleep(.01)
        self.fail('owned provider did not exit')

    def test_dirty_bytes_and_native_identity_survive_observation(self):
        """Repeated capture is read-only and rejects a foreign root session."""
        path = self.cwd / 'untracked.txt'
        path.write_bytes(b'keep\x00dirty\n')
        first = runtime.capture_request(self.request)['capture']
        self.assertNotEqual(first['fingerprint'], self.capture['fingerprint'])
        self.assertEqual(first, runtime.capture_request(self.request)['capture'])
        self.assertEqual(runtime.observe(self.record)['phase'], 'idle')
        self.assertEqual(path.read_bytes(), b'keep\x00dirty\n')
        self.request['origin']['session_id'] = 'foreign'
        with self.assertRaisesRegex(RuntimeError, 'differs from dispatcher'):
            runtime.capture_request(self.request)

    def test_live_provider_and_replacement_are_never_resumed(self):
        """Recovery cannot reinterpret a live or reused PID as an absent session."""
        with self.assertRaisesRegex(RuntimeError, 'proven absence'):
            runtime.resume_preflight(self.record)
        changed = self.capture | {'started': 'some other incarnation'}
        with self.assertRaisesRegex(RuntimeError, 'incarnation changed'):
            runtime.owner(changed)
        changed = self.capture | {'pid': os.getpid()}
        with self.assertRaisesRegex(RuntimeError, 'ownership changed'):
            runtime.owner(changed)
        # This is the atomic guard the production shell selects. Even when a
        # previous preflight was stale, tmux refuses and leaves the PID intact.
        refused = subprocess.run(['tmux', '-S', self.socket, 'respawn-pane',
                                  '-t', self.pane, '/usr/bin/false'],
                                 capture_output=True, check=False, timeout=5)
        self.assertNotEqual(refused.returncode, 0)
        self.assertEqual(runtime.owner(self.capture), 'present')

    def test_dead_pane_is_recoverable_but_missing_or_dirty_changed_is_not(self):
        """Absence is necessary but never sufficient to reconstruct resources."""
        self.stop_provider()
        self.assertEqual(runtime.resume_preflight(self.record)['phase'], 'absent')
        path = self.cwd / 'new-correction'
        path.write_text('preserve this later correction')
        with self.assertRaisesRegex(RuntimeError, 'Git evidence changed'):
            runtime.resume_preflight(self.record)
        path.unlink()
        self.run_command('tmux', '-S', self.socket, 'kill-window', '-t', self.pane)
        self.assertEqual(runtime.owner(self.capture), 'absent')
        with self.assertRaisesRegex(RuntimeError, 'missing pane'):
            runtime.resume_preflight(self.record)

    def test_duplicate_names_and_missing_server_fail_loudly(self):
        """Unknown topology cannot authorize replacement or input delivery."""
        self.run_command('tmux', '-S', self.socket, 'new-window', '-d', '-t', 'fixture:', '-n', 'CT-1', 'sleep 90')
        with self.assertRaisesRegex(RuntimeError, 'ambiguous'):
            runtime.owner(self.capture)
        changed = self.capture | {'socket': str(self.root / 'missing.sock')}
        with self.assertRaisesRegex(RuntimeError, 'command failed'):
            runtime.owner(changed)

    def test_registration_precedes_the_first_native_task(self):
        """Startup identity cannot depend on a task the dispatcher has not submitted."""
        self.rollout.write_text('')
        self.assertTrue(runtime.register(self.settings)['ok'])
        with self.assertRaisesRegex(RuntimeError, 'incomplete'):
            runtime.capture_request(self.request)


if __name__ == '__main__':
    unittest.main()
