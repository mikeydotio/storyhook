"""Approval authority follows exact physical processes and current story policy."""
import copy
import json
from pathlib import Path
import sys
import tempfile
import threading
import unittest
from unittest.mock import patch, Mock
sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'lib'))
import approval_tmux as approval


class BindingTests(unittest.TestCase):
    """No reused PID or stale autonomous label can inherit a watcher's input."""

    def setUp(self):
        self.identity = dict(socket='/private/socket', pane='%8', process=dict(pid=77, start='kernel:1', executable='/claude'),
                             common='/repo/.git', story='SH-1', provider='claude')
        self.binding = {k:self.identity[k] for k in ('socket','pane','process','common','story')}
        self.metadata = dict(provider='claude', autonomy_mode='auto')

    def test_replaced_kernel_identity_stops_before_tmux(self):
        with patch.object(approval, 'process_identity', return_value=dict(self.binding['process'], start='kernel:2')), \
             patch.object(approval.tmux_client, 'client') as client:
            with self.assertRaisesRegex(RuntimeError, 'replaced'):
                approval.command(self.binding, ['send-keys', '-t', '%8', 'Enter'])
            client.assert_not_called()

    def test_reserved_or_closed_policy_stops_before_input(self):
        for state, labels in [('in-progress',['no-auto']), ('in-progress',['human-only']), ('verifying',[])]:
            with self.subTest(state=state, labels=labels), \
                 patch.object(approval, 'process_identity', return_value=self.binding['process']), \
                 patch.object(approval.agent_identity, 'run', return_value=json.dumps({'story':{'story':{'state':state,'labels':labels}}})), \
                 patch.object(approval.tmux_client, 'client') as client:
                with self.assertRaisesRegex(RuntimeError, 'policy'):
                    approval.command(self.binding, ['send-keys', '-t', '%8', 'Enter'])
                client.assert_not_called()

    def test_attended_or_missing_metadata_never_schedules(self):
        with patch.object(approval, 'command') as command:
            for metadata in [None, {}, dict(self.metadata, autonomy_mode='attended')]:
                self.assertFalse(approval.schedule(self.identity, metadata))
            command.assert_not_called()

    def test_completed_same_binding_deduplicates_both_providers(self):
        for provider in ('claude', 'codex'):
            with self.subTest(provider=provider), patch.object(approval, 'command', return_value=json.dumps(dict(binding=self.binding, complete=True))) as command:
                self.assertFalse(approval.schedule(dict(self.identity, provider=provider), dict(self.metadata, provider=provider)))
                self.assertEqual(command.call_count, 1)

    def test_rearm_passes_exact_endpoint_and_kernel_process(self):
        with patch.object(approval, 'command', side_effect=['','']) as command:
            self.assertTrue(approval.schedule(self.identity, self.metadata))
            binding, args = command.call_args.args
            self.assertEqual(binding, self.binding)
            self.assertEqual(args[:4], ['run-shell','-b','-t','%8'])
            self.assertIn('STORYHOOK_AUTO=SH-1', args[4])
            self.assertIn('kernel:1', args[4])

    def test_unconfirmed_watcher_exit_does_not_mark_approval_complete(self):
        with tempfile.TemporaryDirectory(prefix='sh825-watch-', dir='/tmp') as directory:
            with patch.object(approval, 'command', return_value='') as command, \
                 patch.object(approval.subprocess, 'run', return_value=Mock(returncode=0)):
                approval.watch(dict(self.binding, common=directory), '/hook', 'codex', '1')
                self.assertEqual(command.call_count, 1)
                self.assertEqual(command.call_args.args[1][0], 'show-options')

    def test_concurrent_reconciliation_keeps_one_live_watcher(self):
        with tempfile.TemporaryDirectory(prefix='sh825-watch-', dir='/tmp') as directory:
            binding = dict(self.binding, common=directory)
            entered, release = threading.Event(), threading.Event()
            errors = []
            def run(*args, **kwargs):
                entered.set()
                if not release.wait(10): raise RuntimeError('fixture watcher not released')
                return Mock(returncode=0)
            def first():
                try: approval.watch(binding, '/hook', 'codex', '1')
                except BaseException as error: errors.append(error)
            with patch.object(approval, 'command', return_value=''), patch.object(approval.subprocess, 'run', side_effect=run) as child:
                thread = threading.Thread(target=first)
                thread.start()
                try:
                    self.assertTrue(entered.wait(10))
                    approval.watch(binding, '/hook', 'codex', '1')
                    self.assertEqual(child.call_count, 1)
                finally:
                    release.set()
                    thread.join(10)
                self.assertFalse(thread.is_alive())
                self.assertEqual(errors, [])


if __name__ == '__main__': unittest.main()
