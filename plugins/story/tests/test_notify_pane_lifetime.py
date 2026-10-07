"""Notification fixture identities survive controlled scheduling delays (SH-839)."""

import os
from pathlib import Path
import selectors
import subprocess
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
TESTS = Path(__file__).resolve().parent
sys.path.insert(0, str(TESTS.parent / 'lib'))
sys.path.insert(0, str(TESTS.parents[2] / 'scripts/tests'))
import agent_identity
import load_grace


# Model only sleep's clock. The pane is a real owned process, and the normal
# fake tmux and production identity reader observe its actual life and death.
CLOCKED_PANE = '''
import sys
deadline = float(sys.argv[1])
print('ready', flush=True)
for line in sys.stdin:
    if float(line) >= deadline:
        print('expired', flush=True)
        break
    print('live', flush=True)
'''


class NotifyPaneLifetimeTests(unittest.TestCase):
    """Accelerate the old 31-second failure without loading or sleeping the host."""

    def read_reply(self, child):
        """Bound fixture synchronization with existing load-aware test patience."""
        with selectors.DefaultSelector() as ready:
            ready.register(child.stdout, selectors.EVENT_READ)
            self.assertTrue(ready.select(load_grace.patience(5, load_grace.contention())), 'pane clock did not answer')
        answer = child.stdout.readline().strip()
        self.assertTrue(answer, 'pane clock exited without its answer')
        return answer

    def configured_lifetime(self, script, ratio, root):
        """Execute the real scenario setup, controlling only its external load sample."""
        assignments = [line for line in (TESTS / script).read_text().splitlines()
                       if line.startswith('FAKE_TMUX_PANE_LIFETIME=')]
        self.assertEqual(len(assignments), 1, script + ' must declare its scenario lifetime')
        binary = root / 'bin'
        binary.mkdir(exist_ok=True)
        launcher = binary / 'python3'
        launcher.write_text('''#!/usr/bin/env bash
exec "$FIXTURE_PYTHON" -c '
import os, runpy, sys
os.cpu_count = lambda: 10
if hasattr(os, "process_cpu_count"):
    os.process_cpu_count = lambda: 10
os.getloadavg = lambda: (float(os.environ["FIXTURE_LOAD"]) * 10, 0, 0)
sys.argv = sys.argv[1:]
runpy.run_path(sys.argv[0], run_name="__main__")
' "$@"
''')
        launcher.chmod(0o755)
        env = dict(os.environ, TESTS_DIR=str(TESTS), FIXTURE_PYTHON=sys.executable,
                   FIXTURE_LOAD=str(ratio), PATH=str(binary) + os.pathsep + os.environ['PATH'])
        result = subprocess.run(['bash', '-c', assignments[0] +
                                 '\nprintf "%s" "$FAKE_TMUX_PANE_LIFETIME"'],
                                env=env, capture_output=True, text=True, check=True,
                                timeout=load_grace.patience(5, load_grace.contention()))
        return float(result.stdout)

    def observe(self, root, child):
        """Use the same fake terminal row and production liveness check as notify."""
        child.poll()  # Reap an exited owned child before tmux's kill -0 probe.
        fields = {'pane_pid': str(child.pid), 'pane_id': '%1', 'window_name': 'TST-1',
                  'pane_cwd': str(root), 'pane_command': 'claude', 'pane_launch': 'claude'}
        for name, value in fields.items():
            (root / name).write_text(value)
        env = {key: value for key, value in os.environ.items()
               if not key.startswith('FAKE_TMUX_')}
        env['FAKE_TMUX_STATE'] = str(root)
        result = subprocess.run(['bash', str(TESTS / 'fakes/tmux'), 'display-message',
                                 '-p', agent_identity.FORMAT],
                                env=env,
                                capture_output=True, text=True, check=True,
                                timeout=load_grace.patience(5, load_grace.contention()))
        pane = agent_identity.parse_panes(result.stdout)['%1']
        return agent_identity.observe({'project': 'fixture', 'story': 'TST-1',
                                       'common': str(root)}, pane, 'claude')

    def scenario(self, root, lifetime, single_call, expect_expiry):
        """Keep one incarnation across the pause and all nine scenario observations."""
        with subprocess.Popen([sys.executable, '-u', '-c', CLOCKED_PANE, str(lifetime)],
                              stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                              stderr=subprocess.DEVNULL, text=True) as child:
            try:
                self.assertEqual(self.read_reply(child), 'ready')
                initial = self.observe(root, child)
                elapsed = single_call + 1  # The former lifetime ends before first use.
                for _ in range(9):
                    child.stdin.write(str(elapsed) + '\n')
                    child.stdin.flush()
                    answer = self.read_reply(child)
                    if expect_expiry:
                        self.assertEqual(answer, 'expired')
                        child.wait(timeout=load_grace.patience(5, load_grace.contention()))
                        with self.assertRaises(agent_identity.IdentityError) as failure:
                            self.observe(root, child)
                        self.assertEqual(failure.exception.reason, 'pane-dead')
                        return
                    self.assertEqual(answer, 'live', 'fixture expired before notification read')
                    self.assertEqual(self.observe(root, child), initial,
                                     'notification must retain the same pane/process incarnation')
                    elapsed += single_call - 1
                # The longer fixture remains finite; real exit must still be refused.
                child.stdin.write(str(lifetime) + '\n')
                child.stdin.flush()
                self.assertEqual(self.read_reply(child), 'expired')
                child.wait(timeout=load_grace.patience(5, load_grace.contention()))
                with self.assertRaises(agent_identity.IdentityError) as failure:
                    self.observe(root, child)
                self.assertEqual(failure.exception.reason, 'pane-dead')
            finally:
                if child.poll() is None:
                    child.terminate()
                child.wait(timeout=load_grace.patience(5, load_grace.contention()))

    def test_notification_panes_survive_forced_contention_without_replacement(self):
        """Both actual setups survive idle/busy stalls that kill the former fixture."""
        for script in ('test-notify-registered-session.sh', 'test-notify.sh'):
            for ratio in (0, 2):
                with self.subTest(script=script, contention=ratio), \
                        tempfile.TemporaryDirectory(prefix='story-notify-lifetime-', dir='/tmp') as scratch:
                    root = Path(scratch)
                    single = load_grace.patience(30, ratio)
                    # Controlled RED uses the old one-call lifetime, with identical reads.
                    self.scenario(root, single, single, expect_expiry=True)
                    self.scenario(root, self.configured_lifetime(script, ratio, root), single,
                                  expect_expiry=False)


if __name__ == '__main__':
    unittest.main()
