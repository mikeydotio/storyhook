"""Restoration publication changes physical bindings, never conversation authority."""
import copy
import contextlib
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch
sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'lib'))
import restoration


class PublicationTests(unittest.TestCase):
    """Crash recovery accepts only the proven source or exact derived replacement."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='restoration-', dir='/tmp')
        self.addCleanup(self.temp.cleanup)
        self.marker = Path(self.temp.name) / 'lease.json'
        self.plan = dict(lease_before={'tmux': {'socket_path': '/old'}},
                         lease={'tmux': {'socket_path': '/new'}}, marker=str(self.marker),
                         identity_before={'pane': '%1'}, identity={'pane': '%8', 'socket': '/new'},
                         metadata_before={'session_id': 'same', 'pane': '%1'},
                         metadata={'session_id': 'same', 'pane': '%8'}, pane='%8',
                         common='/common', story='SH-1')
        self.marker.write_text(json.dumps(self.plan['lease_before']))
        self.options = {restoration.IDENTITY: self.plan['identity_before'],
                        restoration.CONTINUATION: self.plan['metadata_before']}
        stack = contextlib.ExitStack()
        self.addCleanup(stack.close)
        self.enterContext = stack.enter_context
        self.effects = []
        def tmux(socket, *args):
            self.assertEqual(socket, '/new')
            self.assertEqual(args[0], 'set-option')
            self.options[args[-2]] = json.loads(args[-1])
            self.effects.append(args)
            return ''
        self.enterContext(patch.object(restoration, 'tmux', side_effect=tmux))
        self.enterContext(patch.object(restoration, 'read_option', side_effect=lambda s,p,k,**kw:self.options[k]))
        self.enterContext(patch.object(restoration, 'require_workspace'))
        self.enterContext(patch.object(restoration, 'propose', return_value=self.plan))

    def test_publication_and_repetition_preserve_conversation(self):
        for _ in range(2): restoration.publish({'lease': self.plan['lease_before']}, self.plan)
        self.assertEqual(json.loads(self.marker.read_text()), self.plan['lease'])
        self.assertEqual(self.options[restoration.CONTINUATION], self.plan['metadata'])
        self.assertEqual(len(self.effects), 2)

    def test_partial_publication_recovers_each_field_independently(self):
        self.options[restoration.IDENTITY] = self.plan['identity']
        restoration.publish({}, self.plan)
        self.assertEqual(len(self.effects), 1)
        self.assertEqual(json.loads(self.marker.read_text()), self.plan['lease'])

    def test_conflicting_metadata_refuses_before_any_write(self):
        for field in (restoration.IDENTITY, restoration.CONTINUATION):
            with self.subTest(field=field):
                original = self.options[field]
                self.options[field] = {'foreign': True}
                with self.assertRaisesRegex(RuntimeError, 'changed'): restoration.publish({}, self.plan)
                self.assertEqual(self.effects, [])
                self.assertEqual(json.loads(self.marker.read_text()), self.plan['lease_before'])
                self.options[field] = original

    def test_changed_process_or_generation_refuses(self):
        with patch.object(restoration, 'propose', return_value=dict(self.plan, pane='%9')):
            with self.assertRaisesRegex(RuntimeError, 'proof changed'): restoration.publish({}, self.plan)
        self.assertEqual(self.effects, [])

    def test_foreign_marker_refuses_without_effect(self):
        self.marker.write_text('{"foreign": true}')
        with self.assertRaisesRegex(RuntimeError, 'changed'): restoration.publish({}, self.plan)
        self.assertEqual(self.effects, [])


class ProviderAbsenceTests(unittest.TestCase):
    """Shell death cannot authorize another session while the resumed child lives."""

    def test_dead_replay_shell_preserves_its_surviving_provider(self):
        runtime = restoration.continuation
        provider = dict(pid=88, start='original', executable='/codex')
        capture = dict(socket='/socket', pane='%8', window='@9', pid=77,
                       lease=dict(story_id='SH-1'), restored=dict(provider_process=provider))
        with patch.object(runtime, 'panes', return_value=[['%8','@9','SH-1','77','1','zsh']]), \
             patch.object(runtime, 'process_start', return_value=None), \
             patch.object(runtime, 'process_identity', return_value=provider):
            with self.assertRaisesRegex(RuntimeError, 'live captured process'): runtime.owner(capture)
        with patch.object(runtime, 'panes', return_value=[['%8','@9','SH-1','77','1','zsh']]), \
             patch.object(runtime, 'process_start', return_value=None), \
             patch.object(runtime, 'process_identity', side_effect=ProcessLookupError):
            self.assertEqual(runtime.owner(capture), 'absent')


if __name__ == '__main__': unittest.main()
