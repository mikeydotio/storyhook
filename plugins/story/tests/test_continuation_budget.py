"""Continuation deadlines cover nested probes and dispatch without wall-clock sleeps."""

import io
import json
import os
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'lib'))
import continuation_runtime as runtime
import probe_budget


class ContinuationBudgetTests(unittest.TestCase):
    """Drive production operation nesting against a controlled process boundary."""

    def setUp(self):
        """Keep time, subprocess results and observation fixtures local to each case."""
        self.now = 0.0
        self.calls = []
        self.costs = {}
        self.dispatched = False
        self.lease = {'worktree_path': '/tmp', 'project_slug': 'fixture', 'story_id': 'CT-1'}
        self.capture = {
            'lease': self.lease, 'socket': '/tmp/fixture.sock', 'pane': '%1',
            'head': 'head', 'fingerprint': 'fingerprint', 'session_id': 'old',
            'provider': 'codex', 'autonomy_mode': 'auto', 'model': 'fixture',
            'effort': '', 'speed': 'standard',
        }
        self.value = {'capture': self.capture, 'story_id': 'CT-1', 'id': 'request-1',
                      'origin': {'cwd': '/tmp'}, 'handoff': {'story_id': 'CT-1'},
                      'cwd': '/tmp'}
        self.patch(probe_budget.time, 'monotonic', side_effect=lambda: self.now)
        self.patch(subprocess, 'run', side_effect=self.run_process)
        self.patch(probe_budget.os, 'getloadavg', return_value=(12.5, 0, 0))
        self.patch(probe_budget.os, 'cpu_count', return_value=10)

    def patch(self, target, name, **kwargs):
        """Restore each external boundary even when the assertion fails."""
        patcher = patch.object(target, name, **kwargs)
        result = patcher.start()
        self.addCleanup(patcher.stop)
        return result

    def run_process(self, argv, **kwargs):
        """Model process latency and output while honoring the supplied timeout."""
        kind = 'dispatch' if argv[0] == 'bash' else argv[0]
        allowance = kwargs['timeout']
        self.calls.append((kind, allowance))
        cost = self.costs.get(kind, 1)
        if cost > allowance:
            self.now += allowance
            raise subprocess.TimeoutExpired(argv, allowance)
        self.now += cost
        if kind == 'dispatch':
            self.dispatched = True
            output = json.dumps({'ok': True, 'window_reused': True,
                                 'worktree_reused': True, 'branch_reused': True})
        elif kind == 'metadata':
            output = json.dumps({'session_id': 'new'})
        elif kind == 'fingerprint':
            output = 'fingerprint'
        else:
            output = 'head'
        kwargs['stdout'].write(output.encode())
        return subprocess.CompletedProcess(argv, 0)

    def arrange_resume(self):
        """Supply owned observations, leaving resume, preflight and observe unmocked."""
        def lease(*_args):
            runtime.command(['lease'])
            return self.lease

        def owner(_capture):
            runtime.command(['ps'])
            return 'present' if self.dispatched else 'absent'

        def panes(_socket):
            runtime.command(['panes'])
            return [['%1', '@1', 'CT-1', '42', '1', 'codex']]

        self.patch(runtime, 'cleanup_lease', side_effect=lease)
        self.patch(runtime, 'owner', side_effect=owner)
        self.patch(runtime, 'panes', side_effect=panes)
        self.patch(runtime, 'fingerprint', side_effect=lambda _cwd:
                   runtime.command(['fingerprint']).decode())
        self.patch(runtime, 'tmux', side_effect=lambda *_args:
                   runtime.command(['metadata']).decode())

    def json_resume(self):
        """Exercise the production JSON refusal handler, including timeout details."""
        with patch.object(sys, 'argv', ['continuation_runtime.py', 'resume']), \
                patch.object(sys, 'stdin', io.StringIO(json.dumps(self.value))), \
                patch.object(sys, 'stdout', io.StringIO()) as output:
            runtime.main()
        return json.loads(output.getvalue())

    def test_every_entry_point_owns_one_decreasing_budget(self):
        """Direct callers and nested preflight receive the same deadline contract."""
        def probe_then_stop(*_args):
            runtime.command(['git'])
            runtime.command(['tmux'])
            raise RuntimeError('fixture boundary reached')

        self.patch(runtime, 'cleanup_lease', side_effect=probe_then_stop)
        self.costs = {'git': 5, 'tmux': 5}
        for operation, budget in [(runtime.capture_request, 30), (runtime.observe, 30),
                                  (runtime.register, 30), (runtime.resume_preflight, 30),
                                  (runtime.resume, 150)]:
            with self.subTest(operation=operation.__name__):
                self.calls.clear()
                with self.assertRaisesRegex(RuntimeError, 'fixture boundary reached'):
                    operation(self.value)
                self.assertEqual(self.calls, [('git', budget), ('tmux', budget - 5)])
                self.assertEqual(probe_budget.remaining(), probe_budget.BUDGET_SECONDS)

    def test_nested_operation_cannot_extend_a_callers_deadline(self):
        """Resume also respects a tighter deadline supplied by its immediate caller."""
        self.arrange_resume()
        with probe_budget.operation(budget=10):
            self.now += 2
            with self.assertRaises(probe_budget.ProbeTimeout):
                self.costs['dispatch'] = 120
                runtime.resume(self.value)
        self.assertEqual(self.calls[0], ('lease', 8))
        self.assertEqual(self.calls[-1], ('dispatch', 3))

    def test_resume_shares_time_with_dispatch_and_final_ownership_checks(self):
        """A full 120-second dispatch fits with slow probes in one 150-second operation."""
        self.arrange_resume()
        self.costs = {'lease': 5, 'ps': 5, 'panes': 5, 'git': 5,
                      'fingerprint': 5, 'dispatch': 120, 'metadata': 1}
        # The final process observation has four seconds left after metadata.
        original = self.run_process

        def boundary(argv, **kwargs):
            if self.dispatched and argv[0] == 'ps':
                self.costs['ps'] = 1
            return original(argv, **kwargs)

        self.patch(subprocess, 'run', side_effect=boundary)
        answer = self.json_resume()
        self.assertTrue(answer['ok'], answer)
        self.assertEqual(answer['phase'], 'submitted')
        self.assertEqual(self.calls, [('lease', 150), ('ps', 145), ('panes', 140),
                                     ('git', 135), ('fingerprint', 130),
                                     ('dispatch', 125), ('metadata', 5), ('ps', 4)])

    def test_exhausted_preflight_never_dispatches(self):
        """Time spent reading final Git evidence cannot renew the dispatch allowance."""
        self.arrange_resume()
        self.costs['fingerprint'] = 146
        answer = self.json_resume()
        self.assertFalse(answer['ok'])
        self.assertNotIn('dispatch', [kind for kind, _ in self.calls])
        self.assertIn('0.0s allowance', answer['detail'])
        self.assertIn('150s operation budget', answer['detail'])

    def test_timeout_during_dispatch_or_final_check_returns_diagnostic_refusal(self):
        """No uncertain dispatch or ownership timeout becomes success or a retry."""
        self.arrange_resume()
        for dispatch_cost, final_cost, failed_command in [(146, 1, 'bash'),
                                                         (144, 2, 'metadata')]:
            with self.subTest(stage=failed_command):
                self.now = 0
                self.calls.clear()
                self.dispatched = False
                self.costs = {'dispatch': dispatch_cost, 'metadata': final_cost}
                answer = self.json_resume()
                self.assertFalse(answer['ok'])
                detail = answer['detail']
                for text in (failed_command, 'allowance', '150.0s',
                             '150s operation budget', '12.50 on 10 cores'):
                    self.assertIn(text, detail)
                self.assertEqual([kind for kind, _ in self.calls].count('dispatch'), 1)

    def test_exhausted_budget_never_starts_another_probe(self):
        """Expiry at exactly zero refuses before the next subprocess is spawned."""
        with probe_budget.operation():
            self.now += probe_budget.BUDGET_SECONDS
            with self.assertRaises(probe_budget.ProbeTimeout):
                runtime.command(['ps'])
        self.assertEqual(self.calls, [])

    def test_command_preserves_file_output_context_and_owned_descriptors(self):
        """Budgeting must retain the command transport and workspace authority."""
        def process(argv, **kwargs):
            self.assertEqual(argv, ['git', 'fixture'])
            self.assertEqual(kwargs['cwd'], '/tmp')
            self.assertEqual(kwargs['env'], {'FIXTURE': 'value'})
            self.assertEqual(kwargs['pass_fds'], (42,))
            self.assertFalse(kwargs['check'])
            self.assertGreaterEqual(kwargs['stdout'].fileno(), 0)
            self.assertGreaterEqual(kwargs['stderr'].fileno(), 0)
            kwargs['stderr'].write(b'fixture diagnostic')
            return subprocess.CompletedProcess(argv, 1)

        self.patch(runtime, 'inherited_fds', return_value=(42,))
        self.patch(subprocess, 'run', side_effect=process)
        with self.assertRaisesRegex(RuntimeError, 'git.*fixture.*fixture diagnostic'):
            runtime.command(['git', 'fixture'], cwd='/tmp', env={'FIXTURE': 'value'})

    def test_process_start_survives_slow_ps_within_shared_budget(self):
        """A live PID's incarnation probe may take longer than the former two seconds."""
        pid = os.getpid()
        self.costs['ps'] = 5
        with probe_budget.operation():
            self.assertEqual(runtime.process_start(pid), 'head')
            self.assertEqual(probe_budget.remaining(), probe_budget.BUDGET_SECONDS - 5)
        self.assertEqual(self.calls, [('ps', probe_budget.BUDGET_SECONDS)])
        subprocess.run.assert_called_once()
        self.assertEqual(subprocess.run.call_args.args[0],
                         ['ps', '-p', str(pid), '-o', 'lstart='])

    def test_process_start_reports_exhausted_shared_budget(self):
        """The same real process_start path must refuse when ps exceeds time left."""
        pid = os.getpid()
        self.costs['ps'] = 5
        with probe_budget.operation():
            self.now += probe_budget.BUDGET_SECONDS - 3
            with self.assertRaises(probe_budget.ProbeTimeout) as failure:
                runtime.process_start(pid)
            self.assertEqual(probe_budget.remaining(), 0)
        self.assertEqual(self.calls, [('ps', 3)])
        detail = str(failure.exception)
        for expected in (f'ps -p {pid} -o lstart=', '3.0s allowance',
                         '30.0s of a 30s operation budget', '12.50 on 10 cores'):
            self.assertIn(expected, detail)


if __name__ == '__main__':
    unittest.main()
